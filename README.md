# port-shadow

SSH port-forwarding daemon that mirrors VSCode's "Ports" panel. It polls a remote host for newly listening TCP ports, automatically forwards the ones you have configured to your local machine, and tears them down when they stop.

```
┌ port-shadow — user@myhost — 2 active forward(s) ────────────────────────────┐
│ Remote Port   Local Port   Label                Status    Uptime            │
│                                                                              │
│▶ :3000        :3000        dev server           active    4m12s             │
│  :5432        :5432        postgres             active    4m12s             │
│  :8080        :9341        app server           active    2m05s             │
│  :9000        :9000        old worker           stopped   12m31s            │
└──────────────────────────────────────────────────────────────────────────────┘
┌ logs ────────────────────────────────────────────────────────────────────────┐
│ 14:03:01 INFO  forward started  :3000 → localhost:3000                       │
│ 14:03:01 INFO  forward started  :5432 → localhost:5432                       │
│ 14:05:08 WARN  forward stopped  :9000  (remote port no longer listening)     │
└──────────────────────────────────────────────────────────────────────────────┘
 last poll ok — 3 remote port(s) listening    q/^C quit  ↑↓/jk  PgUp/PgDn logs
```

## How it works

1. You pre-establish an SSH master connection to your remote host.
2. You declare which ports to forward in `.port-shadow.toml` in your project directory.
3. `port-shadow` polls the remote every N seconds (default: 5). When a configured port starts listening, it spawns `ssh -L` reusing the master socket. When it stops, the tunnel is torn down.
4. A terminal UI shows all active and recently stopped forwards, plus a live log panel.

Port mapping is 1:1 by default (remote `:3000` → local `:3000`). If the preferred local port is already in use, an ephemeral port is assigned automatically.

## Prerequisites

- `ssh` must be on your `PATH`.
- On Unix/macOS: OpenSSH with `ControlMaster` support (standard in any modern install).
- On Windows: OpenSSH for Windows (available since Windows 10 1809). `ControlPath` is not supported on Windows; each forward uses a separate SSH connection.

## Installation

### From source

```sh
cargo install --path .
```

### Cross-compiled release binaries

Use [`cross`](https://github.com/cross-rs/cross) and the provided `justfile`:

```sh
cargo install cross
just build-all   # builds all targets into target/<triple>/release/
just dist        # collects binaries into ./dist/
```

| Target | Binary |
|---|---|
| Linux x86\_64 | `dist/port-shadow-linux-x86_64` |
| Linux aarch64 | `dist/port-shadow-linux-aarch64` |
| Windows x86\_64 | `dist/port-shadow-windows-x86_64.exe` |
| macOS Apple Silicon | requires native build or `macos-latest` CI runner |

## Quick start

**1. Start the SSH master connection**

```sh
ssh -M -S /tmp/myhost.sock -N -f user@myhost
```

The `-f` flag backgrounds the process. The socket path (`/tmp/myhost.sock`) is what you pass to `port-shadow`.

**2. Create `.port-shadow.toml` in your project directory**

```toml
[ssh]
host = "user@myhost"
control_path = "/tmp/myhost.sock"

[[ports]]
remote_port = 3000
label = "dev server"

[[ports]]
remote_port = 5432
label = "postgres"
```

**3. Run**

```sh
port-shadow
```

Press `q` or `Ctrl+C` to quit. All tunnels are torn down on exit.

## Configuration reference

The config file is named `.port-shadow.toml` and is read from the current working directory. A different path can be specified with `--config`.

### `[ssh]`

| Key | Type | Default | Description |
|---|---|---|---|
| `host` | string | — | SSH destination (`user@hostname`). Required unless passed as `--host`. |
| `control_path` | string | — | Path to the master ControlPath socket (Unix only). |
| `poll_interval_secs` | integer | `5` | How often to poll for new/removed ports. |
| `port` | integer | `22` | Remote SSH port. |
| `extra_args` | string array | `[]` | Extra arguments passed verbatim to every `ssh` invocation (e.g. `["-i", "~/.ssh/id_ed25519"]`). |

### `[[ports]]`

Each `[[ports]]` entry declares one port to forward. Only listed ports are forwarded — there is no auto-discovery.

| Key | Type | Required | Description |
|---|---|---|---|
| `remote_port` | integer | yes | Port number on the remote host. |
| `local_port` | integer | no | Local port to bind. Defaults to `remote_port`; falls back to an ephemeral port if occupied. |
| `label` | string | no | Human-readable name shown in the TUI. |

### `excluded_ports`

A list of port numbers that are never forwarded, even if they appear in `[[ports]]`. Defaults to `[22]`.

```toml
excluded_ports = [22, 2222]
```

### Full example

See [`.port-shadow.example.toml`](.port-shadow.example.toml).

## CLI reference

```
Usage: port-shadow [OPTIONS]

Options:
  -H, --host <HOST>                    SSH destination [env: PORT_SHADOW_HOST]
  -S, --control-path <CONTROL_PATH>    ControlPath socket (Unix only) [env: PORT_SHADOW_CONTROL_PATH]
  -i, --poll-interval <POLL_INTERVAL>  Poll interval in seconds [env: PORT_SHADOW_POLL_INTERVAL]
  -p, --ssh-port <SSH_PORT>            Remote SSH port [env: PORT_SHADOW_PORT]
  -c, --config <CONFIG>                Path to config file [env: PORT_SHADOW_CONFIG]
  -v, --verbose...                     Increase log verbosity (-v, -vv, -vvv)
  -h, --help                           Print help
  -V, --version                        Print version
```

CLI flags take precedence over config file values. All flags also read from environment variables.

## TUI keybindings

| Key | Action |
|---|---|
| `q` / `Ctrl+C` | Quit and tear down all forwards |
| `↑` / `k` | Select previous row |
| `↓` / `j` | Select next row |
| `PgUp` | Scroll log panel up |
| `PgDn` | Scroll log panel down |
| `G` | Jump to newest log entries |

Tracing/debug output is written to `port-shadow.log` in the working directory so it does not interfere with the TUI.

## Windows notes

`ControlPath` / `ControlMaster` is not supported by OpenSSH on Windows. When `control_path` is set on Windows, `port-shadow` will log a warning and open a separate SSH connection for each port forward. This means each forward will re-authenticate independently; using an SSH agent (`ssh-add`) is strongly recommended.

## Development

```sh
just build       # debug build for current host
just test        # run tests
just lint        # clippy
just fmt         # rustfmt
just run -- --help
```

## License

MIT
