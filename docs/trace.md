# Core trace adapter

The optional `pi-agent-core/trace` feature connects the core's awaited
`EventObserver` boundary to the dependency-free `pi-agent-trace` contract:

```text
AgentEvent reducer → TraceObserver → RedactingSink → caller TraceSink
```

Construct `pi_agent_core::trace::TraceObserver` with a host-owned episode ID
and a `pi-agent-trace::TraceSink`, then register it on the agent builder with
`.observer(Arc::new(observer))`. The adapter emits an
`EpisodeHeader` at `AgentStart`, a compact `Turn` at each `TurnEnd`, a `Tool`
at each settled tool execution, and an `EpisodeEnd` at `AgentEnd`.

The compact V0 trace deliberately records settled tool output and does not
duplicate streaming tool updates. Core observers still receive
`ToolExecutionUpdate` events. The current core tool-start event does not carry
serialized arguments, so the adapter leaves `Tool.input` empty; hosts needing
raw arguments should use a dedicated observer at that boundary until the
event contract grows an explicit redacted-argument field.

Tracing is best effort. `TraceObserver` wraps the sink in
`pi_agent_trace::IsolatedSink`; `failed_events()` reports dropped records while
the agent run continues with the same state and result. To redact prompts or
tool content, wrap the sink in `pi_agent_trace::RedactingSink` before passing it
to the observer.

The adapter is intentionally synchronous at the observer boundary. Sink work
must remain bounded and must not call back into the agent. It creates no task,
thread, executor, clock, session tree, or persistence policy.

`pi-agent-trace` supplies explicit writer adapters for the two V0 encodings:
`JsonLinesSink<W>` writes one stable, escaped JSON record per line, and
`CborSink<W>` writes a concatenated sequence of definite-length CBOR maps.
Both accept an already-open caller-owned `Write`; neither opens a path or
chooses a destination. The JSON and CBOR record maps carry the same
`schema_version` and `type` fields, so an archive format change is a deliberate
trace-contract change rather than an implicit sink behavior change.
