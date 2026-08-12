# svarm

A small terminal multiplexer for coding agents

## Architecture

- **Keep the application model inert.** `app.rs` contains user-visible state and deterministic state transitions only. Process handles, terminal parsers, filesystem access, environment reads, clocks, and other live resources belong to runtime adapters.

- **Prepare a frame before drawing it.** Rendering receives an immutable `UiModel` assembled by the runtime and writes only to the Ratatui frame. A render function must not acquire locks, read environment variables, perform I/O, poll processes, or change application state.

- **Give each type one operational reason to change.** State, PTY management, terminal setup, settings persistence, input translation, and widget composition have separate owners. Add behavior to the narrowest existing owner instead of growing a central coordinator into a catch-all object.

- **Keep external systems at the edges.** Crossterm setup and teardown, pseudo-terminals, child processes, environment discovery, and filesystem persistence stay in their adapter modules. Pass plain values or snapshots across the boundary so core state can be tested without a terminal or subprocess.

- **Separate recognition from reaction.** A detector reports what exact input it recognized; another layer decides what action to take. Detectors must be independently testable and must not write to a PTY, mutate the UI, or trigger lifecycle operations themselves.

- **Make screen-derived claims explainable.** Never label an agent idle, blocked, finished, or awaiting input from timing, missing text, or a single vague match. Default to unknown unless affirmative provider-specific evidence exists, retain the evidence with the result, and cover positive, negative, partial, and split-input cases with fixtures.

- **Share the interaction vocabulary.** Menu entries, keybinding descriptions, semantic colors, dialog shells, and layout calculations should have one canonical definition reused by rendering and input handling. A new view should compose the established patterns before introducing a parallel version.

## Repository map

- `crates/svarm-agent` owns coding-agent commands, child processes, PTYs, terminal parsing, and terminal-protocol recognition.
- `crates/svarm-agent/src/cwd.rs` reads a live process's working directory, so an agent that moves into another checkout is observed where it actually is and `git.rs` probes there.
- `crates/svarm-agent/src/naming.rs` owns headless conversation naming: it asks the session's own agent (`claude -p`, `codex exec`, `grok -p`) for a short name in the background and reduces its output to one line.
- `crates/svarm-tui/src/app.rs` is the pure application model.
- `crates/svarm-tui/src/agents.rs`, `settings.rs`, and `terminal.rs` own live resources and platform effects.
- `crates/svarm-tui/src/runtime.rs` coordinates adapters and applies their observations to the model.
- `crates/svarm-tui/src/input.rs` translates terminal input and contains canonical management-key metadata.
- `crates/svarm-tui/src/ui.rs` renders immutable prepared data; `theme.rs` contains pure semantic styling.
- `crates/svarm-tui/src/screen.rs` draws an agent's emulated screen, and owns the translation from terminal cell to buffer cell so agent colors and attributes survive rendering.
- `crates/svarm-tui/src/screen.rs` draws an agent's emulated screen, and owns the translation from terminal cell to buffer cell so agent colors and attributes survive rendering.
- `src` is only the executable and command-line boundary.

Do not add another crate merely to enforce a conceptual layer. Prefer a focused module until there is a real independent package boundary.

## TUI modal sizes

- **Compact — 64x12.** Short forms and pickers, including the new-agent flow.
- **Standard — 72x18.** Confirmations, settings, and help.
- **Large — responsive, capped at 100x30.** Content-heavy or embedded interactive views, currently the native filesystem browser and Yazi; leave 2 rows and 4 columns around it.

Always center and clamp modals to the terminal. Every centered modal must pass one of these tiers to `render_dialog`; never pass raw dimensions or a precomputed `Rect`. The sidebar-anchored menu popover and full-screen startup chooser are not modals. Add another tier only when content demonstrably cannot fit an existing one at 80x24.

## Working agreement

- Preserve native agent terminal behavior. Normal input, control keys, paste mode, mouse protocol, color queries, and resize events must continue to flow according to the child terminal's advertised modes.
- Every user-visible action must work with both keyboard and mouse. Any rendered keybinding hint must be clickable and invoke the same canonical action; add coverage for both input paths when introducing or changing an interaction.
- Keep the UI usable at 80x24, without color, and entirely from the keyboard. Pair status colors with text or symbols so color is never the only signal.
- Add the smallest test that proves each non-trivial behavior. Pure state transitions and detectors should use unit tests; PTY behavior should use focused integration-style tests with bounded waits.
- Document every behavioral change to vendored code in `vendor/PATCHES.md`, including its source version, purpose, and regression coverage.
- Before handing off a code change, run:

  ```sh
  cargo fmt --all --check
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  ```

## Commit discipline

- Do not create, amend, squash, or push commits unless the user asks. When commits are requested, keep each commit independently understandable and limited to one logical concern; tests and documentation for that concern belong in the same commit.
- Use Conventional Commit subjects in the form `type(optional-scope): imperative summary`. Keep the summary concrete and use a body when the motivation or tradeoff is not obvious.
- Prefer these types: `feat` for user-visible capability, `fix` for defects, `refactor` for behavior-preserving structure, `test` for test-only work, `docs` for documentation, `perf` for measured performance work, `build` for dependencies/build tooling, `ci` for automation, and `chore` for maintenance that fits nowhere else.
- Mark breaking changes with `!` before the colon and explain the migration in a `BREAKING CHANGE:` footer. Avoid mixed commits such as feature work plus unrelated cleanup, vague subjects such as “updates,” and commits that leave tests knowingly broken.
