# Fixture directories

The fixture tree separates source inputs, golden outputs, generated outputs, and external
evidence:

* [`declarative/`](declarative/) contains deterministic JSON inputs described in
  [`../fixture-format.md`](../fixture-format.md).
* [`expected/`](expected/) contains optional checked-in canonical results.
* [`normalized/`](normalized/) is a staging location for runner output and should not be treated
  as source-of-truth.
* [`recorded/`](recorded/) contains immutable captures whose original shape is retained.

Keep one fixture ID and filename across `declarative/`, `expected/`, and `normalized/`. Recorded
captures may use a provider-specific filename and format. The V0 closed slice currently covers
text, tools (including error and parallel ordering), and queued steering/follow-up cases.
