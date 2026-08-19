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
is intentionally only a transcript, a compact multiline composer, and a
status line; a picker is a temporary overlay.

## Ownership and explicit inputs

The command line is deliberately narrow:

```text
pi-agent [--provider <id>] [--model <id>] [--local-base-url <url>]
         [--local-context-window <tokens>] [--cwd <path>]
pi-agent [-h | --help]
pi-agent --provider <id> --model <id> [--thinking <level>] -p <message>
```

`-h` and `--help` print the usage text and exit without starting the
terminal host. `-p`/`--prompt` runs one explicit prompt without terminal mode,
streams only assistant text to stdout, and exits; it requires both
`--provider` and `--model`. `--thinking` accepts `off`, `minimal`, `low`,
`medium`, `high`, `xhigh`, or `max`.

`--cwd` is the explicit workspace authority passed to the default coding-tool
bundle. With no provider/model pair, the host opens the cross-provider model
selector rather than guessing a model. The host may read documented credential
environment variables, but it must never log them or move credential discovery
into core.
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

The TUI also compiles the credential-free `local` provider for an oMLX server.
Select it explicitly with `--provider local`; the default model is
`Laguna-XS-2.1-5bit` and the default endpoint is
`http://127.0.0.1:8000/v1`:

```bash
cargo run -p pi-agent-tui --bin pi-agent -- --provider local --model Laguna-XS-2.1-5bit
```

For a nonstandard oMLX port, pass `--local-base-url`, for example
`http://127.0.0.1:12345/v1`. The repository `make local` target uses this
explicit endpoint option and supplies oMLX's default 32,768-token context
capacity; override it with `LOCAL_CONTEXT_WINDOW=<tokens>` when the server is
configured differently.

Local compaction uses the same OpenAI-compatible streaming endpoint as a normal
turn: the TUI asks the selected oMLX model for a tool-free summary, and core
commits the summary transaction. oMLX does not expose a separate compaction
endpoint. The checked-in Laguna model has a 32,768-token capacity and therefore
gets automatic compaction. For a custom local model, pass its effective server
capacity explicitly, for example `--local-context-window 32768`, to enable
automatic compaction; without that authority, manual `/compact` remains
available but automatic compaction stays disabled.

To download the configured Qwen3.5 4B MLX checkpoint, idempotently start oMLX
on port 12345, and launch the TUI against it, run:

```bash
make local
```

The target bootstraps `~/d/omlx/.venv` and installs the checkout in editable
mode with `uv` before downloading the model, so the first run does not require
a separate Python setup. Repeating the command safely reuses that environment.

Use `make local LOCAL_PI_ARGS="-p 'say hi'"` for a one-shot smoke test. The
target uses the working `hf` downloader shipped in the oMLX virtual environment;
newer `huggingface-cli` wrappers may only emit a deprecation error.

The adapter is configured without an API key or environment lookup. The command
will fail at the provider boundary until the local server is running; that is an
expected live-integration prerequisite, not a TUI setup failure.

The registry is checked-in, feature-selected Rust data. It exposes stable
provider/model identifiers, optional source-backed context capacity, explicit
custom-model resolution, configuration family, and provider capabilities. It
does not read the environment, a credential file, a workspace, or a remote
catalog. Built-in capacity values are synchronized from Pi's generated model
registry; custom-model selections remain capacity-unknown. Remote discovery is
a future host capability, not a core fallback.

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

The renderer displays incrementally delivered assistant text with bounded
plain-text, heading/list, fenced-code, error, and Unicode Markdown-table
treatment. Tables use box-drawing borders and display-width-aware cell
padding. It renders tool calls generically as one lifecycle activity from
their name, serialized arguments, progress, and settled result/error rather
than type-specific widgets. `PageUp` and `PageDown` provide basic
scroll/follow behavior. Code and diff syntax highlighting deliberately remain
outside this host so a shared highlighter can be introduced without changing
the transcript contract.

The fixed footer keeps provider/model, run state, provider-reported token and
cache counters, exact cost, and context status visible. Every unavailable
field is rendered as `unknown`; the host never substitutes a price, cache hit,
or context capacity. When a catalog model has a source-backed capacity, the
footer also reports `context N% used` and whether automatic compaction is
available. Custom models without capacity remain explicitly unknown and do
not opt into automatic compaction.

The local terminal substrate is deliberately small: `Cell`, `Style`, `Rect`,
`Grid`, previous/current frame comparison, and direct Crossterm flushing. A
failed flush must invalidate the previous frame so the next draw repaints it.
Unicode width support is admitted only after a focused rendering/cursor test
demonstrates that scalar-count placement is insufficient.

The mandatory streaming regression runs the compiled `pi-agent` binary inside
a native PTY against a loopback HTTP/1.1 OpenRouter fixture. It submits input
through the PTY, holds the response body after its first SSE text record, and
uses a virtual VT100 screen to assert that first text is already visible. The
test-only `pty-harness` feature exposes the fixture endpoint; normal builds
retain the fixed OpenRouter endpoint and never read that test input.

## Interaction and terminal lifecycle

The native composer supports insertion, multiline paste, Shift+Enter, left/right,
Home/End, word motions (`Alt+B`/`Alt+F`), backspace/delete, prompt history
(`Up`/`Down`), command completion (`Tab`), submission, and `Ctrl+G`. Long
multiline input follows the cursor within the bounded composer region. `$EDITOR`
remains available for larger edits: the host creates a private temporary file,
suspends raw/alternate-screen state, parses and invokes `$EDITOR` without a
shell, reads replacement text, restores the terminal, and removes the file on
recoverable paths. An editor failure preserves the existing composer text.

`TerminalGuard` owns raw mode, alternate screen, cursor visibility, and
bracketed paste. Restoration is a correctness property for ordinary exit,
provider/tool/compaction failures, cancellation, and editor failures. A panic
path should restore terminal state before delegating to the prior panic hook
when practical.

The direct commands are intentionally not a plugin framework:

```text
/help      show keybindings and commands
/model     open the compiled cross-provider model selector
/cost      print per-turn and aggregate reported accounting
/compact   invoke the configured manual compactor
/clear     reset the idle linear conversation
/quit      exit after structured cancellation and settlement
```

The model selector uses arrow keys, literal substring filtering across provider
and model names, Enter, and Escape. It shows every compiled catalog model in a
single list, plus explicit custom-model rows where an adapter allows them. An
unavailable credential remains explainable rather than being guessed or
silently substituted.

Submitting text while idle starts a prompt. Submitting while active enqueues
steering, and the transcript waits for the core event that makes it visible.
`/steer <prompt>` and `/followup <prompt>` expose the two named core queues;
the transcript projection shows each queued prompt, its drain mode, and its
active-turn or idle-boundary placement until core consumes it. A failed or
cancelled idle prompt is restored to the composer for an explicit re-submit;
it is never silently replayed.
`Ctrl+C` calls `Agent::abort` while active; it never kills the process to stop
inference. While idle it clears nonempty input or exits with an empty composer.
`/clear` refuses during an active run and never cancels or queues itself.

## Accounting and compaction

The status and `/cost` display only values reported by the provider: input,
output, reasoning, cache-read, cache-write, and exact decimal cost may each be
unknown. The host must not manufacture a pricing table or estimated cost. The
core may emit deterministic context estimates only when an explicit
automatic-compaction policy is configured; those estimates are capacity policy,
not billing data.

Compaction changes retained context and therefore remains a core transaction.
The caller-supplied, cancellation-aware `Compactor` receives a versioned,
owned context and proposes replacement messages. Core validates message and
tool-call relationships, commits atomically on success, emits typed
`compaction_start`/`compaction_result`/`compaction_end` events, and leaves the
old conversation intact on failure, invalid output, or cancellation.

Manual compaction is idle-only. The TUI installs a provider-backed `Compactor`
for the selected provider/model and configures core `AutomaticCompactionPolicy`
from either an explicit local capacity or the registry's source-backed model
capacity. Its summary request reuses the selected provider through the
OpenAI-compatible context hook, while core owns the split, validation, atomic
commit, threshold trigger, and event lifecycle. Models without a documented or
explicit capacity remain manual-only and report automatic compaction
unavailable; the TUI does not guess a context budget.

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
terminal restoration, and real-binary streaming through the focused PTY
harness. The harness uses only a local HTTP/1.1 fixture and a fake credential;
it never contacts or bills a provider. Do not run pre-commit hooks or push from
this repository.
