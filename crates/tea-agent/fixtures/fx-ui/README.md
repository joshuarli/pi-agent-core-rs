# `fx` UI oracle fixtures

This directory is the tea-owned, normalized-cell boundary for the `fx` terminal
reference. It preserves text/grid geometry and cursor coordinates from the
existing replay archive plus fresh default-minimal PTY captures. `fx/` holds
reference evidence; `tea/` holds reviewed deterministic `Grid<Cell>` targets
that substitute only manifest-approved tea identity/status semantics.

The `fx/` fixtures do not claim color, attribute, or final cursor-paint evidence.
The Render Lab README explicitly excludes those from its default pass/fail
oracle. Their `foreground` and `background` are therefore `null`, and their
`attributes` are empty until a reproducible terminal-stream or virtual-terminal
capture is added. Tea-owned fixtures record the deterministic cell styles and
cursor target produced by tea's renderer; they are not claims about fx styling.

## Format

Each `*.cells.json` file has `format_version: 1` and `kind:
"normalized_grid_fixture"`. A fixture contains a fixed `size`, a `cursor`, and
`runs`. A run expands to one normalized cell per Unicode scalar:

```json
{
  "row": 7,
  "column": 0,
  "text": "❯",
  "style": {
    "foreground": null,
    "background": null,
    "attributes": []
  }
}
```

For a repeated single-character run (such as a divider), `repeat` gives the
number of cells without making the fixture depend on a long literal:
`{"row": 8, "column": 0, "text": "─", "repeat": 167}`.

Unlisted cells are blank cells using `cell_defaults`. Runs must not overlap and
must stay inside the declared rectangle. This compact representation still
describes the complete visible text grid: blank cells are implied, not omitted
from comparison. The checker expands runs before validating coordinates.

Run the deterministic, dependency-free checker from this directory:

```bash
python3 check.py
```

The reproducible minimal capture command is:

```bash
sh capture-minimal.sh /tmp/tea-fx-minimal-captures
```

It builds no provider connection, launches the pinned binary in a private tmux
PTY at 80×24 and 120×40, records startup with no input, `/help` followed by
Enter, and a leading `/`, then writes text grids and cursor coordinates. The
checked-in minimal fixtures were produced by this script with
`/Users/josh/d/fx/zig-out/bin/fx` at commit `83a059c` (binary version `0.0.4`).
The capture path does not record a replay tape because Bun is not required for
the direct PTY capture; the exact input sequence and binary hash are in
`manifest.json`.

## Evidence status

The two legacy-mode cases are frames 8 and 97 from the checked-in replay archive
`/Users/josh/d/fx/tests/e2e/fixtures/fx-render-bug-20260510-075848.tar.gz`.
They are the read-only-tools replay exercised by `fx/tests/e2e/tui-render-replay.test.ts`,
at 167×46, with the `legacy` presentation setting used by that capture. They
are useful as a provenance-backed harness seed, not as the planned default
minimal-mode parity matrix.

The six `minimal-*` cases are fresh captures of the planned presentation mode:
startup, help, and leading-slash menu at both requested sizes. They capture
actual minimal geometry; they do not imply that other states or dimensions have
been captured.

The full minimal-mode matrix (startup, composer, transcript, tools, queues,
menus, temporary surfaces, resize, cancellation, failure, exit, and tiny
terminals) remains planned. New captures should add a manifest case, exact input
or replay provenance, and a normalized cell fixture together.
