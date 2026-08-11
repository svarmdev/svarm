# Svarm

Svarm is a small terminal multiplexer specifically for coding agents. It runs the native Codex or Claude Code TUI in a real PTY, keeps open agents in a fixed sidebar, and shows one agent at a time. A per-user background server owns the agents, so the UI can detach and reconnect later.

It does not speak ACP, Codex app-server, or Claude's streaming protocol. Normal input goes directly to the focused agent.

## Install and run

Codex and/or Claude Code must already be installed and available on `PATH`.

```sh
cargo install --path .
svarm                         # Choose an agent in the current directory
svarm --agent codex           # Codex in the current directory
svarm --agent claude ../repo  # Claude Code in ../repo
svarm --new-session ../repo   # Always create a distinct Svarm session
svarm --attach                # Attach only; never create
svarm list                    # List live Svarm sessions
```

Normal startup discovers every live session. With one or more sessions it asks whether to open an existing session or start a new one. `--attach --workspace ID` opens a listed session directly; add `--takeover` only when you deliberately want to disconnect its current UI. Multiple sessions may use the same workspace path and remain distinct by ID.

If startup needs a choice but stdin or stdout is not a terminal, Svarm prints the eligible sessions and exits. Automation must choose explicitly with `--attach --workspace ID` or `--new-session`.

Svarm requires a terminal of at least 80 columns by 24 rows. `NO_COLOR` applies to
Svarm's sidebar and custom UI; native agent applications keep control of their own colors.

## Keys

Every key except `Ctrl+B` belongs to the native agent TUI. Press `Ctrl+B`, release it, then press a command:

| Command | Action |
| --- | --- |
| `j`, `k`, or arrows | Select the next or previous agent |
| `1`–`9` | Select an agent directly |
| `n`, then `c` or `a` | Start Codex or Claude Code |
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

Pasting respects the focused application's bracketed-paste mode. Mouse events requested
by the focused application, `Ctrl+C`, `Ctrl+Z`, and other terminal controls pass through
normally.

## Session and server lifecycle

Detach, session stop, and server stop are intentionally different:

- `Ctrl+B d` disconnects this UI immediately. Agents, their PTYs, terminal screens, process status, order, selection, and unseen-output generations remain live in the server.
- `Ctrl+B q` confirms and then terminates every agent in the current Svarm session.
- `svarm stop --workspace ID` stops one named session. `--yes` is accepted only with this unambiguous ID form.
- `svarm server stop` reports affected session and agent counts, confirms, then stops every session and the server.
- `svarm server status` reports reachability, PID, versions, socket, uptime, and client/session counts.

Closing a terminal window, losing SSH, or crashing only the client has the same server-side effect as detach. Reattachment reconstructs the bounded visible terminal state and current input modes; it does not replay an unbounded output history.

This is live-process persistence, not durable checkpointing. Agents do not survive a server crash, explicit server termination, operating-system termination, reboot, or machine failure. Terminal screens and agent output are never persisted to disk. If the operating system keeps the server alive through sleep/wake, sessions remain available.

The server exits after a short grace period when it has no sessions and no connected clients. An exited agent remains visible and keeps its session alive until explicitly closed.

## Runtime files and privacy

The Unix socket, singleton lock, and diagnostic PID file live in the private per-user runtime directory (`$XDG_RUNTIME_DIR/svarm` when safe, otherwise a UID-specific temporary directory). The directory is mode `0700`, the socket is private, and Linux/macOS peer credentials are checked.

Rotating `server.log` and `client.log` files live under `$XDG_STATE_HOME/svarm`, `~/.local/state/svarm`, or the private runtime fallback and are mode `0600`. Each is limited to roughly 1 MiB with two retained rotations. Default logs contain lifecycle information only: never terminal output, reconstructed screens, typed keys, pasted text, prompts, tokens, environment dumps, or full protocol payloads.

## Troubleshooting

- Run `svarm server status`, then `svarm list`, to distinguish a missing server from a missing session.
- If a session is already attached, the error identifies the connection/process and attachment age. Re-run with `--attach --workspace ID --takeover` only when disconnecting that UI is intentional.
- A protocol-version mismatch leaves the live server and its agents untouched. Use matching Svarm client/server versions; do not delete the socket or kill a PID from the diagnostic file.
- After a client connection error, Svarm restores the host terminal and prints the exact `svarm --attach --workspace ID` command. Agents may still be running.
- If a workspace path later disappears, use `svarm list` and address the live session by ID.

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

Before a release, manually verify Linux and macOS; local terminals and SSH; nesting in tmux or another multiplexer; keyboard-only use; `NO_COLOR` and a basic 16-color terminal; 80x24, 120x40, and a large terminal; dark and light themes across at least three terminal emulators; mouse-disabled and mouse-reporting child applications; bracketed and plain paste; abrupt terminal closure followed by attach; and sleep/wake while the operating system retains the server.
