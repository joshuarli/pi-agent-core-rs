# `pi-agent` TUI implementation plan

This is the delivery plan for the repository-owned terminal host in
[`crates/pi-agent-tui`](crates/pi-agent-tui). It turns the parity audit against
`~/d/pi` (`v0.83.0`) into independently reviewable coding-agent tranches.

The durable boundary is still [`docs/tui.md`](docs/tui.md): Rust core owns agent
semantics, provider adapters, queues, cancellation, accounting, and compaction;
the TUI owns terminal input, event projection, and presentation. This plan must
not turn the host into a second runtime, session system, or ambient Pi
configuration layer.

## Decision summary

The first useful TUI is not a feature-by-feature clone of Pi. It is a reliable
coding loop that makes the core's actual state visible.

| Priority | Keep in the near-term plan | Why |
| --- | --- | --- |
| **P0 — core/critical** | End-to-end response streaming; live assistant/tool lifecycle; cancellation and error visibility; persistent provider-reported usage/cache/cost; minimal Markdown/code/error rendering; steering/follow-up queue semantics; terminal correctness | Without these, a run looks stalled, accounting is hidden, prompts can be lost, or the terminal can be left unusable. |
| **P0/P1 — critical for long sessions** | Context indication and overflow guardrails are P0; the TUI's provider-backed automatic compaction and retry are enabled for OpenRouter catalog models with source-backed capacity | A short demo can work without this; a coding session cannot safely grow without it. |
| **P1 — valuable after the loop** | Persistent linear sessions/resume, search/copy, richer selectors, retry/recovery affordances, approved local estimates | These improve continuity and throughput but do not fix the basic live-run contract. |
| **P2 — defer** | Advanced terminal polish, image paste, fork/clone/tree, export/import/share, hotkeys beyond `Ctrl+G`, broad provider coverage and dynamic catalogs | They are substantial surfaces with separate contracts and are not required to make the current host useful. |

### Minimum pleasant release

The first release stops at a small, coherent surface:

- One linear transcript, one composer, and one compact status/footer region.
- Real incremental response delivery through one reference provider path, with
  generic tool lifecycle, cancellation, and error visibility.
- Basic block/code/error rendering, not a full document renderer.
- `Ctrl+G` → `$EDITOR`, `Ctrl+C`, one discoverable queue action, and the
  cross-provider `/model` selector.
- Provider-reported usage, cache read/write, exact cost, and known context
  status; unknown values remain unknown.

Persistence, configuration, extensions, themes, rich editor behavior, broad
provider coverage, and multiple panes are deliberately outside this release
cut. A tranche is not complete because it can accommodate one more feature; it
is complete when this surface is responsive, legible, and dependable.

### The streaming blocker

Streaming is a core/provider work item, not only a renderer task. The TUI
already appends `MessageUpdate` deltas in
[`state.rs`](crates/pi-agent-tui/src/app/state.rs), but the built-in HTTP
transport reads the response body to EOF before returning a stream
([`http.rs`](crates/pi-agent-core/src/provider/http.rs)). OpenRouter and
Command Code construct their finite streams after `complete()` returns, and
the local payload explicitly requests `"stream": false`.

The implementation must therefore establish a genuinely incremental transport
boundary and preserve ordered core events through the host. A renderer-only
spinner or fake chunking is not a completion of this tranche.

For visual and interaction quality, use `~/d/ds4` as the priority reference;
use `~/d/pi` primarily as a feature-gap reference. The relevant DS4 examples
are the small live REPL and token presentation in `~/d/ds4/ds4_cli.c`, plus the
interactive acceptance checks in `~/d/ds4/QA_BEFORE_RELEASES.md` (status bar,
queued prompts, interruption recovery, readable code output, and terminal
flicker). Borrow its restraint, feedback, and recovery behavior—not its session,
web-tool, split-screen, or broader product surface.

### Current implementation anchors

Use these as the first reading list for an implementation tranche:

- Transport and adapters: [`provider/http.rs`](crates/pi-agent-core/src/provider/http.rs),
  [`openrouter/mod.rs`](crates/pi-agent-core/src/provider/openrouter/mod.rs),
  [`commandcode/mod.rs`](crates/pi-agent-core/src/provider/commandcode/mod.rs),
  and [`local/payload.rs`](crates/pi-agent-core/src/provider/local/payload.rs).
- Event projection and frame delivery: [`app/state.rs`](crates/pi-agent-tui/src/app/state.rs),
  [`app/runtime.rs`](crates/pi-agent-tui/src/app/runtime.rs), and
  [`render.rs`](crates/pi-agent-tui/src/render.rs).
- Accounting presentation: [`state/accounting.rs`](crates/pi-agent-core/src/state/accounting.rs),
  [`app/support.rs`](crates/pi-agent-tui/src/app/support.rs), and
  [`app/input.rs`](crates/pi-agent-tui/src/app/input.rs).
- Queue and compaction contracts: [`agent/mod.rs`](crates/pi-agent-core/src/agent/mod.rs),
  [`app/host.rs`](crates/pi-agent-tui/src/app/host.rs), and
  [`provider/registry/catalog.rs`](crates/pi-agent-core/src/provider/registry/catalog.rs).
- Tool output boundary: [`local_operations.rs`](crates/pi-agent-core/src/tools/local_operations.rs).

## Delivery rules

- Each tranche has a narrow contract, focused tests, and an observable exit
  criterion. Keep unrelated parity features out of a tranche.
- Anything requiring new persistence, ambient discovery, new authority, or a
  new policy/runtime layer stays out of the initial plan until that contract is
  separately approved.
- Preserve the typed core event boundary. The TUI may project events but must
  not infer conversation state from rendered text or maintain a shadow agent
  loop.
- Unknown provider fields stay unknown. Never invent a price table, cache value,
  context window, or model capability in the host.
- Prefer a small local implementation and existing core APIs. A dependency or
  new persisted format needs a separate contract review.
- Keep provider-agnostic behavior in core and provider-specific details in
  adapters. In particular, do not solve transport blocking by coupling the core
  to a Tokio- or widget-specific runtime.
- The normal review unit is one tranche: implementation, regression tests,
  documentation, and a short verification note.

## Headless PTY harness (cross-cutting P0 infrastructure)

Create the headless pseudo-terminal harness in Tranche 0 and maintain it for
the entire plan. It is the behavioral judge for the host boundary; unit tests
alone cannot prove that raw mode, alternate-screen state, input bytes, redraws,
and visible status work together.

The harness should:

- Launch the real `pi-agent` binary in a controlled pseudo-terminal with a
  deterministic terminal size and environment.
- Feed key bytes and paste/editor inputs, including `Ctrl+C`, `Ctrl+G`, queue
  actions, resize events, and exit sequences.
- Drive fake or fixture-backed provider/tool events, including delayed response
  chunks, tool progress, errors, cancellation, accounting, and compaction
  outcomes without network credentials.
- Capture output and process/lifecycle state so tests can assert observable
  behavior: incremental text before settlement, footer values, queued prompts,
  error notices, redraw behavior, and terminal restoration.

Keep the harness deterministic, test-only, and narrowly scoped. The fixture
provider/transport layer and the real-binary PTY layer are two layers of one
maintained test system, not two independent harnesses. Assert semantic markers
and lifecycle outcomes by default; do not make full-screen snapshots or a
general terminal framework part of the product. Any PTY library dependency
requires the same dependency review as other TUI dependencies, and the harness
must not add a production test mode or hide a real transport failure behind a
mock-only test.

Every subsequent tranche must add or update at least one PTY scenario when it
changes interactive behavior. The harness is maintained infrastructure, not a
temporary bootstrap test to be discarded after Tranche 0.

## Tranche 0 — Contract and harness (P0 prerequisite)

Make the live-run behavior testable before adding presentation features.

### Work

1. Write/retain deterministic event fixtures covering message start/update/end,
   thinking deltas, tool start/update/end, retry, abort, error, accounting, and
   compaction events.
2. Specify the projection invariants for `AppState`: ordered deltas, one settled
   assistant message, no duplicate tool results, explicit unknown accounting,
   and terminal-safe transitions.
3. Create the test-only headless PTY harness and its fixture provider/transport
   layer. Keep the two layers as one maintained system, independent of network
   credentials, and include a delayed-chunk scenario that proves an update is
   visible before the request settles.
4. Define basic terminal correctness separately from terminal polish: raw and
   alternate-screen restoration, resize safety, cursor/input integrity, and
   `Ctrl+C`/`Ctrl+G` behavior.
5. Record the current limitations of local shell output: the default local
   operation currently emits output after process completion, so live stdout is
   a separate event-contract change.

### Exit criteria

- A focused test detects and rejects provider buffering until EOF.
- Event projection and accounting tests distinguish unknown from zero.
- The real binary can be driven headlessly through a PTY and proves incremental
  output, input handling, cancellation, and terminal restoration.
- Resize, input, cancellation, and editor transitions do not corrupt the
  terminal or leave raw/alternate-screen mode enabled.
- Cancellation, provider failure, editor failure, and ordinary exit all have a
  terminal-restoration assertion where a PTY test is warranted.

## Tranche 1 — End-to-end live response (P0 / core-critical)

Deliver the moment-to-moment interaction that users expect from a coding agent.

### Work

1. Replace the collect-then-stream path with an incremental transport contract
   for adapters that support streaming. Preserve event order and final usage.
2. Prove the contract end-to-end through one reference provider path and its
   fixture-backed PTY scenario. Adapt OpenRouter, Command Code, and local
   requests afterward as their APIs support it; do not make broad provider
   coverage a gate for this tranche.
3. Ensure the host's event pump can redraw while a provider is producing data;
   synchronous transport work must not starve terminal input and frame updates.
4. Render assistant text incrementally, including a clear working state and
   thinking visibility when the core reports it.
5. Render tool lifecycle states as a grouped, readable activity item: started,
   updating, settled, cancelled, and failed. Keep generic rendering for unknown
   tools.
6. Make `Ctrl+C` cancellation, provider errors, retries, and aborted turns
   visible and unambiguous. Preserve the existing structured cancellation
   semantics.

### Exit criteria

- A delayed multi-chunk fixture visibly produces several assistant updates
  before completion.
- A tool run shows start and settlement without requiring serialized event dumps.
- Input remains responsive during a live response, and `Ctrl+C` settles cleanly.
- No renderer or TUI test fabricates streaming that adapters did not provide.

## Tranche 2 — Minimal readability and persistent telemetry (P0 / core-critical)

Make the live state legible without opening a command or losing the transcript.

### Work

1. Add bounded block rendering for plain text, basic Markdown, code blocks, and
   errors. A small local renderer is sufficient; full Markdown parsing,
   syntax-highlighting frameworks, and terminal-protocol work are later
   presentation features.
2. Add minimal generic tool presentation for start, progress, result, and error,
   while retaining a safe fallback for unknown tools. Rich type-specific cards,
   diffs, expansion, and image results are deferred.
3. Add a persistent compact footer/status region showing, when reported:
   provider/model, run state, input/output/reasoning tokens, cache-read and
   cache-write tokens, exact cost, and context estimate/capacity. Keep it to one
   compact status line or a small fixed two-line region, never a dashboard.
4. Derive cache-hit information only from available counters. Label every
   unavailable field as unknown and keep `/cost` as the detailed view.
5. Keep the status projection event-derived and recoverable from a snapshot;
   do not duplicate accounting or context policy in the renderer.

### Exit criteria

- A user can see an active stream, current model, and aggregate reported
  accounting without typing `/cost`.
- Cache read/write and cost remain visibly distinct; missing provider data is
  not rendered as zero.
- Basic Markdown, code blocks, and tool errors are readable in a narrow
  terminal and remain stable under resize/scroll tests.

## Tranche 3 — Reliable coding loop (P0, with P1 follow-on)

Remove the interaction traps that interrupt a real edit/run/review cycle.

### Work

1. Expose both core queues: steering for the current turn and follow-up for the
   next turn. Show queued prompts, their delivery mode, and their eventual
   transcript placement. The exact Pi hotkey is optional; a discoverable command
   or minimal key is sufficient initially.
2. Keep `Ctrl+G` → `$EDITOR` as the supported large-edit path. The native
   composer now covers multiline paste/Shift+Enter, history, word motions, and
   command completion; kill/yank and grapheme-perfect cursor movement remain
   later editor work.
3. Make active/idle command behavior explicit: no accidental `/clear`, model
   replacement, or quit during a running operation; explain why an operation is
   unavailable.
4. **P0:** Add context status and an overflow guardrail. The host installs a
   provider-backed compactor for OpenRouter catalog models with source-backed
   capacity; core performs threshold compaction and preserves the atomic
   transaction. Models without capacity remain explicitly unavailable rather
   than receiving an invented summary budget.
5. Cover the common error path: preserve the prompt when an editor or provider
   fails, restore input after cancellation, and make retry/re-submit behavior
   explicit.

### Exit criteria

- A prompt submitted during a run is either visibly steered or visibly queued;
  it is never silently dropped.
- A long context cannot fail mysteriously: the user sees capacity status and an
  explicit compaction/unavailable decision before overflow.
- Automatic compaction is available for documented OpenRouter catalog models;
  custom models without capacity remain manual-only.
- The core remains the source of queue and compaction truth.

## Tranche 4 — Continuity and bounded productivity (P1 / deferred)

Add features that make repeated work pleasant after the live loop is solid.

### Candidate work

- Persist and resume **linear** sessions with an explicit, versioned privacy and
  redaction contract; add `/new` and a minimal session picker.
- Full Markdown parsing, syntax highlighting, type-specific tool cards,
  expandable diffs, and richer tool-output presentation.

### Exit criteria

- Session files and resume behavior have documented ownership, location, schema,
  redaction, and failure recovery. This is not an ambient `$HOME` discovery
  feature.
- Productivity additions do not alter core event ordering or queue semantics.

## Tranche 5 — Provider integrations (P2 / deferred)

Treat ecosystem breadth as a later host integration project, not a prerequisite
for the first usable TUI.

### Candidate work

- Additional provider adapters, authorized catalog refresh, model metadata, and
  explicit user-managed authentication/login flows.

Every item here needs explicit provider authority, credential handling,
cache/staleness rules, and tests before implementation. No item is required to
close Tranches 1–3. Ambient credential discovery or a persistent credential
store is not implied by this tranche.

## Explicitly deferred for now

These are reasonable future host work, but should not block the critical loop:

- Terminal polish: full grapheme/wide-cell/IME handling, Kitty keyboard
  protocols, mouse interaction, hardware cursor fidelity, advanced copy/search,
  and elaborate scrolling/layout effects.
- Image paste, drag/drop, image rendering protocols, and multimodal attachment
  UX.
- Fork, clone, session trees/branches, bookmarks, and tree navigation.
- Export, import, share, and other session interchange formats.
- Hotkeys beyond `Ctrl+G` → `$EDITOR` (plus only the minimum discoverable queue
  action needed by Tranche 3).
- Broad provider coverage, remote model discovery, OAuth/subscription flows,
  and dynamic pricing catalogs.
- Full native editor parity: multiline buffer, history/undo system,
  kill/yank, path/fuzzy completion, and paste markers.
- Live shell stdout streaming until the local tool event contract can emit
  incremental chunks; tool lifecycle visibility remains P0.

## Out of scope unless a separate proposal changes the contract

The following are deliberately outside this host's current authority and
boundary:

- Subagents, worktrees, plan-mode UX, or a second orchestration loop.
- Approval, sandbox, policy-bypass, or project-trust UI that grants authority
  not already supplied by the embedding application.
- MCP server management, browser/web-search UI, or ambient network tools.
- Automatic Pi installation/config/session discovery; ambient loading of
  `AGENTS.md`, `.pi` settings, prompt templates, skills, or sessions; package or
  marketplace discovery; or a hidden global extension registry.
- A TUI extension/runtime layer: Luau-defined commands, custom widgets, plugin
  discovery, or policy callbacks that can change authority or orchestration.
- Theme files, configurable keybindings, theme hot reload, and broad command
  customization. Built-in key additions remain possible; a configuration
  system does not.
- Credential stores, ambient login, or automatic OAuth/subscription discovery.
- Git dashboards, task boards, tabs, multiple chat panes, and general terminal
  framework behavior.
- An embedded full-screen editor or a persistent shell pane; `Ctrl+G` remains an
  explicit external-editor escape hatch.

## Verification and handoff

Use the pinned nightly toolchain and the narrowest useful checks for each
tranche. Retain both focused unit/event tests and the maintained headless PTY
harness. At minimum, cover event projection, delayed streaming, tool lifecycle,
queue delivery, unknown-versus-zero accounting, resize/scroll behavior,
compaction failure atomicity, and terminal restoration. Record provider
limitations rather than masking them. Do not run formatters, linters, pre-commit
hooks, or pushes as part of this plan.

The minimum pleasant release is complete when Tranches 0–2 and the P0 portion
of Tranche 3 produce a responsive, honest, readable coding loop. The P1 portion
of Tranche 3 and Tranches 4–5 remain optional follow-on work; the out-of-scope
items require a new authority and contract decision before they are scheduled.
