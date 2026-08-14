# Declarative fixture format

Declarative fixtures describe an agent run without describing how either implementation invokes
it. They are JSON files with `format_version: 1` and `kind: "declarative_parity_fixture"`.
Unknown top-level fields are rejected so a typo cannot silently change a test. JSON object key
order is insignificant; array order is significant.

## Shape

The required shape is:

```json
{
  "format_version": 1,
  "kind": "declarative_parity_fixture",
  "id": "single-turn-text",
  "description": "A short human-readable reason for this case.",
  "setup": {
    "system_prompt": "...",
    "model": { "provider": "fixture", "id": "deterministic-text" },
    "thinking_level": "off",
    "tools": []
  },
  "actions": [
    { "kind": "prompt", "text": "..." }
  ],
  "model_script": [
    {
      "chunks": [
        { "kind": "text_delta", "text": "..." },
        {
          "kind": "done",
          "stop_reason": "stop",
          "usage": {
            "input": 0,
            "output": 0,
            "cache_read": 0,
            "cache_write": 0,
            "total_tokens": 0
          }
        }
      ]
    }
  ],
  "stream_comparison": "semantic",
  "host": { "tools": [] },
  "assertions": {
    "outcome": "completed",
    "event_types": ["agent_start", "agent_settled"]
  }
}
```

`description` is required for review but is not compared. `id` is a stable slash-separated
identifier, unique within the fixture tree, and is also the basename used for expected and
normalized output. It must not contain a path escape, an absolute path, or a provider secret.
`stream_comparison` is optional and defaults to `semantic`; its only other value is `exact`.

### `setup`

`setup` is explicit agent state:

* `system_prompt` is the exact prompt text, including intentional whitespace.
* `model` has the provider and model identifier. For deterministic fixtures the provider should
  be `fixture`; a live provider is not permitted in a declarative fixture.
* `thinking_level` is one of the levels supported by the target contract (`off`, `minimal`,
  `low`, `medium`, or `high`).
* `steering_mode` and `follow_up_mode` are optional `one-at-a-time` (the default) or `all` drain
  policies for their corresponding explicit queues.
* `tools` is an ordered list of tool definitions. Each definition has `name`, `description`, and
  a JSON value `parameters` describing its input shape. `execution_mode` is optional and is
  `parallel` by default; `sequential` makes an entire assistant tool batch sequential, matching
  Pi. Tool names are unique. The schema is data in the fixture; validating it does not require a
  schema package in the harness.

Tool definitions are only capabilities supplied to the agent. They do not grant ambient
filesystem, process, clock, network, or environment access.

### `actions`

Actions are applied in order. Version 1 defines:

* `{ "kind": "prompt", "text": string }` — add a user message and start inference.
* `{ "kind": "continue" }` — continue from the settled state without adding a user message.
* `{ "kind": "cancel", "boundary": string }` — request cancellation at the named deterministic
  boundary (`model_stream`, `tool_prepare`, `tool_execute`, or `between_turns`).

An action consumes one `model_script` turn when inference is started. A fixture must provide
enough turns for its actions. Extra turns are an error, rather than silently ignored input.

The currently checked-in V0 adapters implement a deliberately closed action slice: ordered
`steer`, `follow_up`, `prompt`, and `continue` actions. They reject `cancel` until a differential
case extends both adapters. The format reserves cancellation boundaries so fixture data does not
need a schema migration when that work lands.

### `model_script`

`model_script` is a deterministic provider-neutral stream script. The upstream and Rust adapters
translate it into their respective provider-stream interfaces. Each entry is one inference turn;
`chunks` are emitted in order. Version 1 chunk kinds are:

* `text_delta` with a string `text`;
* `thinking_delta` with a string `text`;
* `tool_call` with a stable fixture-local `id`, `name`, and JSON `arguments`;
* `done` with `stop_reason` (`stop`, `tool_call`, `length`, or `error`) and a complete `usage`
  object;
* `error` with a typed `kind` and stable `message`.

Every turn ends in exactly one `done` or `error` chunk. A `tool_call` done turn is followed by
the tool result and, unless the fixture cancels, the next scripted inference turn. Scripted
arguments are data, not executable code.

### `host`

`host.tools` gives deterministic responses for tools used by the model script. A tool response
has the shape:

```json
{
  "name": "echo",
  "calls": [
    {
      "arguments": { "text": "hello" },
      "result": {
        "is_error": false,
        "content": [{ "type": "text", "text": "hello" }]
      }
    }
  ]
}
```

The first exact `arguments` match is selected. A missing match is a fixture error; a runner may
not invent a tool result. `result.content` uses the same content-part vocabulary as canonical
messages (`text`, `thinking`, `tool_call`, and `json`). A host response may set `is_error: true`
to exercise tool failure while still settling the agent loop. `yield_once: true` is an optional,
runner-owned deterministic scheduling directive: it causes that call to yield one poll/microtask
turn before returning, allowing a fixture to assert completion ordering without a clock.
`updates` is an optional ordered array of text partial results emitted before the final result. It
exists to exercise `tool_execution_update` ordering and is runner data rather than an ambient
progress channel.

For the current hook slice, `host.before_tool_call` may be `{ "tool_name": string, "reason":
string }`. It blocks exactly that named call after schema validation and creates the stated error
tool result. `host.after_tool_call` may be `{ "tool_name": string, "content": string,
"is_error": boolean }`; it replaces those terminal result fields after execution. Other hook
behavior is added only with an upstream differential fixture.

### `assertions`

Assertions are intentionally a small projection of the canonical result. They may require
`outcome`, `event_types`, `assistant_text`, `messages`, `usage`, `tool_results`, or `error`.
`event_types` is a convenience projection: a role-bearing event is rendered as
`<type>:<role>` (for example, `message_start:user`), while other events use their `type` alone.
Missing assertion fields mean “do not assert this field”; they do not mean “ignore a required
canonical result field.” For a complete golden result, put the full canonical result in
`fixtures/expected/<id>.json`.

## Fixture classes

Do not mix these classes:

* **Declarative** fixtures are provider-free inputs and are safe to execute repeatedly.
* **Expected** fixtures are checked-in canonical outputs for a declarative fixture.
* **Recorded** fixtures preserve an external capture, including provider errors and unavailable
  endpoints. Their original shape is evidence and is not rewritten in place.

The existing OpenRouter capture under `fixtures/recorded/openrouter/` is therefore intentionally
not a declarative fixture and must not be edited to fit this format. A recorded adapter maps its
stable semantic fields into the canonical result shape described next.
