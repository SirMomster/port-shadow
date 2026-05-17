use anyhow::Context;
use std::collections::HashSet;

/// Queries the remote host for TCP ports currently in LISTEN state.
/// Uses `ss -tlnp` (preferred) with fallback to `netstat -tlnp`.
///
/// Returns the set of port numbers that are listening.
pub async fn discover_listening_ports(
    host: &str,
    ssh_port: u16,
    control_path: Option<&str>,
    extra_args: &[String],
) -> anyhow::Result<HashSet<u16>> {
    // Try ss first, fall back to netstat
    let output = run_remote_command(
        host,
        ssh_port,
        control_path,
        extra_args,
        "ss -tlnp 2>/dev/null || netstat -tlnp 2>/dev/null",
    )
    .await
    .context("failed to run port discovery command on remote host")?;

    parse_listening_ports(&output)
}

/// Runs a single command on the remote over SSH, returning stdout as a String.
pub async fn run_remote_command(
    host: &str,
    ssh_port: u16,
    control_path: Option<&str>,
    extra_args: &[String],
    command: &str,
) -> anyhow::Result<String> {
    let mut args: Vec<String> = Vec::new();

    // Batch mode: never prompt for passwords
    args.push("-o".into());
    args.push("BatchMode=yes".into());

    // Disable strict host key prompting in automated mode
    args.push("-o".into());
    args.push("StrictHostKeyChecking=accept-new".into());

    // SSH port
    args.push("-p".into());
    args.push(ssh_port.to_string());

    // ControlPath multiplexing (Unix only)
    #[cfg(unix)]
    if let Some(cp) = control_path {
        args.push("-o".into());
        args.push(format!("ControlPath={cp}"));
        args.push("-o".into());
        args.push("ControlMaster=no".into());
    }

    // Extra user-supplied args
    args.extend_from_slice(extra_args);

    args.push(host.into());
    args.push(command.into());

    tracing::debug!(host, ?args, "running remote SSH command");

    let output = tokio::process::Command::new("ssh")
        .args(&args)
        .output()
        .await
        .context("failed to spawn ssh subprocess")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ssh command exited with {}: {stderr}",
            output.status
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses the output of `ss -tlnp` or `netstat -tlnp` into a set of port numbers.
fn parse_listening_ports(output: &str) -> anyhow::Result<HashSet<u16>> {
    let mut ports = HashSet::new();

    for line in output.lines() {
        let line = line.trim();

        // Skip header lines
        if line.starts_with("State")
            || line.starts_with("Proto")
            || line.starts_with("Active")
            || line.is_empty()
        {
            continue;
        }

        // Both ss and netstat have the local address in the 4th column (index 3)
        // ss output:  State  Recv-Q  Send-Q  Local Address:Port  ...
        // netstat:    tcp    0       0       0.0.0.0:3000         ...
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }

        let local_addr = cols[3];

        // Extract port from the last colon-separated segment
        if let Some(port_str) = local_addr.rsplit(':').next() {
            if let Ok(port) = port_str.parse::<u16>() {
                ports.insert(port);
            }
        }
    }

    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ss_output() {
        let input = r#"
State   Recv-Q  Send-Q  Local Address:Port  Peer Address:Port
LISTEN  0       128     0.0.0.0:22           0.0.0.0:*
LISTEN  0       128     0.0.0.0:3000         0.0.0.0:*
LISTEN  0       128     [::]:8080            [::]:*
"#;
        let ports = parse_listening_ports(input).unwrap();
        assert!(ports.contains(&22));
        assert!(ports.contains(&3000));
        assert!(ports.contains(&8080));
    }

    #[test]
    fn parses_netstat_output() {
        let input = r#"
Active Internet connections (only servers)
Proto Recv-Q Send-Q Local Address           Foreign Address         State
tcp        0      0 0.0.0.0:22              0.0.0.0:*               LISTEN
tcp        0      0 127.0.0.1:5432          0.0.0.0:*               LISTEN
tcp6       0      0 :::8080                 :::*                    LISTEN
"#;
        let ports = parse_listening_ports(input).unwrap();
        assert!(ports.contains(&22));
        assert!(ports.contains(&5432));
        assert!(ports.contains(&8080));
    }
}
