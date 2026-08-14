"""Machine-readable and concise human-readable quality reports."""

from __future__ import annotations

from collections.abc import Mapping
import json
from typing import Any, Optional

from .diff import TraceDiff, diff_traces
from .metrics import TraceMetrics, extract_metrics
from .schema import REPORT_SCHEMA_VERSION, TraceArtifact, coerce_trace


def build_report(
    left: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    right: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    *,
    context: int = 2,
    left_label: str = "left",
    right_label: str = "right",
) -> dict[str, Any]:
    """Build a complete differential artifact without provider-specific data."""

    left_trace = coerce_trace(left)
    right_trace = coerce_trace(right)
    difference = diff_traces(left_trace, right_trace, context=context)
    left_metrics = extract_metrics(left_trace)
    right_metrics = extract_metrics(right_trace)
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "equal": difference.equal,
        "labels": {"left": left_label, "right": right_label},
        "diff": difference.to_dict(),
        "metrics": {
            left_label: left_metrics.to_dict(),
            right_label: right_metrics.to_dict(),
        },
    }


def report_json(
    report_or_left: Mapping[str, Any] | TraceArtifact | list[Mapping[str, Any]],
    right: Optional[TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]]] = None,
    *,
    context: int = 2,
    left_label: str = "left",
    right_label: str = "right",
) -> str:
    """Serialize a report, or build one from two traces and serialize it."""

    report = (
        dict(report_or_left)
        if right is None and isinstance(report_or_left, Mapping) and "diff" in report_or_left
        else build_report(report_or_left, right, context=context, left_label=left_label, right_label=right_label)  # type: ignore[arg-type]
    )
    return json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


json_report = report_json


def trace_json(trace: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]]) -> str:
    """Serialize one trace in stable canonical JSON form."""

    return coerce_trace(trace).canonical_json()


def _metrics_line(label: str, metrics: Mapping[str, Any]) -> str:
    errors = metrics.get("tool_errors", {})
    by_class = errors.get("by_class", {}) if isinstance(errors, Mapping) else {}
    error_text = ", ".join(f"{name}={count}" for name, count in sorted(by_class.items())) or "none"
    retries = metrics.get("retry_count", 0)
    order = "preserved" if metrics.get("order_preserved", True) else "out-of-order"
    lifecycle = metrics.get("lifecycle", {})
    status = lifecycle.get("status", "unknown") if isinstance(lifecycle, Mapping) else "unknown"
    return (
        f"{label}: events={metrics.get('event_count', 0)} "
        f"requests={metrics.get('request_count', 0)} responses={metrics.get('response_count', 0)} "
        f"tools={metrics.get('tool_call_count', 0)}/{metrics.get('tool_result_count', 0)} "
        f"errors={metrics.get('tool_error_count', 0)} ({error_text}) "
        f"retries={retries} order={order} lifecycle={status}"
    )


def human_report(
    left: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]],
    right: Optional[TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]]] = None,
    *,
    context: int = 2,
    left_label: str = "left",
    right_label: str = "right",
) -> str:
    """Render the high-signal portion of a differential report.

    The event payloads in a divergence are intentionally abbreviated to one
    line.  Callers can use :func:`report_json` for the complete context.
    """

    if right is None:
        raise TypeError("human_report requires both left and right traces")
    report = build_report(left, right, context=context, left_label=left_label, right_label=right_label)
    diff = report["diff"]
    left_metrics = report["metrics"][left_label]
    right_metrics = report["metrics"][right_label]
    lines = [
        f"trace comparison: {'MATCH' if report['equal'] else 'DIVERGED'}",
        _metrics_line(left_label, left_metrics),
        _metrics_line(right_label, right_metrics),
    ]
    divergence = diff.get("first_divergence")
    if divergence is None:
        lines.append("first divergence: none")
        return "\n".join(lines)
    lines.append(
        f"first divergence: index={divergence['index']} reason={divergence['reason']} "
        f"(context ±{diff.get('context_radius', context)})"
    )
    left_event = divergence.get("left")
    right_event = divergence.get("right")
    lines.append(f"  {left_label}: {_event_line(left_event)}")
    lines.append(f"  {right_label}: {_event_line(right_event)}")
    return "\n".join(lines)


def _event_line(event: Any) -> str:
    if event is None:
        return "<missing>"
    if not isinstance(event, Mapping):
        return str(event)
    kind = event.get("kind", "<unknown>")
    seq = event.get("seq", "?")
    fields = {key: value for key, value in event.items() if key not in {"seq", "kind"}}
    payload = json.dumps(fields, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    # Keep reports useful in terminal logs even when a provider supplied a
    # very large tool output.
    if len(payload) > 240:
        payload = payload[:237] + "..."
    return f"#{seq} {kind} {payload}"

