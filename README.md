# Svarm

Svarm is a terminal workspace for coding agents. It runs the native Codex, Claude Code, or Grok Build TUI in a real PTY, keeps multiple agents in one session, and provides a sidebar for switching between them. A per-user background server owns the agents, so the UI can detach and reconnect later.

Normal input goes directly to the focused agent, preserving the behavior of the native agent TUI.

## Install and run

Codex, Claude Code, and/or Grok Build must already be installed and available on `PATH`.

Yazi is optional. When installed, Svarm can use it for browsing directories; otherwise it uses its built-in browser.

```sh
# Install the latest supported GitHub release into ~/.local/bin:
curl -fsSL https://raw.githubusercontent.com/williamcr01/svarm/main/install.sh | sh

# Install a specific release or choose another directory:
curl -fsSL https://raw.githubusercontent.com/williamcr01/svarm/main/install.sh | sh -s -- v0.1.0
curl -fsSL https://raw.githubusercontent.com/williamcr01/svarm/main/install.sh | sh -s -- --dir /usr/local/bin --yes

# Check for and install a newer release:
svarm upgrade

# From a checkout of this repository:
cargo install --path .
svarm                         # Open or create a workspace-neutral session
svarm .                       # Open the new-agent form with this workspace
svarm --agent codex .         # Start Codex here
svarm --agent claude ../repo  # Claude Code in ../repo
svarm --agent grok .          # Grok Build here
svarm --new-session ../repo   # New session; seed the new-agent form
svarm --attach                # Attach only; never create
svarm list                    # List live Svarm sessions
```

Releases are published for Linux x86_64 and ARM64, and macOS Intel and Apple
Silicon. The convenience installer and `svarm upgrade` both verify the release
archive against `SHA256SUMS` before installing it. They need `curl` or `wget`,
`tar`, and `sha256sum` or `shasum`; installation stops if no checksum tool is
available. Pass `svarm upgrade --yes` to skip confirmation. If a Svarm server is
running during an upgrade, the server and all of its agents are stopped only
after the new release has been downloaded and verified.

The installer places `svarm` in `~/.local/bin`; add that directory to `PATH` if
needed. To review the script before running it, download it separately and run
`sh install.sh`.

Svarm requires a terminal of at least 80 columns by 24 rows. `NO_COLOR` applies to
Svarm's sidebar and custom UI; native agent applications keep control of their own colors.

## Session and server lifecycle

Detach, session stop, and server stop are intentionally different:

- `Ctrl+B d` disconnects this UI immediately. Agents, their PTYs, terminal screens, process status, order, selection, and unseen-output generations remain live in the server.
- `Ctrl+B q` confirms and then terminates every agent in the current Svarm session.
- `svarm stop --session ID` stops one named session. Without an ID, an interactive session chooser opens. `--yes` is accepted only with the unambiguous ID form.
- `svarm server stop` reports affected session and agent counts, confirms, then stops every session and the server.
- `svarm server status` reports reachability, PID, versions, socket, uptime, and client/session counts.

Closing a terminal window, losing SSH, or crashing only the client has the same server-side effect as detach. Reattachment reconstructs the live terminal and its bounded 10,000-row in-memory history; history is never unbounded or written to disk.

This is live-process persistence, not durable checkpointing. Agents do not survive a server crash, explicit server termination, operating-system termination, reboot, or machine failure. Terminal screens and agent output are never persisted to disk. If the operating system keeps the server alive through sleep/wake, sessions remain available.

The server exits after a short grace period when it has no sessions and no connected clients. An exited agent remains visible and keeps its session alive until explicitly closed.

## Troubleshooting

- Run `svarm server status`, then `svarm list`, to distinguish a missing server from a missing session.
- If a session is already attached, the error identifies the connection/process and attachment age. Re-run with `--attach --session ID --takeover` only when disconnecting that UI is intentional.
- Protocol version 10 is incompatible with older live clients and servers. A mismatch leaves the live server and its agents untouched; restart or use matching builds rather than deleting its socket.
- After a client connection error, Svarm restores the host terminal and prints the exact `svarm --attach --session ID` command. Agents may still be running.
- Missing remembered workspaces remain visible with a `missing` marker and cannot be selected. Press `b` to browse to another directory.
- If Yazi cannot be found on `PATH`, the native browser opens automatically. Permission and invalid-executable errors are reported instead of being treated as absence.

## Develop

The workspace is split by responsibility:

- `svarm-agent` launches and manages coding agents in pseudo-terminals.
- `svarm-tui` owns application state, terminal input, rendering, themes, and settings.
- the root `svarm` package contains only the executable entry point and CLI arguments.

Inside `svarm-tui`, the application model contains no terminal, process, or filesystem
resources. Runtime adapters prepare immutable observations for state updates and rendering,
while the renderer only draws the prepared frame.

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Before a release, manually verify Linux and macOS; local terminals and SSH; nesting in
tmux or another multiplexer; keyboard-only use; `NO_COLOR` and a basic 16-color
terminal; 80x24, 120x40, and a large terminal; Yazi installed, absent, customized,
cancelled, crashed, and force-closed; native browsing through hidden and symlinked
directories; mouse-disabled and mouse-reporting children; bracketed and plain paste;
several agents in different directories; resize while browsing; abrupt terminal
closure followed by attach; and sleep/wake while the operating system retains the
server.
