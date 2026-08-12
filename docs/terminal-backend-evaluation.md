# Terminal backend evaluation

## Decision

Keep `vt100-psmux` behind Svarm's `TerminalBackend` adapter for now. The scrolling hot path no
longer depends on an emulator-specific wire stream: Svarm owns compact semantic snapshots, viewport
flow control, and the 25 MB per-agent storage policy. A backend replacement therefore does not need
to solve the client/server performance problem again.

`libghostty-vt` is the preferred backend to reevaluate, but it is not yet a suitable required build
dependency for Svarm:

- its public C API describes itself as unstable and subject to breaking changes;
- its build adds a Zig/C toolchain requirement, and Zig is not present in Svarm's current build
  environment;
- its terminal options already provide byte- and line-bounded scrollback, while its render state
  exposes full/partial dirty state and dirty rows—the two capabilities that would materially improve
  the current adapter.

Primary API references: [libghostty-vt API status](https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt.h),
[terminal scrollback options](https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt/terminal.h),
and [dirty render state](https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt/render.h).

## Re-evaluation gate

Run a replacement spike when the C API has a tagged compatibility policy or Svarm has a portable
way to supply the required Zig-built library. Keep the experiment entirely inside
`crates/svarm-agent/src/terminal_backend.rs` and prove these behaviors before switching:

1. split PTY input, Unicode graphemes, wide cells, colors, attributes, OSC 8 links, titles, cursor
   styles, bells, clipboard requests, progress, shell metadata, and all advertised input modes;
2. 25 MB logical scrollback, oldest-first eviction, viewport clamping, resize reflow, alternate
   screen behavior, and history beyond 10,000 rows when it fits;
3. dirty rows can produce Svarm `TerminalSnapshotDiff` values without rebuilding the complete grid;
4. builds and tests pass on the supported Linux targets without requiring a system-installed
   Ghostty application.

The Svarm protocol must remain backend-neutral. A Ghostty adapter should emit the existing semantic
model and must not expose Ghostty handles or snapshot formats to the TUI.
