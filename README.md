# Svarm

Svarm is a small terminal multiplexer specifically for coding agents. It runs the native Codex or Claude Code TUI in a real PTY, keeps open agents in a fixed sidebar, and shows one agent at a time. A per-user background server owns the agents, so the UI can detach and reconnect later.

It does not speak ACP, Codex app-server, or Claude's streaming protocol. Normal input goes directly to the focused agent.

## Install and run

Codex and/or Claude Code must already be installed and available on `PATH`.

```sh
cargo install --path .
svarm                         # Open or create a workspace-neutral session
svarm .                       # Open the new-agent form with this workspace
svarm --agent codex .         # Start Codex here
svarm --agent claude ../repo  # Claude Code in ../repo
svarm --new-session ../repo   # New session; seed the new-agent form
svarm --attach                # Attach only; never create
svarm list                    # List live Svarm sessions
```

Sessions are process groups and attachment targets, not workspaces. Agents in one
session can run in different directories, and an empty session remains usable.
Normal startup discovers every live session and asks whether to open one or start a
new one. `--attach --session ID` opens a listed session directly; `--workspace ID`
is a hidden compatibility alias for one release. Add `--takeover` only when you
deliberately want to disconnect its current UI.

If startup needs a choice but stdin or stdout is not a terminal, Svarm prints the
eligible sessions and exits. Automation must choose explicitly with
`--attach --session ID` or `--new-session`.

Svarm requires a terminal of at least 80 columns by 24 rows. `NO_COLOR` applies to
Svarm's sidebar and custom UI; native agent applications keep control of their own colors.

## Keys

Every key except `Ctrl+B` belongs to the native agent TUI. Press `Ctrl+B`, release it, then press a command:

| Command | Action |
| --- | --- |
| `j`, `k`, or arrows | Select the next or previous agent |
| `1`–`9` | Select an agent directly |
| `PageUp`, `PageDown` | Scroll the selected agent's terminal history |
| `n` | Open the workspace/agent/start form |
| `b` | Toggle the sidebar |
| `m` | Open the sidebar menu |
| `x` | Close the selected agent, after confirmation |
| `d` | Detach — agents keep running |
| `q` | Stop session — terminates all agents, after confirmation |
| `?` | Show keybinds directly |
| `Ctrl+B` | Send a literal `Ctrl+B` to the agent |

The Menu control at the bottom of the sidebar is also clickable. Its Keybinds and
Settings entries work with the mouse or with arrows and Enter. Settings currently
contains the available theme palettes; the selected theme is saved in
`$XDG_CONFIG_HOME/svarm/settings.json` (or `~/.config/svarm/settings.json`).

Pasting respects the focused application's bracketed-paste mode. The mouse wheel scrolls an
overflowing sidebar. Over an agent, wheel events follow native terminal rules: applications that
requested the mouse receive them, alternate-screen applications that enable alternate scrolling
receive cursor input, and normal terminal output uses Svarm's bounded history. Use
`Ctrl+B, PageUp` to enter history explicitly.
Typing or pasting returns to the live screen. Clicks are forwarded only when the application has
requested mouse input. `Ctrl+C`, `Ctrl+Z`, and other terminal controls pass through normally.

The new-agent form defaults to the last successfully launched workspace and agent.
Workspace history is kept in most-recently-used order. From the workspace list,
press `b` to open Yazi inside Svarm. If `yazi` is not installed, Svarm opens its
keyboard-only native directory browser instead. While Yazi is focused, input belongs
only to Yazi; `Ctrl+B, x` force-closes it and `Ctrl+B, Ctrl+B` sends a literal prefix.
Yazi image previews are not composited, but text previews, colors, attributes, mouse,
paste, resize, and its cursor are supported.

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

## Runtime files and privacy

The Unix socket, singleton lock, and diagnostic PID file live in the private per-user runtime directory (`$XDG_RUNTIME_DIR/svarm` when safe, otherwise a UID-specific temporary directory). The directory is mode `0700`, the socket is private, and Linux/macOS peer credentials are checked.

Rotating `server.log` and `client.log` files live under `$XDG_STATE_HOME/svarm`, `~/.local/state/svarm`, or the private runtime fallback and are mode `0600`. Each is limited to roughly 1 MiB with two retained rotations. Default logs contain lifecycle information only: never terminal output, reconstructed screens, typed keys, pasted text, prompts, tokens, environment dumps, or full protocol payloads.

Yazi cwd-result files are created with mode `0600` in the private runtime directory
and removed after selection, cancellation, failure, force-close, or client shutdown.
Settings persist only the theme, canonical workspace history, and last agent kind.

## Troubleshooting

- Run `svarm server status`, then `svarm list`, to distinguish a missing server from a missing session.
- If a session is already attached, the error identifies the connection/process and attachment age. Re-run with `--attach --session ID --takeover` only when disconnecting that UI is intentional.
- Protocol version 3 is incompatible with older live clients and servers. A mismatch leaves the live server and its agents untouched; restart or use matching builds rather than deleting its socket.
- After a client connection error, Svarm restores the host terminal and prints the exact `svarm --attach --session ID` command. Agents may still be running.
- Missing remembered workspaces remain visible with a `missing` marker and cannot be selected. Press `b` to browse to another directory.
- If Yazi cannot be found on `PATH`, the native browser opens automatically. Permission and invalid-executable errors are reported instead of being treated as absence.

## Scope

Svarm deliberately has no remote listener, shells, tabs, pane splitting, arbitrary commands, or simultaneous writable clients for one session. It recognizes only Codex and Claude Code. Provider-specific blocked or idle states are not inferred from timing or vague screen matches.

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
