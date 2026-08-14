# Provider adapters

The default `pi-agent-core` build contains only the `ModelProvider` and
`ModelEventStream` ports. It does not choose a provider, issue HTTP requests,
or discover credentials. Optional adapters are an embedding convenience, not
a change to that core boundary.

The built-in finite-response adapters retry replay-safe failures with a bounded
exponential backoff. The standard policy makes the initial attempt plus three
retries at 250 ms, 500 ms, and 1 s, capped at 8 s. Transport failures are
retryable for both adapters; provider response errors are retried only when the
adapter can classify them as transient (for example, 429 or 5xx). Hosts can
replace the policy with `RetryPolicy` through each adapter config's
`with_retry_policy` method. The generic `ModelProvider` port does not retry
opaque caller providers or replay a stream after it has exposed events.

| Feature | Module | Wire protocol | Intended use |
| --- | --- | --- | --- |
| `provider-openrouter` | `pi_agent_core::provider::openrouter` | OpenRouter Chat Completions plus optional generation accounting | Opt-in finite-response transport with packet-bound model validation and cancellation-aware child custody. |
| `provider-commandcode` | `pi_agent_core::provider::commandcode` | Command Code `/alpha/generate` NDJSON | Opt-in Command Code gateway transport; the evaluation runner selects it with `--provider commandcode`. |

Enable only the provider an application owns:

```toml
[dependencies]
pi-agent-core = { path = "../pi-agent-core-rs/crates/pi-agent-core", features = ["provider-commandcode"] }
```

Neither feature is enabled by default. The adapters use the caller's selected
executor and add no Tokio dependency.

## Factory-grade OpenRouter contract

`OpenRouterProvider` takes an explicit `OpenRouterConfig`; it does not read a
credential file, environment variable, working directory, or model catalog.
Use `OpenRouterConfig::try_new` (or call `validate` before admission) so an
empty key/model, zero output cap, or unsafe key spelling fails before transport.
Every request must carry a `ModelDescriptor` whose provider is exactly
`openrouter` and whose model is exactly the configured model. A missing or
mismatched descriptor is a terminal adapter error and never results in a
network request.

The configured `max_tokens` is sent as the OpenRouter `max_tokens` output cap.
Request-scoped `ThinkingLevel` values are mapped to OpenRouter's native
`reasoning: { "effort": ... }` object (`off` maps to `none`); the default level
omits the field. This keeps provider-specific wire details in the adapter while
leaving policy and model selection with the host.

The provider runs a direct `curl` child with a private mode-0600 config file,
without putting the API key in argv or the child environment. It polls the
run's `CancellationToken`; cancellation kills and reaps that direct child and
settles as `StopReason::Cancelled`. Response output is captured in private
bounded-lifetime files so a full provider response cannot deadlock on a pipe.

Provider accounting is available through `usage_snapshot` and `cost_report`.
Token counters retain unknown-vs-zero semantics. Each cost turn and the
aggregate report expose exact non-negative decimal strings in
`total_usd_exact`, `upstream_inference_usd_exact`,
`reported_total_usd_exact`, and
`reported_upstream_inference_usd_exact`. The parallel `f64` fields exist only
as convenience projections and must not be used for budget decisions.

When an adapter drives `Agent`, its normalized `Usage` update is retained in
`AgentSnapshot.accounting` and emitted as `AgentEventKind::ModelTurnUsage`.
OpenRouter maps reported cache fields and exact total cost into that update;
Command Code maps the token fields it reports and leaves cache and cost
unknown. The core performs no pricing lookup.

## Credentials and host authority

Both adapters accept a key directly in their configuration. They never read
environment variables, a home-directory auth file, the current working
directory, or the system clock. Applications may obtain credentials and host
facts using their own secret/capability boundary, then pass those values in.

Command Code also requires a `CommandCodeHostContext`, which makes the
gateway's `workingDir`, `date`, and `environment` fields an explicit host
decision:

```rust,no_run
use pi_agent_core::provider::commandcode::{
    CommandCodeConfig, CommandCodeHostContext, CommandCodeProvider,
};
use pi_agent_core::provider::RetryPolicy;
use std::time::Duration;

let host = CommandCodeHostContext::new("/sandbox/project", "2026-08-14", "linux")?;
let config = CommandCodeConfig::new("caller-supplied-api-key", "deepseek/deepseek-v4-flash", host)?;
let config = config.with_retry_policy(RetryPolicy::new(
    3,
    Duration::from_millis(250),
    Duration::from_secs(8),
));
let provider = CommandCodeProvider::new(config);
# let _ = provider;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`CommandCodeConfig` also provides explicit permission mode, a canonical UUID thread ID, mode,
temperature, output-token limit, and zero-data-retention-header settings. When present, the
thread ID is also sent as the Command Code session ID, matching the current headless client's
per-thread request shape without having the library generate or discover an identifier.
The current Command Code client metadata is also preserved: the project slug defaults to the
final component of the already-explicit `workingDir` (and can be overridden), and taste learning
defaults to the upstream client's enabled setting but can be disabled with
`with_taste_learning_enabled(false)`.
The provider accepts only a request whose `ModelDescriptor` is
`command-code` with the configured model, avoiding a silent model mismatch.

The `pi-agent-eval` executable is a caller-owned integration boundary. Its
Command Code mode reads `COMMANDCODE_API_KEY` from its process environment,
not from the library, and requires explicit `--commandcode-date` and
`--commandcode-environment` values plus a caller-owned canonical UUID passed as
`--commandcode-thread-id`, plus `--commandcode-project-slug`. This keeps ambient
secret and host lookup out of `CommandCodeProvider` while making a deliberate
command-line harness practical.

## Context and stream mapping

The Command Code adapter consumes a caller-converted standard Chat
Completions JSON message array from `ModelRequest.context`. It maps textual
user/assistant messages and function-style assistant tool calls into the
gateway's `text`, `tool-call`, and `tool-result` content blocks. A tool result
must match a preceding assistant tool call, so the adapter can preserve its
tool name instead of guessing it.

The gateway's `text-delta`, `tool-call`, `finish`, usage, error, and abort
events map directly to core model-stream events. Gateway error payloads stay
generic before entering agent state, so a remote service cannot inject
arbitrary error text into a transcript. A trusted host can instead call
`CommandCodeProvider::last_error_report()` for the last failure's source,
message, status, type, code, and retryability classification. The configured
API key is redacted from this host-only report, but its remote message remains
untrusted data and belongs only in private host diagnostics. Reasoning deltas
are intentionally not retained: the current core model-stream contract has no
separate reasoning content variant, so treating them as assistant text would
corrupt the visible answer. This is a known API limitation rather than a hidden
fallback. The current gateway may emit a `provider-metadata` envelope after
`finish`; it is accepted as non-content metadata rather than misclassified as a
second terminal event.

The first adapters collect their bounded `curl` response before returning a
finite core stream. They preserve event grammar and terminal validation, but
they do not yet expose network-time incremental deltas. Hosts needing live
transport streaming should implement `ModelProvider` directly while preserving
the same request and event contracts.
