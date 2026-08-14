"""Trace-first-divergence comparison with local context."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
import json
from typing import Any, Optional

from .schema import TraceArtifact, TraceEvent, coerce_trace


@dataclass(frozen=True)
class Divergence:
    """The first event position at which two traces cease to agree."""

    index: int
    reason: str
    left: Optional[dict[str, Any]]
    right: Optional[dict[str, Any]]

    def to_dict(self) -> dict[str, Any]:
        return {
            "index": self.index,
            "reason": self.reason,
            "left": self.left,
            "right": self.right,
        }


@dataclass(frozen=True)
class TraceDiff:
    """A deterministic diff result suitable for JSON or human reports."""

    equal: bool
    left_length: int
    right_length: int
    divergence: Optional[Divergence]
    left_context: tuple[Optional[dict[str, Any]], ...]
    right_context: tuple[Optional[dict[str, Any]], ...]
    context_radius: int

    @property
    def first_divergence(self) -> Optional[Divergence]:
        return self.divergence

    @property
    def first_divergence_index(self) -> Optional[int]:
        return self.divergence.index if self.divergence is not None else None

    def to_dict(self) -> dict[str, Any]:
        return {
            "equal": self.equal,
            "left_length": self.left_length,
            "right_length": self.right_length,
            "first_divergence": self.divergence.to_dict() if self.divergence else None,
            "context_radius": self.context_radius,
            "context": {
                "left": list(self.left_context),
                "right": list(self.right_context),
            },
            # These aliases make reports convenient to consume without
            # forcing callers to know the nested context spelling.
            "left_context": list(self.left_context),
            "right_context": list(self.right_context),
        }

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _event_dict(event: Optional[TraceEvent]) -> Optional[dict[str, Any]]:
    return event.to_dict() if event is not None else None


def _reason(left: Optional[TraceEvent], right: Optional[TraceEvent]) -> str:
    if left is None:
        return "missing_left_event"
    if right is None:
        return "missing_right_event"
    if left.kind != right.kind:
        return "kind"
    if left.data != right.data:
        return "fields"
    if left.seq != right.seq:
        return "sequence"
    return "event"


def diff_traces(
    left: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    right: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    *,
    context: int = 2,
) -> TraceDiff:
    """Compare event order and payloads, returning the earliest divergence.

    Trace metadata and IDs are intentionally excluded from event comparison:
    they identify runs, not the trajectory.  Sequence numbers and every event
    field are compared, so a reordered tool result is reported at the first
    affected position rather than being hidden by a set comparison.
    """

    if not isinstance(context, int) or isinstance(context, bool) or context < 0:
        raise ValueError("context radius must be a non-negative integer")
    left_trace = coerce_trace(left)
    right_trace = coerce_trace(right)
    first: Optional[Divergence] = None
    shared = min(len(left_trace.events), len(right_trace.events))
    for index in range(shared):
        left_event = left_trace.events[index]
        right_event = right_trace.events[index]
        if left_event.to_dict() != right_event.to_dict():
            first = Divergence(index, _reason(left_event, right_event), left_event.to_dict(), right_event.to_dict())
            break
    if first is None and len(left_trace.events) != len(right_trace.events):
        index = shared
        left_event = left_trace.events[index] if index < len(left_trace.events) else None
        right_event = right_trace.events[index] if index < len(right_trace.events) else None
        first = Divergence(index, _reason(left_event, right_event), _event_dict(left_event), _event_dict(right_event))

    if first is None:
        left_context: tuple[Optional[dict[str, Any]], ...] = ()
        right_context: tuple[Optional[dict[str, Any]], ...] = ()
    else:
        start = max(0, first.index - context)
        stop = first.index + context + 1
        left_context = tuple(
            _event_dict(left_trace.events[index]) if index < len(left_trace.events) else None
            for index in range(start, stop)
        )
        right_context = tuple(
            _event_dict(right_trace.events[index]) if index < len(right_trace.events) else None
            for index in range(start, stop)
        )
    return TraceDiff(
        equal=first is None,
        left_length=len(left_trace.events),
        right_length=len(right_trace.events),
        divergence=first,
        left_context=left_context,
        right_context=right_context,
        context_radius=context,
    )


def first_divergence(
    left: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    right: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    *,
    context: int = 2,
) -> Optional[Divergence]:
    return diff_traces(left, right, context=context).divergence


diff_trace = diff_traces

