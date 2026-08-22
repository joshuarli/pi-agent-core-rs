# Recover `fx` terminal parity as an executable contract

## Decision and current baseline

`e335cd0` is not an `fx`-like TUI implementation. It added partial scaffolding—`FrameLayout`, `VisualLayout`, `Theme`, `TranscriptEntry`, and `UiSurface`—without routing the live terminal through those abstractions. Treat the current renderer as the legacy baseline, not a visual foundation to preserve.

The bounded objective is:

> Make tea's interactive terminal feel like the default `fx` minimal transcript/composer experience, while retaining tea's event model, provider/model registry, session format, accounting semantics, compaction rules, and reduced command surface.

`fx` is a behavior and visual oracle, never code to port. Tea must not acquire its permissions, settings, skills, subagents, tabs, images, web UI, persisted themes, or raw-ANSI renderer.

The oracle is `/Users/josh/d/fx` at `83a059c643cfe911db470a7c6c1dbc8fdb61de8a`, using the default dark **minimal** presentation mode. This correction matters: minimal fx uses a plain `┃ ` composer rail and no idle top/bottom divider. Fresh 80×24 and 120×40 captures place startup welcome at row 0, the composer at row 2, and the status below it at row 4; it is a top-to-bottom flow, not a bottom-anchored footer. `❯ ` and tinted composer/message cards are legacy/normal-mode behavior, not the default target.

## Why the previous plan failed

The previous plan named the right kinds of components, but its milestones and tests allowed their existence to stand in for their integration.

| Claimed capability | What HEAD actually does | Why it remains visibly different |
| --- | --- | --- |
| `fx` compatibility oracle | [`docs/fx-ui-compatibility.md`](docs/fx-ui-compatibility.md) names states and dimensions, but no captures, normalizer, or oracle test exists. | Nothing fails when tea drifts from fx. |
| Measured frame | [`ui/frame_layout.rs`](crates/tea-agent/src/ui/frame_layout.rs) is reached only by `measured_frame_layout`; [`render()`](crates/tea-agent/src/render.rs) still calls legacy `layout_for`. | The live frame remains header → composer → transcript → status instead of fx's solved transcript → activity → composer → status flow. |
| Typed entries | [`TranscriptEntry`](crates/tea-agent/src/app/state.rs) is rebuilt by parsing legacy preformatted `TranscriptLine` strings; the renderer consumes the strings. | Typed data is not the paint source, so text prefixes and one-off rows still define the UI. |
| Composer geometry | [`ui/visual_layout.rs`](crates/tea-agent/src/ui/visual_layout.rs) has isolated tests only; rendering and cursor placement use separate hand-written helpers. | Soft wraps, visual-row movement, cursor placement, and painted rows can disagree. |
| Theme | [`ui/theme.rs`](crates/tea-agent/src/ui/theme.rs) is unused; the renderer hard-codes colors. | Tea has no coherent or testable fx-style style hierarchy. |
| Temporary surfaces | `/help` and `/cost` set `UiSurface` but append persistent transcript text. Only the old picker overlay is painted. | Escape changes an enum, not a dedicated screen. |
| Activity and tools | `⏺ Asking` is footer text, queues/compaction are transcript strings, and tool details default expanded. | It does not use fx's transient activity and compact tool-group language. |
| Verification | The two PTY tests mainly use `screen.contains(...)`; one still accepts `𝒑i-agent` because tea says “`𝒑i-agent compatible`.” | Passing tests cannot catch wrong cells, layout, cursor, style, or tea identity. |

The plan also assumed the wrong current fx behavior. Ctrl+O opens/closes fx's full transcript/detail viewer; it is not a per-tool expand toggle. `• Thinking` and connected tool markers are the relevant minimal-mode activity language, not a permanent `⏺ Asking` row. Ctrl+End follow mode may remain a tea convenience, but it is not an fx parity requirement.

From here, a helper, enum, or module is not a milestone until a deterministic `Grid` frame and any necessary PTY interaction prove that the live terminal uses it.

## Constraints and semantic substitutions

- Keep `tea-core` and `tea-protocol` unchanged unless a focused failing test proves the existing typed event stream cannot describe the projection.
- Keep the direct `Grid`/`FrameDiff`/Crossterm substrate. Do not add a widget, command, or terminal framework—or any dependency—without approval.
- Brand the welcome state as tea. Remove the “`𝒑i-agent compatible`” wording and stop testing for `𝒑i-agent`.
- Keep tea's provider/model, context, accounting, session, and queue terms. Provider-reported values stay unknown when unknown; tea never invents prices or token values.
- Normalize cells as `(row, column, character, foreground, background, attributes)`, not raw ANSI byte streams.
- A fixture may substitute tea identity, tea command names, and tea accounting fields for corresponding fx cells only when that substitution is named in the parity manifest. Geometry, style role, spacing, cursor, and interaction affordance remain reference-backed.
- Omitted fx behavior must be named as omitted; “fx-like” must never silently imply settings, skills (`$`), files (`@`), permissions, or subagents.

## Phase 0 — use fx's existing render evidence to create a real oracle

Do not invent a second capture system before inspecting and reusing fx's existing evidence:

- `tests/e2e/tui-render-lab.test.ts` and `tests/e2e/render-lab/` provide tape, native/virtual-terminal, evidence, and report machinery.
- `tests/e2e/tui-render-replay.test.ts` and `tests/e2e/tui-render-assertions.ts` provide replay and virtual-screen assertions.
- `tui-startup`, `tui-input-navigation`, `tui-composer-edit-contracts`, `tui-slash-menu`, `tui-resize`, `tui-cost`, and `tui-full-transcript-brutal` identify deterministic inputs for target states.

Build and drive `/Users/josh/d/fx/zig-out/bin/fx` from the pinned checkout; never invoke `fx` from `PATH`. Reuse its tapes or virtual-screen evidence where possible, and write a small tea-owned adapter only for exporting normalized cells. Do not copy its renderer or make tea's tests depend on a live sibling checkout.

Render Lab's default pass/fail evidence is byte replay plus terminal-owned text/grid state; it deliberately does not prove font shaping, final cursor paint, or color. Use it for deterministic inputs, event boundaries, resize, scrollback, and grid structure. For the required foreground/background/attribute cells, the tea-owned capture adapter must derive bounded style evidence from the recorded terminal stream or another reproducible virtual-terminal layer—never screenshots alone.

Check in the resulting evidence, including the exact input/replay sequence:

```text
crates/tea-agent/fixtures/fx-ui/
  README.md
  manifest.json
  fx/<state>-<columns>x<rows>.cells.json
  tea/<state>-<columns>x<rows>.cells.json
```

`manifest.json` records the fx commit, presentation mode, source tape/test, terminal size, capture checksum, and allowed tea substitutions. The tea frames are a reviewed projection target, not a byte-for-byte copy of an application with a different semantic surface.

Capture default minimal-mode states at 120×40, 80×24, 40×10, and supported tiny sizes (0×0, 1×1, and 2×2 where the host permits them): startup; empty, typed, and multiline composer; submitted user prompt; streaming assistant; active/completed tool; queue; leading-slash menu; help; model picker; session picker; cost; scroll/detail viewer; resize; cancellation; provider failure; and exit. Resize must record consecutive frames so stale cells are observable.

Update [`docs/fx-ui-compatibility.md`](docs/fx-ui-compatibility.md) only after the captures exist. It currently describes planned evidence as if it were captured evidence.

**Gate:** a claimed reference state is incomplete until its manifest entry, normalized cells, and replay inputs exist. Documentation by itself is not an oracle.

## Phase 1 — establish one live presentation seam

First add a focused renderer regression that fails on HEAD. It must assert the captured 80×24 idle minimal frame and thereby reject the old `header + composer + transcript + status` layout. Replace the existing `empty_composer_starts_at_the_top_of_the_frame` test; it codifies the mismatch.

Then establish exactly one live path:

```text
lossless AgentEvent
        │
        ▼
typed AppState presentation projection
        │
        ▼
FrameLayout + VisualLayout + Theme + UiSurface
        │
        ▼
Grid<Cell> + cursor target
        │
        ▼
FrameDiff + TerminalGuard
```

### Typed projection

- Make `TranscriptEntry::{Welcome, User, Assistant, Tool, Notice, Error}` the canonical renderer input. Retain raw event-derived content and lifecycle state; do not recover it by stripping `"you: "` and `"assistant: "`.
- Keep generic `ToolProjection` keyed by core call ID with arguments, progress, settlement, lifecycle, and display state. It is a read-only projection; core remains authoritative.
- Keep queues, transient activity, accounting/context footer data, and active surface as separate typed projection state. Do not persist them as transcript strings.
- Remove legacy adapters once the renderer no longer needs them. There must be one paint source, not typed state plus a parallel string transcript.

### Frame and status flow

- Make `render()`, `transcript_metrics`, `composer_cursor_position`, and redraw all consume the same `FrameLayout`. Retire the old `Layout` and `layout_for` path; an unused public helper does not count as integration.
- Follow the captured fx **minimal** frame rather than imposing generic dividers or an always-bottom-fixed footer. A short transcript flows from row zero into a blank row, composer, blank row, and status; once content fills the terminal, only the transcript scrolls behind the preserved input/affordance rows. The planner solves transient activity/queue rows, multiline composer window, inline menu, hint/status row, and modal surface reservation as one frame.
- In minimal idle state, paint the captured `┃ ` rail and hint hierarchy with no invented divider. Use dividers only in reference-backed picker/modal states.
- Welcome is tea-branded transcript content and scrolls with the transcript; it is not a permanent header.
- Establish and test the row-priority policy for tiny terminals: preserve the cursor-containing composer and essential submit/cancel affordance before optional activity, menu, or telemetry rows.
- A resize/shrink blanks every cell no longer owned by the new frame. Retain the failed-flush invalidation rule and test it at the boundary that owns it.

### Composer geometry and style

- `VisualLayout` becomes the single authority for painted rows, visible window, cursor location, and Up/Down visual-row movement. Remove duplicate wrapping and cursor calculations from `render.rs`.
- Match the captured prefix/continuation treatment, two-cell prefix budget, hidden-above indicator, hard newlines, word-aware soft wrapping, wide and combining Unicode, and absolute tab stops where the reduced tea composer supports them.
- Preserve tea's native controls—history, word motion, Shift+Enter, bracketed multiline paste, `$EDITOR`, PageUp/PageDown, and Ctrl+End if retained—but label Ctrl+End as tea-specific and test it independently of the fx oracle.
- Route every rendered style through `Theme` roles. Default dark minimal is the parity target; a light palette remains process-local only if it is separately tested. Remove hard-coded renderer colors once roles exist.

**Gate:** rendering a typed assistant entry, a compact tool entry, and a multiline composer through `render()` must demonstrably exercise `FrameLayout`, `VisualLayout`, `Theme`, and typed projection in cells and cursor coordinates. Isolated helper tests are necessary but insufficient.

## Phase 2 — implement the reduced parity map in reference order

Add the failing normalized-grid fixture before each behavior change. Do not build later surfaces on the legacy renderer.

| Surface | Required tea behavior | Acceptance evidence |
| --- | --- | --- |
| Welcome and idle | Tea identity/version and `/help` hint in transcript; minimal `┃` composer and tea footer values. | Exact 120×40 and 80×24 frames; no `𝒑i-agent` copy. |
| Composer | Captured rail/continuation chrome, wrapping, visible window, and cursor. | Exact frames/cursor for ASCII, wide/combining text, hard newline, soft wrap, tabs if supported, and tiny terminals. |
| Transcript | Connected bold user rail, readable streaming Markdown, and captured spacing. | Submitted and streaming frames; first token mutates a body cell before settlement. |
| Activity and tools | `• Thinking`/reference-derived transient activity and generic compact tool groups with connected result rows. | Started/progress/success/error frames. No type-specific tool widgets. |
| Full detail | Ctrl+O opens/closes a tea reduced full-transcript/detail viewer; tool detail is reviewed there. | Open, scroll, close, and return-to-live frames. |
| Queues and scrolling | Collapsed queue/activity treatment at the captured boundary; independent transcript scroll/follow. | Queue, PageUp/PageDown, new-output, and tea-specific follow-mode frames. |
| Slash completion | Leading `/` opens the reduced tea command catalog in the captured inline/footer location. Where tea supports inline slash completion, it follows fx's cursor-local behavior; `$` and `@` remain excluded. | Exact menu geometry plus PTY Up/Down, Tab, Enter, Escape. |
| Help/model/session/cost | Real temporary surfaces. Help/models/resume follow the captured full-screen surface shape; cost uses tea accounting and renders missing values as `unknown`. | Exact surface frame before and after Escape; no transcript-only stand-in. |
| Failure and exit | Reference-shaped activity/error treatment, tea's explicit prompt restoration, and terminal restoration. | PTY frames for cancellation/failure and restored terminal modes. |

Tool groups begin compact. Any retained per-tool expansion needs an explicit focus model and separate contract; it must not reuse Ctrl+O's fx full-detail meaning. `UiSurface` must select a renderer branch: setting the enum and appending transcript text is explicitly not a surface implementation.

## Phase 3 — verification that catches a visual regression

### Deterministic grid fixtures

- Add a tea-agent harness that creates deterministic presentation fixtures, renders to `Grid`, normalizes cells, and checks text, row/column placement, style role, and cursor target against the tea fixture.
- Run every oracle state at the four reference sizes and supported tiny sizes. Assert that the consecutive frame after resize/shrink blanks stale cells.
- Retain focused state tests for event-to-entry projection, streaming update coalescing, tool grouping, queue display, unknown-versus-zero accounting, command filtering, surface transitions, and flush-failure invalidation.
- Fixture names, state names, test names, and manifest entries use the same parity-map vocabulary so every visual claim is searchable to its evidence.

### PTY interactions

Keep `screen.contains(...)` only for asynchronous readiness. Once a state is settled, assert virtual-screen cells/cursor against its fixture. Extend `crates/tea-agent/tests/pty_streaming.rs` (or split by behavior) for:

- first streamed token before provider settlement;
- leading slash-menu navigation, acceptance, and Escape;
- help, model, session, cost, and full-detail entry/exit;
- soft-wrap cursor navigation and bracketed paste;
- compact-tool/full-detail behavior;
- resize while streaming and stale-cell clearing;
- cancellation, provider failure, editor failure, ordinary exit, and terminal restoration.

Run focused checks first, then broaden after the matching fixture passes:

```bash
cargo +nightly-2026-07-24 test -p tea-agent
cargo +nightly-2026-07-24 test -p tea-agent --features pty-harness --test pty_streaming
cargo +nightly-2026-07-24 test --workspace
git diff --check
```

Do not run formatters, linters, pre-commit hooks, or push a remote as part of this work.

## Phase 4 — close the contract only after the screen matches

After every parity-map row has its normalized tea fixture and necessary PTY case, update `docs/tui.md`, `docs/fx-ui-compatibility.md`, and `docs/verification.md` with final ownership boundaries, capture paths, and non-goals. Remove stale fixed-layout language.

The work is not complete because `ui/` contains intended module names, an enum transitions, or broad tests pass. It is complete when the tea binary renders the reviewed fixture states through the one live presentation seam, and a return to the old string-line/top-composer UI would fail visibly and deterministically.
