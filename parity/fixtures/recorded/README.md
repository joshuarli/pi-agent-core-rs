# Recorded evidence

Recorded files preserve an upstream or provider capture exactly as collected, including stable
error responses and unavailable endpoints. They are not executed and do not permit network access.
An adapter may read them and emit a canonical result, but must not rewrite the recorded file to
fit the declarative format. See [`../../normalization.md`](../../normalization.md) for the mapping
of the existing `recorded_pi_sdk_terminal_response` shape.

The OpenRouter capture must be produced only by
[`../../upstream/record-openrouter.mts`](../../upstream/record-openrouter.mts) from the pinned
source checkout via `vault OPENROUTER_API_KEY -- …`. A host-installed `pi` executable is never a
capture source, because its version can differ from `parity/UPSTREAM_COMMIT`.
