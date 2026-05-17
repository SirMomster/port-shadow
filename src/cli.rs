use clap::Parser;

/// port-shadow: SSH port-forwarding daemon.
///
/// Periodically polls a remote host for listening ports and forwards them
/// to the local machine via SSH tunnels, mirroring VSCode's port forwarding.
///
/// Ports to forward must be declared in `.port-shadow.toml` in the current
/// directory (opt-in). The SSH master connection must be pre-established
/// before running this tool (Unix only). On Windows a fresh connection is
/// made for each port forward.
///
/// Example:
///   ssh -M -S /tmp/myhost.sock -N user@myhost &
///   port-shadow --host user@myhost --control-path /tmp/myhost.sock
#[derive(Debug, Parser)]
#[command(name = "port-shadow", version, about, long_about = None)]
pub struct Cli {
    /// SSH destination, e.g. "user@hostname". Overrides config file value.
    #[arg(long, short = 'H', env = "PORT_SHADOW_HOST")]
    pub host: Option<String>,

    /// Path to the SSH ControlPath socket (Unix only).
    /// Must match the socket used when establishing the master connection.
    /// Overrides config file value.
    #[arg(long, short = 'S', env = "PORT_SHADOW_CONTROL_PATH")]
    pub control_path: Option<String>,

    /// How often to poll the remote for changes (seconds). Overrides config.
    #[arg(long, short = 'i', env = "PORT_SHADOW_POLL_INTERVAL")]
    pub poll_interval: Option<u64>,

    /// SSH port on the remote host. Overrides config.
    #[arg(long, short = 'p', env = "PORT_SHADOW_PORT")]
    pub ssh_port: Option<u16>,

    /// Path to the config file. Defaults to `.port-shadow.toml` in the
    /// current working directory.
    #[arg(long, short = 'c', env = "PORT_SHADOW_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Log verbosity. Use multiple times for more detail (-v, -vv, -vvv).
    #[arg(long, short = 'v', action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Enable the terminal user interface. Without this flag, events are
    /// printed as plain log lines to stdout.
    #[arg(long)]
    pub tui: bool,
}
