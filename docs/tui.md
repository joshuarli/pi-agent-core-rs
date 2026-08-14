# `pi-agent` terminal host

`pi-agent` is the small interactive terminal host in
[`crates/pi-agent-tui`](../crates/pi-agent-tui). It is a control surface and
projection for `pi-agent-core`, not a second agent runtime, session manager, or
ambient Pi installation.

The core continues to own conversation state, model streaming, tool scheduling,
queues, cancellation, accounting, compaction transactions, and lifecycle
ordering. The host owns only terminal interaction, explicit startup inputs,
provider credentials, and rendering.

| Core contract | Terminal responsibility |
| --- | --- |
| Agent state machine and typed events | Lossless event-derived transcript and status projection |
| Compiled provider/model registry | Picker and explicit adapter configuration |
| Provider-reported accounting and compaction lifecycle | Concise status and direct commands |
| Prompts, steering queues, and structured cancellation | Keyboard dispatch and command parsing |

The core event boundary contains no ANSI, layout, or terminal concepts. The
TUI must not infer state from terminal output or keep a shadow conversation.

## V0 product boundary

One invocation hosts one linear, in-memory coding conversation in an explicit
workspace. It can select a compiled provider/model, run the pinned coding
profile, render the lossless core event stream, submit or steer prompts, cancel
work, show generic tool activity, display provider-reported accounting, invoke
manual compaction when configured, and edit the prompt in `$EDITOR`.

It does not persist sessions, discover a Pi installation, read a TUI
configuration file, or become a general terminal framework. The normal screen
is intentionally only a transcript, one-line composer, and status line; a
picker is a temporary overlay.

## Ownership and explicit inputs

The command line is deliberately narrow:

```text
pi-agent [--provider <id>] [--model <id>] [--cwd <path>]
```

`--cwd` is the explicit workspace authority passed to the default coding-tool
bundle. With no provider/model pair, the host opens the compiled picker rather
than guessing a model. The host may read documented credential environment
variables, but it must never log them or move credential discovery into core.
No preferences, keys, model choices, themes, keymaps, or sessions are
persisted.

For a reproducible provider check without terminal state, use the headless
probe. It assembles the same default profile and OpenAI-compatible context
hook as `pi-agent`, then drives one OpenRouter prompt and prints the assistant
text. The probe does not read `.env`; source that file explicitly at the shell
boundary:

```bash
set -a
. ./.env
set +a
make tui-smoke
```

Provider failures are returned as a non-zero process result with their local
error classification, which makes this a useful first check before debugging
terminal rendering or input handling.

The registry is checked-in, feature-selected Rust data. It exposes stable
provider/model identifiers, optional source-backed context capacity, explicit
custom-model resolution, configuration family, and provider capabilities. It
does not read the environment, a credential file, a workspace, or a remote
catalog. Remote discovery is a future host capability, not a core fallback.

Provider/model replacement uses `Agent::replace_model_provider` only while the
agent is idle. It preserves the existing prompt, tools, queues, and retained
conversation; an active or cancelling agent is left unchanged. The picker must
not replace an adapter beneath a running operation.

## Event projection and rendering

The host subscribes through `Agent::subscribe_lossless`. That channel is
ordered and has no capacity-based drop path, but unread events consume
caller-owned memory until drained or dropped. `AgentSnapshot` is for recovery
and inspection; it is not the usual transcript source.

```text
AgentEvent -> AppState projection -> Grid<Cell> -> frame diff -> Crossterm
```

`AppState` may contain viewport position, composer text/cursor, picker state,
status presentation, and transient notices. It must not own model, provider,
accounting, tool, compaction, or conversation semantics.

The v0 renderer displays raw Markdown with simple wrapping and incremental
assistant text. It renders tool calls generically from their name, serialized
arguments, updates, and settled result/error rather than type-specific widgets.
`PageUp`, `PageDown`, and `End` provide basic scroll/follow behavior. Markdown
parsing, syntax highlighting, rich text, and a general layout tree are not
required for this boundary.

The local terminal substrate is deliberately small: `Cell`, `Style`, `Rect`,
`Grid`, previous/current frame comparison, and direct Crossterm flushing. A
failed flush must invalidate the previous frame so the next draw repaints it.
Unicode width support is admitted only after a focused rendering/cursor test
demonstrates that scalar-count placement is insufficient.

## Interaction and terminal lifecycle

The native composer supports insertion, left/right, Home/End, backspace/delete,
paste, submission, and `Ctrl+G`. It intentionally has no multiline editing,
history, word motions, or completion. `$EDITOR` supplies multiline editing:
the host creates a private temporary file, suspends raw/alternate-screen state,
parses and invokes `$EDITOR` without a shell, reads replacement text, restores
the terminal, and removes the file on recoverable paths. An editor failure
preserves the existing composer text.

`TerminalGuard` owns raw mode, alternate screen, cursor visibility, and
bracketed paste. Restoration is a correctness property for ordinary exit,
provider/tool/compaction failures, cancellation, and editor failures. A panic
path should restore terminal state before delegating to the prior panic hook
when practical.

The direct commands are intentionally not a plugin framework:

```text
/help      show keybindings and commands
/provider  open the compiled provider picker
/model     open the compiled model picker
/cost      print per-turn and aggregate reported accounting
/compact   invoke the configured manual compactor
/clear     reset the idle linear conversation
/quit      exit after structured cancellation and settlement
```

Pickers use arrow keys, literal substring filtering, Enter, and Escape. They
show only compiled static entries plus an explicit custom-model path. An
unavailable credential remains explainable rather than being guessed or
silently substituted.

Submitting text while idle starts a prompt. Submitting while active enqueues
steering, and the transcript waits for the core event that makes it visible.
`Ctrl+C` calls `Agent::abort` while active; it never kills the process to stop
inference. While idle it clears nonempty input or exits with an empty composer.
`/clear` refuses during an active run and never cancels or queues itself.

## Accounting and compaction

The status and `/cost` display only values reported by the provider: input,
output, reasoning, cache-read, cache-write, and exact decimal cost may each be
unknown. The host must not manufacture a pricing table, estimated cost,
context-token estimate, telemetry, or budget behavior.

Compaction changes retained context and therefore remains a core transaction.
The caller-supplied, cancellation-aware `Compactor` receives a versioned,
owned context and proposes replacement messages. Core validates message and
tool-call relationships, commits atomically on success, emits typed
`compaction_start`/`compaction_result`/`compaction_end` events, and leaves the
old conversation intact on failure, invalid output, or cancellation.

Manual compaction is idle-only. A provider with no documented concrete
compaction policy is reported unavailable; the TUI must not invent a summary
prompt or context budget.

## Dependency and architecture discipline

The application owns a Smol executor while `pi-agent-core` remains
executor-agnostic and Tokio-free. It uses direct Crossterm synchronous events
and direct rendering, not Ratatui or another widget/terminal abstraction. The
CLI uses `std::env::args_os()` rather than Clap, and local typed errors rather
than `anyhow`.

New dependencies begin rejected. Prefer a small local implementation when its
contract is clear and bounded. Before proposing an exception, establish that it
is needed for ordinary interactive use, cannot be clearly implemented locally,
adds less surface than it removes, and improves rather than weakens auditability.
Review `cargo tree` and `cargo tree -e features` before and after an accepted
dependency change. The current direct dependency evidence lives in
[`crates/pi-agent-tui/DEPENDENCY-REVIEW.md`](../crates/pi-agent-tui/DEPENDENCY-REVIEW.md).

Do not add an application state framework, command framework, terminal widget
system, configuration/session format, fuzzy matcher, clipboard layer, or
generic event bus merely to factor this small program.

## Post-V0 direction

Future work keeps the same typed core boundaries; it does not create a second
agent loop, hidden session store, or ambient configuration system.

| Area | Deferred work |
| --- | --- |
| Transcript | Markdown rendering, restrained highlighting, code-block horizontal handling, richer generic tool hints, search/copy, new-output markers, refined scrolling |
| Composer and commands | Multiline native editing, grapheme/wide-cell correctness, word movement, history, paste presentation, richer editor integration, completion, and a narrow explicit command registry |
| Extensions | Capability-scoped Luau-defined commands only; no automatic extension discovery, package manager, or marketplace |
| Providers | Authorized cached remote model discovery, catalog update tooling, richer metadata, and explicit user-managed authentication flows |
| Accounting | Budgets, warnings, richer history, and clearly labelled local estimates where they are ever justified |
| Context | Automatic compaction policy, context-window recovery, previews, and configurable compactor policies |
| Continuity | Persistent linear sessions, resume, import/export, and their separate privacy/redaction contract |

The following remain out of scope unless a separate proposal establishes their
authority and contract: session trees/branches/bookmarks, subagents or
worktrees, plan-mode UX, approval/sandbox/policy bypass UI, MCP management,
extension or package discovery, configuration/theme/keymap files, browser or
web-search UI, image paste/rendering, embedded editors, git UI, dashboards,
tabs, and multiple chat panes.

## Verification expectations

Use the pinned nightly toolchain. Core contract changes need focused tests and
the usual core documentation updates. TUI changes should cover deterministic
grid/diff behavior, raw event projection, composer/editor boundaries, picker
availability, active `/clear` refusal, accounting unknown-versus-zero handling,
and terminal restoration where a focused PTY test is necessary. Do not run
pre-commit hooks or push from this repository.
