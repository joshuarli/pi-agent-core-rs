# Recorded evidence

Recorded files preserve a provider capture exactly as collected, including stable
error responses, unavailable endpoints, and successful streaming sessions. They are not executed
and do not permit network access.
An adapter may read them and emit a canonical result, but must not rewrite the recorded file to
fit the declarative format. See [`../../normalization.md`](../../normalization.md) for the mapping
of the existing `recorded_pi_sdk_terminal_response` shape.

The checked-in OpenRouter captures have a provider-free replay and provenance verifier at
[`../../recorded/replay.py`](../../recorded/replay.py), exercised by
[`../../run-recorded.sh`](../../run-recorded.sh). Its canonical output is checked in beside the
capture as `*.canonical.json`; this is a replay golden, not a second provider capture.

An OpenRouter capture is immutable evidence collected through an explicitly scoped provider
adapter and caller-managed credentials. A host-installed `pi` executable is not a replay source.

`poolside/laguna-xs-2.1` currently records OpenRouter's redacted guardrail/privacy-policy 404.
It is terminal provider-error evidence, not a successful Poolside session or an available-model
selection for the final comparative evaluation. Its canonical replay has a separate remediation
hint: an account restricted to Zero Data Retention models can receive this 404 when the selected
model has no eligible Zero Data Retention endpoint. The original OpenRouter message remains
unaltered in the recorded capture and canonical `error.message`.

`deepseek/deepseek-v4-flash-0731` completed the same no-tool session. Its capture
also records the SDK's provider-shaped thinking stream; replay canonicalizes the thought text
without treating its provider signature as core protocol state. It establishes only this small
live model interaction, not comparative coding-task parity.
