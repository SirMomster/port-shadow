mod cli;
mod config;
mod discovery;
mod forward;

use anyhow::Context;
use clap::Parser;
use std::collections::HashSet;
use std::time::Duration;
use tracing_subscriber::{fmt, EnvFilter};

use cli::Cli;
use config::Config;
use forward::{resolve_local_port, ForwardManager};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ------------------------------------------------------------------
    // Logging setup
    // ------------------------------------------------------------------
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("port_shadow={level}")));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .init();

    // ------------------------------------------------------------------
    // Load config
    // ------------------------------------------------------------------
    let config_path = cli.config.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cannot determine current directory")
            .into()
    });

    // If a specific file was given, use its parent dir; otherwise use the path directly.
    let config_dir = if config_path.is_file() {
        config_path
            .parent()
            .unwrap_or(&config_path)
            .to_path_buf()
    } else {
        config_path.clone()
    };

    let mut cfg = Config::load_from_dir(&config_dir)?;

    // CLI overrides
    if let Some(h) = cli.host {
        cfg.ssh.host = Some(h);
    }
    if let Some(cp) = cli.control_path {
        cfg.ssh.control_path = Some(cp);
    }
    if let Some(i) = cli.poll_interval {
        cfg.ssh.poll_interval_secs = i;
    }
    if let Some(p) = cli.ssh_port {
        cfg.ssh.port = p;
    }

    // Validate required fields
    let host = cfg
        .ssh
        .host
        .clone()
        .context("SSH host is required. Provide --host or set [ssh] host in .port-shadow.toml")?;

    if cfg.ports.is_empty() {
        tracing::warn!("no ports configured in .port-shadow.toml — nothing to forward");
        tracing::warn!("add [[ports]] entries to .port-shadow.toml to enable forwarding");
    }

    let control_path = cfg.ssh.control_path.clone();
    let ssh_port = cfg.ssh.port;
    let poll_interval = Duration::from_secs(cfg.ssh.poll_interval_secs);
    let excluded = cfg.excluded_set();
    let extra_args = cfg.ssh.extra_args.clone();

    tracing::info!(
        host,
        ssh_port,
        poll_interval_secs = cfg.ssh.poll_interval_secs,
        control_path = control_path.as_deref().unwrap_or("(none — Windows mode)"),
        port_entries = cfg.ports.len(),
        "port-shadow starting"
    );

    #[cfg(windows)]
    if control_path.is_some() {
        tracing::warn!(
            "ControlPath is not supported on Windows. Each forward will use a separate SSH connection."
        );
    }

    // ------------------------------------------------------------------
    // Main polling loop
    // ------------------------------------------------------------------
    let mut manager = ForwardManager::new();

    // Shutdown signal
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let mut interval = tokio::time::interval(poll_interval);
    // Don't try to catch up on missed ticks
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Reap any ssh -L processes that have died
                manager.reap_dead_forwards().await;

                // Discover currently-listening remote ports
                let remote_listening = match discovery::discover_listening_ports(
                    &host,
                    ssh_port,
                    control_path.as_deref(),
                    &extra_args,
                )
                .await
                {
                    Ok(ports) => ports,
                    Err(e) => {
                        tracing::error!(error = %e, "port discovery failed");
                        continue;
                    }
                };

                tracing::debug!(
                    count = remote_listening.len(),
                    "discovered remote listening ports"
                );

                // Build set of configured remote ports from config
                let configured_ports: HashSet<u16> = cfg
                    .ports
                    .iter()
                    .map(|m| m.remote_port)
                    .filter(|p| !excluded.contains(p))
                    .collect();

                // Start forwards for configured ports that are now listening
                // and not yet forwarded
                for mapping in &cfg.ports {
                    let rport = mapping.remote_port;

                    if excluded.contains(&rport) {
                        continue;
                    }
                    if !remote_listening.contains(&rport) {
                        // Port not yet listening on remote — skip
                        continue;
                    }
                    if manager.is_active(rport) {
                        // Already forwarded
                        continue;
                    }

                    // Determine local port
                    let preferred = mapping.local_port.unwrap_or(rport);
                    let local_port = match resolve_local_port(preferred).await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(
                                remote_port = rport,
                                error = %e,
                                "failed to resolve local port"
                            );
                            continue;
                        }
                    };

                    if let Err(e) = manager
                        .start_forward(
                            &host,
                            ssh_port,
                            control_path.as_deref(),
                            &extra_args,
                            rport,
                            local_port,
                            mapping.label.clone(),
                        )
                        .await
                    {
                        tracing::error!(
                            remote_port = rport,
                            error = %e,
                            "failed to start forward"
                        );
                    }
                }

                // Tear down forwards for ports that are:
                //   a) no longer listening on remote, OR
                //   b) removed from config (not in configured_ports)
                let active_ports: Vec<u16> = manager.active_remote_ports().collect();
                for rport in active_ports {
                    let still_listening = remote_listening.contains(&rport);
                    let still_configured = configured_ports.contains(&rport);

                    if !still_listening {
                        tracing::info!(
                            remote_port = rport,
                            "remote port is no longer listening — tearing down"
                        );
                        manager.stop_forward(rport).await;
                    } else if !still_configured {
                        tracing::info!(
                            remote_port = rport,
                            "port removed from config — tearing down"
                        );
                        manager.stop_forward(rport).await;
                    }
                }
            }

            _ = &mut shutdown => {
                tracing::info!("received shutdown signal — tearing down all forwards");
                manager.teardown_all().await;
                tracing::info!("all forwards stopped, exiting");
                break;
            }
        }
    }

    Ok(())
}
