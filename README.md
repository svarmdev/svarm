# Svarm

Svarm is a small terminal multiplexer specifically for coding agents. It runs the native Codex or Claude Code TUI in a real PTY, keeps open agents in a fixed sidebar, and shows one agent at a time.

It does not speak ACP, Codex app-server, or Claude's streaming protocol. Normal input goes directly to the focused agent.

## Install and run

Codex and/or Claude Code must already be installed and available on `PATH`.

```sh
cargo install --path .
svarm                         # Codex in the current directory
svarm --agent claude ../repo  # Claude Code in ../repo
```

Svarm requires a terminal of at least 80 columns by 24 rows. It supports `NO_COLOR`.

## Keys

Every key except `Ctrl+B` belongs to the native agent TUI. Press `Ctrl+B`, release it, then press a command:

| Command | Action |
| --- | --- |
| `j`, `k`, or arrows | Select the next or previous agent |
| `1`–`9` | Select an agent directly |
| `n`, then `c` or `a` | Start Codex or Claude Code |
| `b` | Toggle the sidebar |
| `x` | Close the selected agent, after confirmation |
| `q` | Stop all agents and quit, after confirmation |
| `?` | Show help |
| `Ctrl+B` | Send a literal `Ctrl+B` to the agent |

Pasting respects the focused application's bracketed-paste mode. `Ctrl+C`, `Ctrl+Z`, and other terminal controls pass through normally.

## Scope

Svarm deliberately has no shells, tabs, pane splitting, or arbitrary commands. It recognizes only Codex and Claude Code.

Sessions currently live for the lifetime of the Svarm process; quitting stops them. Sidebar state reports process lifecycle and unseen output. Durable detach/reattach and provider-specific blocked/idle integrations are intentionally left for a later version rather than inferred by screen-scraping.

## Develop

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

