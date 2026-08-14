# Recorded replay adapter

`replay.py` is a provider-free adapter for the checked-in OpenRouter terminal captures.
It reads the recording as immutable evidence and emits the canonical parity result; it never
invokes Pi, Node, a shell, a network client, or a credential injector. The adapter does not
attempt to reproduce the provider request or guess whether the provider error is retryable.

Run the checked-in verification from any working directory:

```text
./parity/run-recorded.sh
```

The command validates:

* `capture.pi_commit` and `capture.pi_agent_core_version` against `parity/UPSTREAM_COMMIT`;
* the pinned-source capture runner and OpenRouter/model identity;
* the required redaction manifest, including the API key, authorization headers, session ID,
  and timestamps;
* the one-turn event sequence (including optional assistant streaming updates), request,
  assistant response, usage shape, and terminal outcome;
* exact equality with the checked-in `.canonical.json` replay result.

For errors, the canonical output retains `outcome: "error"` with `error.kind: "model"`, while
preserving the raw stable error message captured by the SDK. The successful DeepSeek capture
canonicalizes its provider-shaped `thinking` part to core `{ "type": "thinking", "text": … }`
and deliberately omits the provider-specific signature. A recording is immutable evidence, not a
live availability test or comparative coding-task result. Captures with tools, multiple turns,
image parts, or other provider-specific fields must receive a separately scoped adapter and
fixture rather than being silently generalized here.

When OpenRouter's raw error explicitly names both privacy and guardrail restrictions, the
canonical error also has a non-authoritative `hint`. It explains that a Zero Data Retention account
policy can exclude the selected model and yield this 404; it does not alter or reinterpret the raw
provider message.

For machine-readable output without the checked-in golden comparison:

```text
python3 parity/recorded/replay.py \
  parity/fixtures/recorded/openrouter/inclusionai-ling-3.0-tiny-free-unavailable.json
```
