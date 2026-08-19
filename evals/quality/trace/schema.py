"""The JSON-facing canonical trace schema.

The quality harness deliberately keeps this layer independent of a runtime
crate.  A trace is an ordered list of records.  Each record has a sequence
number, a stable ``kind`` discriminator, and an otherwise extensible object of
fields.  Unknown fields are retained: the quality artifact must not silently
discard a new tool or provider field before a fixture comparison can see it.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
import copy
import json
from typing import Any, Optional


TRACE_SCHEMA_VERSION = "pi-quality-trace/v1"
METRICS_SCHEMA_VERSION = "pi-quality-trace-metrics/v1"
REPORT_SCHEMA_VERSION = "pi-quality-trace-report/v1"

# These are the vocabulary understood by the built-in metrics extractor.  The
# schema remains open to additional event kinds so a new adapter can be
# inspected and diffed before this list is extended.
EVENT_KINDS = (
    "run_start",
    "request",
    "response",
    "tool_call",
    "tool_result",
    "error",
    "run_end",
)


def _canonical_value(value: Any) -> Any:
    """Return a JSON-compatible copy with object keys in stable order.

    Arrays are intentionally *not* sorted.  Message order, tool-definition
    order, and tool-completion order are semantic evidence in an evaluation.
    ``json.dumps(sort_keys=True)`` would produce the same bytes, but producing
    the ordered copy also makes ``to_dict`` useful for callers that inspect it.
    """

    if isinstance(value, Mapping):
        if any(not isinstance(key, str) for key in value):
            raise TypeError("canonical JSON objects require string keys")
        return {key: _canonical_value(value[key]) for key in sorted(value)}
    if isinstance(value, (list, tuple)):
        return [_canonical_value(item) for item in value]
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    raise TypeError(f"value of type {type(value).__name__} is not JSON-compatible")


@dataclass(frozen=True)
class TraceEvent:
    """One ordered event in a canonical quality trace.

    ``data`` is copied at construction and is never interpreted or filtered by
    the schema layer.  That is important for tool fields: ``arguments``,
    ``input``, ``output``, ``error``, retry metadata, and provider extensions
    all remain available to a downstream diff.
    """

    seq: int
    kind: str
    data: Mapping[str, Any]

    def __post_init__(self) -> None:
        if not isinstance(self.seq, int) or isinstance(self.seq, bool) or self.seq < 0:
            raise ValueError("trace event seq must be a non-negative integer")
        if not isinstance(self.kind, str) or not self.kind:
            raise ValueError("trace event kind must be a non-empty string")
        if not isinstance(self.data, Mapping):
            raise TypeError("trace event data must be an object")
        if "seq" in self.data or "kind" in self.data:
            raise ValueError("trace event data cannot redefine seq or kind")
        # Validate and detach caller-owned data while retaining the frozen
        # event's useful Mapping interface.
        detached = _canonical_value(copy.deepcopy(dict(self.data)))
        object.__setattr__(self, "data", detached)

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any], seq: Optional[int] = None) -> "TraceEvent":
        if not isinstance(value, Mapping):
            raise TypeError("trace event must be an object")
        raw_seq = value.get("seq", seq)
        if raw_seq is None:
            raise ValueError("trace event is missing seq")
        if "kind" in value:
            discriminator = "kind"
        elif "event_type" in value:
            discriminator = "event_type"
        elif "type" in value:
            discriminator = "type"
        else:
            discriminator = "kind"
        raw_kind = value.get(discriminator)
        if not isinstance(raw_kind, str) or not raw_kind:
            raise ValueError("trace event is missing kind")
        data = {
            key: copy.deepcopy(item)
            for key, item in value.items()
            if key not in {"seq", discriminator}
        }
        return cls(int(raw_seq), raw_kind, data)

    def to_dict(self) -> dict[str, Any]:
        output: dict[str, Any] = {"seq": self.seq, "kind": self.kind}
        output.update(_canonical_value(self.data))
        return output

    def canonical_json(self) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


@dataclass(frozen=True)
class TraceArtifact:
    """A complete, serializable quality-evaluation trajectory."""

    events: tuple[TraceEvent, ...]
    trace_id: str = ""
    metadata: Mapping[str, Any] = None  # type: ignore[assignment]
    schema_version: str = TRACE_SCHEMA_VERSION

    def __post_init__(self) -> None:
        if not isinstance(self.schema_version, str) or not self.schema_version:
            raise ValueError("trace schema_version must be a non-empty string")
        if not isinstance(self.trace_id, str):
            raise TypeError("trace_id must be a string")
        if self.metadata is None:
            object.__setattr__(self, "metadata", {})
        elif not isinstance(self.metadata, Mapping):
            raise TypeError("trace metadata must be an object")
        else:
            object.__setattr__(self, "metadata", _canonical_value(copy.deepcopy(dict(self.metadata))))
        normalized_events: list[TraceEvent] = []
        for item in self.events:
            if not isinstance(item, TraceEvent):
                raise TypeError("trace events must be TraceEvent values")
            normalized_events.append(item)
        object.__setattr__(self, "events", tuple(normalized_events))

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "TraceArtifact":
        if not isinstance(value, Mapping):
            raise TypeError("trace artifact must be an object")
        raw_events = value.get("events", value.get("trace"))
        if not isinstance(raw_events, list):
            raise ValueError("trace artifact events must be an array")
        events = tuple(TraceEvent.from_mapping(item, seq=index) for index, item in enumerate(raw_events))
        return cls(
            events=events,
            trace_id=str(value.get("trace_id", value.get("run_id", ""))),
            metadata=value.get("metadata", {}),
            schema_version=str(value.get("schema_version", TRACE_SCHEMA_VERSION)),
        )

    @classmethod
    def from_events(
        cls,
        events: Iterable[TraceEvent | Mapping[str, Any]],
        *,
        trace_id: str = "",
        metadata: Optional[Mapping[str, Any]] = None,
    ) -> "TraceArtifact":
        canonical_events: list[TraceEvent] = []
        for index, item in enumerate(events):
            if isinstance(item, TraceEvent):
                canonical_events.append(item)
            elif isinstance(item, Mapping):
                canonical_events.append(TraceEvent.from_mapping(item, seq=index))
            else:
                raise TypeError("trace events must be objects")
        return cls(tuple(canonical_events), trace_id=trace_id, metadata=metadata or {})

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "trace_id": self.trace_id,
            "metadata": _canonical_value(self.metadata),
            "events": [event.to_dict() for event in self.events],
        }

    def canonical_json(self) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)

    def validate_lifecycle(self) -> tuple[bool, str]:
        """Validate the conventional start/end envelope without dropping data."""

        if not self.events:
            return False, "empty"
        starts = sum(_event_kind(event) in {"run_start", "episode_start", "start"} for event in self.events)
        ends = sum(_event_kind(event) in {"run_end", "episode_end", "end", "terminal"} for event in self.events)
        if starts != 1 or ends != 1:
            return False, "missing_or_duplicate_boundary"
        if _event_kind(self.events[0]) not in {"run_start", "episode_start", "start"}:
            return False, "start_not_first"
        if _event_kind(self.events[-1]) not in {"run_end", "episode_end", "end", "terminal"}:
            return False, "end_not_last"
        return True, "valid"


def _event_kind(value: TraceEvent | Mapping[str, Any]) -> str:
    if isinstance(value, TraceEvent):
        return value.kind
    raw = value.get("kind", value.get("event_type", value.get("type", "")))
    return str(raw) if raw is not None else ""


def event(kind: str, *, seq: Optional[int] = None, **fields: Any) -> TraceEvent:
    """Construct an event, assigning a temporary zero sequence when omitted."""

    return TraceEvent(0 if seq is None else seq, kind, fields)


def trace_from_events(
    events: Iterable[TraceEvent | Mapping[str, Any]],
    *,
    trace_id: str = "",
    metadata: Optional[Mapping[str, Any]] = None,
) -> TraceArtifact:
    return TraceArtifact.from_events(events, trace_id=trace_id, metadata=metadata)


def coerce_trace(value: TraceArtifact | Mapping[str, Any] | Iterable[Mapping[str, Any]]) -> TraceArtifact:
    """Convert adapter output or an already canonical trace to an artifact."""

    if isinstance(value, TraceArtifact):
        return value
    if isinstance(value, Mapping):
        if "events" in value or "trace" in value:
            return TraceArtifact.from_dict(value)
        raise ValueError("trace object must contain events or trace")
    return TraceArtifact.from_events(value)


# Names used by adapters and reports are intentionally aliases, not separate
# types.  This prevents one side of a differential run from accidentally
# acquiring a subtly different schema.
CanonicalEvent = TraceEvent
CanonicalTrace = TraceArtifact
Trace = TraceArtifact
