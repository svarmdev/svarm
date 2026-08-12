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
