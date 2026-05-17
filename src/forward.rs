use anyhow::Context;
use std::collections::HashMap;
use tokio::process::Child;

/// Represents an active SSH port forward.
#[derive(Debug)]
pub struct ActiveForward {
    pub remote_port: u16,
    pub local_port: u16,
    #[allow(dead_code)] // reserved for future TUI/status display
    pub label: Option<String>,
    process: Child,
}

impl ActiveForward {
    /// Kills the underlying SSH process.
    pub async fn teardown(mut self) {
        tracing::info!(
            remote_port = self.remote_port,
            local_port = self.local_port,
            "tearing down forward"
        );
        if let Err(e) = self.process.kill().await {
            tracing::warn!(
                remote_port = self.remote_port,
                error = %e,
                "failed to kill ssh -L process"
            );
        }
        // Reap the process
        let _ = self.process.wait().await;
    }
}

/// Manages all active SSH port forwards, keyed by remote port.
#[derive(Debug, Default)]
pub struct ForwardManager {
    active: HashMap<u16, ActiveForward>,
}

impl ForwardManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if `remote_port` is already being forwarded.
    pub fn is_active(&self, remote_port: u16) -> bool {
        self.active.contains_key(&remote_port)
    }

    /// Returns the set of currently forwarded remote ports.
    pub fn active_remote_ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.active.keys().copied()
    }

    /// Starts a new `ssh -L` forward for `remote_port -> local_port`.
    /// On Unix, reuses the ControlPath master connection.
    /// On Windows, opens a fresh connection (ControlPath ignored).
    pub async fn start_forward(
        &mut self,
        host: &str,
        ssh_port: u16,
        control_path: Option<&str>,
        extra_args: &[String],
        remote_port: u16,
        local_port: u16,
        label: Option<String>,
    ) -> anyhow::Result<()> {
        let mut args: Vec<String> = Vec::new();

        // Don't allocate a terminal, run in background
        args.push("-N".into());

        // Batch mode
        args.push("-o".into());
        args.push("BatchMode=yes".into());
        args.push("-o".into());
        args.push("StrictHostKeyChecking=accept-new".into());

        // SSH port
        args.push("-p".into());
        args.push(ssh_port.to_string());

        // ControlPath (Unix only)
        #[cfg(unix)]
        if let Some(cp) = control_path {
            args.push("-o".into());
            args.push(format!("ControlPath={cp}"));
            args.push("-o".into());
            args.push("ControlMaster=no".into());
        }

        #[cfg(windows)]
        if control_path.is_some() {
            tracing::warn!(
                "ControlPath is not supported on Windows; using a fresh SSH connection for each forward"
            );
        }

        // Extra user-supplied args
        args.extend_from_slice(extra_args);

        // -L local_port:localhost:remote_port
        args.push("-L".into());
        args.push(format!("{local_port}:localhost:{remote_port}"));

        args.push(host.into());

        tracing::debug!(
            remote_port,
            local_port,
            host,
            ?args,
            "spawning ssh -L process"
        );

        let process = tokio::process::Command::new("ssh")
            .args(&args)
            // Don't inherit stdin so it can't accidentally grab terminal input
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn ssh -L for port {remote_port}"))?;

        tracing::info!(
            remote_port,
            local_port,
            label = label.as_deref().unwrap_or("-"),
            "forward started"
        );

        self.active.insert(
            remote_port,
            ActiveForward {
                remote_port,
                local_port,
                label,
                process,
            },
        );

        Ok(())
    }

    /// Stops the forward for `remote_port` if one is active.
    pub async fn stop_forward(&mut self, remote_port: u16) {
        if let Some(fwd) = self.active.remove(&remote_port) {
            fwd.teardown().await;
        }
    }

    /// Tears down all active forwards.
    pub async fn teardown_all(&mut self) {
        let ports: Vec<u16> = self.active.keys().copied().collect();
        for port in ports {
            self.stop_forward(port).await;
        }
    }

    /// Checks if any previously-active forwards have had their SSH process die
    /// unexpectedly, and removes them from the active set.
    pub async fn reap_dead_forwards(&mut self) {
        let mut dead: Vec<u16> = Vec::new();
        for (port, fwd) in self.active.iter_mut() {
            match fwd.process.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!(
                        remote_port = port,
                        local_port = fwd.local_port,
                        exit_status = %status,
                        "ssh -L process exited unexpectedly"
                    );
                    dead.push(*port);
                }
                Ok(None) => {} // still running
                Err(e) => {
                    tracing::warn!(remote_port = port, error = %e, "failed to check process status");
                }
            }
        }
        for port in dead {
            self.active.remove(&port);
        }
    }
}

/// Finds a local port to use for the given remote port.
/// Tries `preferred` first; if occupied, picks a random ephemeral port.
pub async fn resolve_local_port(preferred: u16) -> anyhow::Result<u16> {
    if is_port_available(preferred).await {
        return Ok(preferred);
    }

    tracing::debug!(
        preferred,
        "preferred local port is occupied, picking ephemeral port"
    );

    // Bind to port 0 to let the OS assign an ephemeral port
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", 0u16))
            .await
            .context("failed to bind to ephemeral port")?;
    let addr = listener.local_addr()?;
    // Drop the listener immediately; the port may be reused before ssh -L binds
    // it, but this is the standard approach for ephemeral port selection.
    Ok(addr.port())
}

/// Returns true if TCP port `port` on 127.0.0.1 is not currently in use.
async fn is_port_available(port: u16) -> bool {
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok()
}
