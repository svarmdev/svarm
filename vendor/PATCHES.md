# Vendored patches

Every behavioral change to code under `vendor/` must be recorded here.

## `vt100-psmux` 0.16.9

Source: the `vt100-psmux` 0.16.9 crate published on crates.io.

### Retain top-anchored scroll-region output

- File: `vt100-psmux/src/grid.rs`
- Location: `Grid::scroll_up`
- Change: rows enter scrollback when the active scroll region begins at row zero. Upstream only
  retains rows when no scroll region is active.
- Reason: inline terminal applications keep a composer below a top-anchored transcript region.
  Rows leaving that region are terminal history; discarding them makes normal scrollback lose
  completed output.
- Regression test:
  `terminal_process::tests::top_anchored_scroll_regions_feed_host_history`

When upgrading the crate, remove this patch if upstream has equivalent behavior. Otherwise,
reapply it and run the regression test before updating this version entry.

### Bound scrollback by logical allocation size

- Files: `vt100-psmux/src/{grid,parser,perform,row,screen}.rs`
- Change: add a byte-budgeted parser constructor. The main grid charges the allocated capacity of
  its active and historical rows, keeps the active screen even when it exceeds the budget, and
  evicts the oldest history rows until the configured budget is met.
- Reason: a row limit makes memory consumption grow with pane width. Svarm runs multiple agent
  terminals, so a per-terminal byte budget provides predictable memory use while allowing the
  backend representation to determine the retained row count.
- Regression tests: `terminal_backend::tests::byte_budget_retains_more_narrow_rows`,
  `terminal_backend::tests::active_rows_survive_a_smaller_budget`, and
  `terminal_backend::tests::resize_charges_mixed_width_history`.

When upgrading the crate, remove this patch if upstream offers an equivalent byte budget that
includes active rows. Otherwise, reapply it and run the regression tests above.

### Bound OSC 8 hyperlink metadata

- File: `vt100-psmux/src/screen.rs`
- Change: reject individual hyperlink URIs above 4 KiB and stop interning new unique URIs after
  the per-screen table reaches 1 MB. Existing links remain valid and duplicate URIs remain usable.
- Reason: cells store hyperlink identifiers, but the URI table otherwise grows independently of
  scrollback eviction and could defeat the terminal memory budget.
- Regression test: `terminal_backend::tests::hyperlink_metadata_is_bounded`.

When upgrading the crate, retain equivalent per-screen limits or reapply this patch and its test.
