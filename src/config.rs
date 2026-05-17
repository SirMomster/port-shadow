use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// A single port mapping entry from the config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PortMapping {
    /// Port number on the remote host to forward.
    pub remote_port: u16,
    /// Local port to bind. If omitted, defaults to same as remote_port,
    /// falling back to an ephemeral port if the preferred port is occupied.
    pub local_port: Option<u16>,
    /// Human-readable label shown in log output.
    pub label: Option<String>,
}

/// Top-level configuration file schema.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Config {
    /// SSH connection settings. All fields can be overridden by CLI flags.
    #[serde(default)]
    pub ssh: SshConfig,

    /// Ports to forward. Only ports listed here are forwarded (opt-in).
    #[serde(default)]
    pub ports: Vec<PortMapping>,

    /// Ports that should never be forwarded even if listed elsewhere.
    /// Defaults to [22] if not specified.
    #[serde(default = "default_excluded_ports")]
    pub excluded_ports: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SshConfig {
    /// SSH destination, e.g. "user@hostname" or just "hostname".
    pub host: Option<String>,
    /// Path to the ControlPath socket for connection multiplexing.
    /// On Windows this is ignored and a fresh connection is made per forward.
    pub control_path: Option<String>,
    /// How often (in seconds) to poll the remote for new listening ports.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// SSH port (default 22).
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// Additional SSH options passed verbatim to the ssh command.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_poll_interval() -> u64 {
    5
}

fn default_ssh_port() -> u16 {
    22
}

fn default_excluded_ports() -> Vec<u16> {
    vec![22]
}

impl Config {
    /// Load config from `.port-shadow.toml` in the given directory.
    /// Returns `Ok(Config::default())` if the file does not exist.
    pub fn load_from_dir(dir: &Path) -> anyhow::Result<Self> {
        let config_path = dir.join(".port-shadow.toml");
        if !config_path.exists() {
            tracing::debug!(
                path = %config_path.display(),
                "no config file found, using defaults"
            );
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config file: {e}"))?;
        tracing::info!(
            path = %config_path.display(),
            port_count = config.ports.len(),
            "loaded config"
        );
        Ok(config)
    }

    /// Returns the set of excluded ports.
    pub fn excluded_set(&self) -> HashSet<u16> {
        self.excluded_ports.iter().copied().collect()
    }
}
