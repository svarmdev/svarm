# Svarm protocol

Svarm uses length-prefixed JSON envelopes over its local Unix socket. Protocol version 6 replaced
terminal-emulator byte streams with Svarm-owned semantic terminal frames. Clients do not need a VT
parser and terminal backends are not part of the wire format.

## Semantic terminal state

A `terminal_full` event contains an agent id, output generation, terminal sequence, and one complete
`snapshot`. A snapshot is sufficient to reconstruct the visible terminal without earlier events and
contains:

- terminal size and one semantic cell per row and column;
- cell text, wide-character flags, foreground/background colors, text attributes, and an optional
  one-based OSC 8 hyperlink reference;
- wrapped-row flags;
- cursor position, visibility, and requested style;
- alternate-screen state and scrollback position, retained row count, and capacity;
- application cursor/keypad, bracketed paste, keyboard, alternate-scroll, mouse protocol, and mouse
  encoding modes;
- title, OSC 7 working directory, OSC 8 hyperlink URI table, OSC 9;4 progress, pending OSC 52
  clipboard request, bell counters, and shell command metadata.

Colors are `default`, an indexed palette entry, or an RGB triple. Text attributes are independent
booleans for bold, dim, italic, underline, inverse, blink, hidden, and strikethrough. Cell contents
are Unicode strings and may include combining characters.

A `terminal_viewport` event carries the same complete snapshot shape for a requested historical
scrollback position. Historical viewports are separate from the live frame sequence.

## Incremental frames

A `terminal_diff` event contains `base_sequence`, `sequence`, and a semantic `diff`. The diff carries
the next non-grid terminal state plus only changed cells (by row-major cell index) and changed
wrapped-row flags. Resize changes are sent as a new full frame because cell indices are defined by
the snapshot size.

The client applies a diff only when all of these conditions hold:

1. it has a live snapshot for the agent;
2. `base_sequence` equals the last accepted sequence;
3. `sequence` is newer than the last accepted sequence;
4. the diff size matches the live snapshot and every cell, row, and hyperlink reference is valid.

Older or repeated frames are duplicates and do not change the screen. A missing base, sequence gap,
wrong size, or invalid patch causes the client to request `resync_terminal`. The server answers with
a complete semantic frame. Successfully accepted and duplicate frames are acknowledged with
`acknowledge_frame`; the existing acknowledgement and queue-pressure behavior remains unchanged.

## Backend boundary

The PTY runtime owns a terminal backend implementing Svarm's `TerminalBackend` interface. The
current `vt100-psmux` adapter parses arbitrary and split PTY output and converts its state into
semantic snapshots. Runtime coordination, the protocol, client cache, application model, and
renderer use only Svarm terminal types. A replacement emulator therefore changes the adapter and
backend construction, not frame or UI semantics.
