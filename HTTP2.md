# HTTP/2 transport boundary

HTTP/2 is a provider-transport concern, not a core execution requirement. The
`ModelProvider` and `ModelEventStream` contracts carry model requests and
events; they do not expose sockets, TLS sessions, ALPN, HTTP versions, stream
IDs, or flow-control windows.

## What Pi actually does

Pi's normal Node `fetch` path installs an Undici dispatcher with
`allowH2: false` in [`http-dispatcher.ts`](/Users/josh/d/pi/packages/coding-agent/src/core/http-dispatcher.ts:79).
The OpenAI and Anthropic SDK adapters receive that fetch implementation, and
Codex SSE calls the same global fetch. Those requests are therefore explicitly
HTTP/1.1-oriented, even though Undici supports HTTP/2.

Bedrock is the deliberate exception. The AWS Bedrock runtime client defaults to
`NodeHttp2Handler`; Pi selects an HTTP/1.1 handler when a proxy is configured or
`AWS_BEDROCK_FORCE_HTTP1=1` is set ([Bedrock setup](/Users/josh/d/pi/packages/ai/src/api/bedrock-converse-stream.ts:197)).
Codex WebSocket is a separate transport choice, not a requirement that ordinary
provider requests use HTTP/2.

## The Rust adapter decision

The in-tree Rust adapters are serial finite-response adapters. They collect one
OpenRouter SSE, Command Code NDJSON, or local Chat Completions response before
returning the core stream. They now use `ureq` with its rustls feature, which is
the smallest self-contained HTTPS facility that matches this HTTP/1.1 request
shape. No external command-line HTTP executable, process environment, temporary credential file, or
Tokio runtime is required.

HTTP/2 and asynchronous I/O are independent decisions. A blocking HTTP/2
client is possible, and an asynchronous HTTP/1.1 client is possible. We do not
add HTTP/2 merely to make the current serial adapters “more modern.”

## If an HTTP/2 adapter is needed later

Keep it behind a provider-specific feature. The adapter owns TLS and ALPN
negotiation, drives the HTTP/2 connection continuously, admits streams only
when peer capacity permits, releases inbound flow-control capacity as bytes
are consumed, and treats DATA-frame boundaries as arbitrary parser chunks.

Cancellation must reset or drop the active stream and settle the connection
before the provider operation returns. GOAWAY, stream reset, protocol errors,
malformed SSE/NDJSON, HTTP status failures, and caller cancellation remain
distinct outcomes. Retry is allowed only before model events are exposed and
only for classified replay-safe failures.

Adding such an adapter must not put `h2`, TLS, socket, or executor types in the
default core API. It should preserve the existing provider event contract and
be justified by a concrete provider requirement such as Bedrock parity, an
H2-only endpoint, or multiplexed live streaming.
