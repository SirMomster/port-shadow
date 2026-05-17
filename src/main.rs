mod cli;
mod config;
mod discovery;
mod events;
mod forward;
mod tui;

use anyhow::Context;
use clap::Parser;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing_subscriber::{fmt, EnvFilter};

use cli::Cli;
use config::Config;
use events::{AppEvent, LogLevel};
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

    if cli.tui {
        // Write tracing output to a log file so it doesn't collide with the TUI.
        // Use RUST_LOG=port_shadow=debug to control level.
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("port-shadow.log")
            .ok();

        if let Some(file) = log_file {
            fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_writer(file)
                .init();
        }
    } else {
        fmt().with_env_filter(filter).with_target(false).init();
    }

    // ------------------------------------------------------------------
    // Load config
    // ------------------------------------------------------------------
    let config_path = cli.config.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cannot determine current directory")
            .into()
    });

    let config_dir = if config_path.is_file() {
        config_path.parent().unwrap_or(&config_path).to_path_buf()
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
    let host =
        cfg.ssh.host.clone().context(
            "SSH host is required. Provide --host or set [ssh] host in .port-shadow.toml",
        )?;

    let control_path = cfg.ssh.control_path.clone();
    let ssh_port = cfg.ssh.port;
    let poll_interval = Duration::from_secs(cfg.ssh.poll_interval_secs);
    let excluded = cfg.excluded_set();
    let extra_args = cfg.ssh.extra_args.clone();

    // ------------------------------------------------------------------
    // Channel connecting polling loop → TUI / log consumer
    // ------------------------------------------------------------------
    let (tx, rx) = mpsc::unbounded_channel::<AppEvent>();

    if cfg.ports.is_empty() {
        let _ = tx.send(AppEvent::Log {
            level: LogLevel::Info,
            message: "no [[ports]] in config — auto-discovering new ports on the remote".into(),
        });
    }

    #[cfg(windows)]
    if control_path.is_some() {
        let _ = tx.send(AppEvent::Log {
            level: LogLevel::Warn,
            message: "ControlPath is not supported on Windows; using fresh SSH connections".into(),
        });
    }

    // ------------------------------------------------------------------
    // Spawn the polling loop as a background task
    // ------------------------------------------------------------------
    let poll_tx = tx.clone();
    let poll_host = host.clone();
    let poll_cfg = cfg.clone();
    let poll_control_path = control_path.clone();

    tokio::spawn(async move {
        run_poll_loop(
            poll_host,
            ssh_port,
            poll_control_path,
            extra_args,
            poll_interval,
            excluded,
            poll_cfg,
            poll_tx,
        )
        .await;
    });

    // ------------------------------------------------------------------
    // Run the TUI or plain log consumer in the main task
    // ------------------------------------------------------------------
    if cli.tui {
        tui::run_tui(rx, host).await?;
    } else {
        run_log_consumer(rx).await;
    }

    // Signal the polling loop to stop by dropping the sender;
    // the spawned task will exit when it tries to send on a closed channel.
    drop(tx);

    Ok(())
}

/// Consume AppEvents and print them as plain log lines (no TUI).
async fn run_log_consumer(mut rx: mpsc::UnboundedReceiver<AppEvent>) {
    use tracing::{error, info, warn};
    while let Some(event) = rx.recv().await {
        match event {
            AppEvent::Log { level, message } => match level {
                LogLevel::Error => error!("{}", message),
                LogLevel::Warn => warn!("{}", message),
                LogLevel::Info => info!("{}", message),
            },
            AppEvent::PollOk { discovered } => {
                info!("poll ok: {} port(s) discovered", discovered);
            }
            AppEvent::PollError { message } => {
                error!("poll error: {}", message);
            }
            AppEvent::ForwardStarted {
                remote_port,
                local_port,
                label,
            } => {
                let tag = label.as_deref().unwrap_or("");
                if tag.is_empty() {
                    info!("forward started: :{} -> :{}", remote_port, local_port);
                } else {
                    info!(
                        "forward started: :{} -> :{} ({})",
                        remote_port, local_port, tag
                    );
                }
            }
            AppEvent::ForwardStopped {
                remote_port,
                reason,
            } => {
                info!("forward stopped: :{} — {}", remote_port, reason);
            }
            AppEvent::ForwardDied { remote_port } => {
                warn!("forward died: :{}", remote_port);
            }
            AppEvent::Shutdown => break,
        }
    }
}

/// The main polling loop. Sends `AppEvent`s through `tx` and exits
/// when the sender is dropped (TUI/consumer quit).
///
/// Two forwarding modes run simultaneously:
///
/// 1. **Explicit** – ports listed in `[[ports]]` are forwarded whenever they
///    appear on the remote, exactly as before.
///
/// 2. **Auto-discovery** – on the first successful poll the current set of
///    remote ports is recorded as the *baseline*. Any port that appears in a
///    later poll but was not in the baseline (and is not excluded or already
///    managed) is automatically forwarded.  When it disappears the tunnel is
///    torn down.
async fn run_poll_loop(
    host: String,
    ssh_port: u16,
    control_path: Option<String>,
    extra_args: Vec<String>,
    poll_interval: Duration,
    excluded: HashSet<u16>,
    cfg: Config,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut manager = ForwardManager::new();
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Ports that existed on the first successful poll – we don't forward these
    // automatically; only ports that *appear after* the baseline are forwarded.
    let mut baseline: Option<HashSet<u16>> = None;

    loop {
        interval.tick().await;

        // Check if consumer has quit
        if tx.is_closed() {
            manager.teardown_all().await;
            return;
        }

        // Reap dead ssh -L processes
        let dead = manager.reap_dead_forwards().await;
        for port in dead {
            let _ = tx.send(AppEvent::ForwardDied { remote_port: port });
        }

        // Discover listening ports on remote
        let remote_listening = match discovery::discover_listening_ports(
            &host,
            ssh_port,
            control_path.as_deref(),
            &extra_args,
        )
        .await
        {
            Ok(ports) => {
                let _ = tx.send(AppEvent::PollOk {
                    discovered: ports.len(),
                });
                ports
            }
            Err(e) => {
                let _ = tx.send(AppEvent::PollError {
                    message: e.to_string(),
                });
                continue;
            }
        };

        // -----------------------------------------------------------------
        // First successful poll: record baseline
        // -----------------------------------------------------------------
        if baseline.is_none() {
            tracing::info!(
                count = remote_listening.len(),
                "baseline snapshot recorded; new ports appearing after this will be auto-forwarded"
            );
            baseline = Some(remote_listening.clone());
        }
        let baseline_ports = baseline.as_ref().unwrap();

        // -----------------------------------------------------------------
        // Explicit forwards (from [[ports]] config)
        // -----------------------------------------------------------------
        let configured_ports: HashSet<u16> = cfg
            .ports
            .iter()
            .map(|m| m.remote_port)
            .filter(|p| !excluded.contains(p))
            .collect();

        for mapping in &cfg.ports {
            let rport = mapping.remote_port;
            if excluded.contains(&rport) || !remote_listening.contains(&rport) {
                continue;
            }
            if manager.is_active(rport) {
                continue;
            }

            let preferred = mapping.local_port.unwrap_or(rport);
            let local_port = match resolve_local_port(preferred).await {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.send(AppEvent::Log {
                        level: LogLevel::Error,
                        message: format!("cannot resolve local port for :{rport}: {e}"),
                    });
                    continue;
                }
            };

            match manager
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
                Ok(()) => {
                    let _ = tx.send(AppEvent::ForwardStarted {
                        remote_port: rport,
                        local_port,
                        label: mapping.label.clone(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Log {
                        level: LogLevel::Error,
                        message: format!("failed to start forward :{rport}: {e}"),
                    });
                }
            }
        }

        // -----------------------------------------------------------------
        // Auto-discovery forwards (ports that appeared after the baseline)
        // -----------------------------------------------------------------
        for &rport in &remote_listening {
            // Skip if excluded, in baseline, already configured explicitly,
            // or already managed
            if excluded.contains(&rport)
                || baseline_ports.contains(&rport)
                || configured_ports.contains(&rport)
                || manager.is_active(rport)
            {
                continue;
            }

            let local_port = match resolve_local_port(rport).await {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.send(AppEvent::Log {
                        level: LogLevel::Error,
                        message: format!("cannot resolve local port for :{rport}: {e}"),
                    });
                    continue;
                }
            };

            match manager
                .start_forward(
                    &host,
                    ssh_port,
                    control_path.as_deref(),
                    &extra_args,
                    rport,
                    local_port,
                    None,
                )
                .await
            {
                Ok(()) => {
                    let _ = tx.send(AppEvent::ForwardStarted {
                        remote_port: rport,
                        local_port,
                        label: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Log {
                        level: LogLevel::Error,
                        message: format!("auto-forward failed for :{rport}: {e}"),
                    });
                }
            }
        }

        // -----------------------------------------------------------------
        // Tear down forwards that stopped listening
        // -----------------------------------------------------------------
        let active: Vec<u16> = manager.active_remote_ports().collect();
        for rport in active {
            if !remote_listening.contains(&rport) {
                tracing::info!(
                    remote_port = rport,
                    "remote port no longer in listening set, tearing down forward"
                );
                manager.stop_forward(rport).await;
                let _ = tx.send(AppEvent::ForwardStopped {
                    remote_port: rport,
                    reason: "remote port no longer listening".into(),
                });
            }
        }
    }
}
