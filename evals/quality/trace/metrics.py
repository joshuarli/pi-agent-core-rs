"""Observable trajectory metrics for canonical traces."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
import json
import re
from typing import Any, Optional

from .fingerprint import request_semantic_fingerprint
from .schema import METRICS_SCHEMA_VERSION, TraceArtifact, TraceEvent, coerce_trace


_KIND_ALIASES = {
    "episode_header": "run_start",
    "start": "run_start",
    "started": "run_start",
    "run_started": "run_start",
    "model_request": "request",
    "llm_request": "request",
    "prompt": "request",
    "model_response": "response",
    "llm_response": "response",
    "assistant": "response",
    "tool_use": "tool_call",
    "tool_invocation": "tool_call",
    "call_tool": "tool_call",
    "tool": "tool_call",
    "tool_return": "tool_result",
    "tool_completion": "tool_result",
    "tool_completed": "tool_result",
    "tool_error": "tool_result",
    "episode_end": "run_end",
    "end": "run_end",
    "finished": "run_end",
    "run_finished": "run_end",
    "terminal": "run_end",
    "failure": "error",
}


def _kind(event: TraceEvent) -> str:
    raw = str(event.kind).strip().lower().replace("-", "_").replace(" ", "_")
    if raw == "tool":
        phase = str(event.data.get("phase", event.data.get("status", ""))).lower()
        if phase in {"result", "completed", "complete", "error", "failed", "failure"}:
            return "tool_result"
    return _KIND_ALIASES.get(raw, raw)


def _text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if value is None:
        return ""
    try:
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    except (TypeError, ValueError):
        return str(value)


def _stable(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _first(data: Mapping[str, Any], *names: str) -> Any:
    for name in names:
        if name in data and data[name] is not None:
            return data[name]
    return None


def _tool_container(data: Mapping[str, Any]) -> Mapping[str, Any]:
    nested = data.get("tool")
    return nested if isinstance(nested, Mapping) else data


def _call_id(data: Mapping[str, Any], seq: int) -> str:
    raw = _first(data, "call_id", "tool_call_id", "id")
    if raw is None:
        nested = _tool_container(data)
        raw = _first(nested, "call_id", "tool_call_id", "id")
    return str(raw) if raw is not None and str(raw) else f"event-{seq}"


def _tool_name(data: Mapping[str, Any]) -> str:
    nested = _tool_container(data)
    raw = _first(data, "name", "tool_name")
    if raw is None:
        raw = _first(nested, "name", "tool_name")
    return str(raw) if raw is not None else "<unknown>"


def _tool_arguments(data: Mapping[str, Any]) -> Any:
    for source in (data, _tool_container(data)):
        raw = _first(source, "arguments", "input", "args", "parameters", "params")
        if raw is not None:
            return raw
    return None


def _error_value(data: Mapping[str, Any], kind: str) -> Any:
    raw = _first(data, "error", "exception", "failure")
    if raw is not None:
        return raw
    status = str(data.get("status", "")).lower()
    if kind == "tool_result" and status in {"error", "failed", "failure", "rejected"}:
        return data.get("message", status)
    if data.get("ok") is False or data.get("success") is False:
        return data.get("message", "tool failed")
    return None


def _explicit_error_class(error: Any, data: Mapping[str, Any]) -> str:
    candidates: list[Any] = []
    if isinstance(error, Mapping):
        candidates.extend(error.get(key) for key in ("class", "error_class", "category", "kind", "type", "code"))
    candidates.extend(data.get(key) for key in ("error_class", "error_type", "error_category"))
    for candidate in candidates:
        if candidate is not None and str(candidate).strip():
            return str(candidate).strip().lower().replace("-", "_").replace(" ", "_")
    return ""


def classify_tool_error(error: Any, data: Optional[Mapping[str, Any]] = None) -> str:
    """Classify an error into a small stable vocabulary.

    Explicit adapter classifications win; otherwise common words in the
    provider/runtime diagnostic are mapped.  Unknown diagnostics remain
    ``unknown`` rather than being guessed as execution failures.
    """

    body = data or {}
    explicit = _explicit_error_class(error, body)
    text = f"{explicit} {_text(error)} {_text(body.get('message', ''))}".lower()
    if any(token in text for token in ("cancel", "aborted", "abort", "interrupt")):
        return "cancelled"
    if any(token in text for token in ("timeout", "timed_out", "deadline", "deadline_exceeded")):
        return "timeout"
    if any(token in text for token in ("validation", "invalid_argument", "invalid_args", "schema", "malformed")):
        return "validation"
    if any(token in text for token in ("unknown_tool", "tool_not_found", "not_found", "no_such_tool")):
        return "unknown_tool"
    if any(token in text for token in ("permission", "forbidden", "access_denied", "unauthorized")):
        return "permission"
    if any(token in text for token in ("transport", "network", "connection", "socket", "http_", "rate_limit")):
        return "transport"
    if any(token in text for token in ("provider", "model_error", "upstream")):
        return "provider"
    if any(token in text for token in ("execution", "exception", "runtime", "command_failed", "exit_code")):
        return "execution"
    return "unknown"


def _is_retry(data: Mapping[str, Any]) -> bool:
    for key in ("is_retry", "retry"):
        value = data.get(key)
        if isinstance(value, Mapping):
            if value.get("is_retry") is True or value.get("attempt", 1) not in (0, 1):
                return True
        elif value is True:
            return True
    attempt = data.get("attempt", data.get("retry_attempt"))
    return isinstance(attempt, int) and not isinstance(attempt, bool) and attempt > 1


def _status(data: Mapping[str, Any]) -> str:
    raw = _first(data, "status", "reason", "outcome", "terminal_status")
    if isinstance(raw, Mapping):
        raw = _first(raw, "status", "reason")
    text = str(raw or "completed").lower().replace("-", "_").replace(" ", "_")
    if text in {"ok", "success", "successful", "done", "complete", "completed"}:
        return "completed"
    if text in {"cancel", "cancelled", "canceled", "interrupt", "interrupted"}:
        return "cancelled"
    if text in {"abort", "aborted"}:
        return "aborted"
    if text in {"fail", "failed", "failure", "error"}:
        return "failed"
    return text or "unknown"


def _timestamp(data: Mapping[str, Any]) -> Optional[float]:
    value = _first(data, "timestamp_ms", "timestamp", "at_ms", "time_ms")
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else None


@dataclass
class TraceMetrics:
    schema_version: str = METRICS_SCHEMA_VERSION
    event_count: int = 0
    request_count: int = 0
    response_count: int = 0
    tool_call_count: int = 0
    tool_result_count: int = 0
    tool_success_count: int = 0
    tool_error_count: int = 0
    tool_error_classes: dict[str, int] = field(default_factory=dict)
    retry_count: int = 0
    retry_call_ids: list[str] = field(default_factory=list)
    call_order: list[str] = field(default_factory=list)
    result_order: list[str] = field(default_factory=list)
    out_of_order_results: list[str] = field(default_factory=list)
    order_preserved: bool = True
    request_fingerprints: list[str] = field(default_factory=list)
    lifecycle: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        errors = {key: self.tool_error_classes[key] for key in sorted(self.tool_error_classes)}
        return {
            "schema_version": self.schema_version,
            "event_count": self.event_count,
            "request_count": self.request_count,
            "response_count": self.response_count,
            "tool_call_count": self.tool_call_count,
            "tool_result_count": self.tool_result_count,
            "tool_success_count": self.tool_success_count,
            "tool_error_count": self.tool_error_count,
            "tool_error_classes": errors,
            "tool_errors": {"total": self.tool_error_count, "by_class": errors},
            "retry_count": self.retry_count,
            "retry_call_ids": list(self.retry_call_ids),
            "retries": {"count": self.retry_count, "call_ids": list(self.retry_call_ids)},
            "call_order": list(self.call_order),
            "result_order": list(self.result_order),
            "order": {
                "calls": list(self.call_order),
                "results": list(self.result_order),
                "out_of_order_results": list(self.out_of_order_results),
                "preserved": self.order_preserved,
            },
            "out_of_order_results": list(self.out_of_order_results),
            "order_preserved": self.order_preserved,
            "request_fingerprints": list(self.request_fingerprints),
            "lifecycle": dict(self.lifecycle),
        }


def extract_metrics(trace: TraceArtifact | Mapping[str, Any] | list[Mapping[str, Any]]) -> TraceMetrics:
    """Extract deterministic counters and ordering/lifecycle evidence."""

    artifact = coerce_trace(trace)
    metrics = TraceMetrics(event_count=len(artifact.events))
    calls: dict[str, dict[str, Any]] = {}
    key_calls: dict[str, list[str]] = {}
    failed_keys: set[str] = set()
    explicit_retry_ids: list[str] = []
    result_call_ids: list[str] = []
    starts: list[int] = []
    ends: list[int] = []
    lifecycle_status = "running"
    start_time: Optional[float] = None
    finish_time: Optional[float] = None

    for event in artifact.events:
        kind = _kind(event)
        data = event.data
        if kind == "request":
            metrics.request_count += 1
            request = data.get("request") if isinstance(data.get("request"), Mapping) else data
            if isinstance(request, Mapping):
                metrics.request_fingerprints.append(request_semantic_fingerprint(request))
        elif kind == "response":
            metrics.response_count += 1
        elif kind == "tool_call":
            metrics.tool_call_count += 1
            call_id = _call_id(data, event.seq)
            name = _tool_name(data)
            arguments = _tool_arguments(data)
            key = _stable([name, arguments])
            explicit_retry = _is_retry(data) or data.get("retry_of") is not None
            retry = explicit_retry or key in failed_keys
            if retry and call_id not in metrics.retry_call_ids:
                metrics.retry_count += 1
                metrics.retry_call_ids.append(call_id)
            calls[call_id] = {"key": key, "failed": False, "seq": event.seq}
            key_calls.setdefault(key, []).append(call_id)
            metrics.call_order.append(call_id)
            if data.get("retry_of") is not None:
                explicit_retry_ids.append(str(data["retry_of"]))
        elif kind == "tool_result":
            metrics.tool_result_count += 1
            call_id = _call_id(data, event.seq)
            # Result records in some adapters only carry name/arguments.  Pair
            # those with the newest still-unsettled matching call.
            if call_id not in calls:
                name = _tool_name(data)
                key = _stable([name, _tool_arguments(data)])
                candidates = key_calls.get(key, [])
                unsettled = [candidate for candidate in candidates if not calls[candidate].get("settled")]
                if unsettled:
                    call_id = unsettled[-1]
            metrics.result_order.append(call_id)
            result_call_ids.append(call_id)
            error = _error_value(data, kind)
            if error is None:
                metrics.tool_success_count += 1
            else:
                metrics.tool_error_count += 1
                category = classify_tool_error(error, data)
                metrics.tool_error_classes[category] = metrics.tool_error_classes.get(category, 0) + 1
                if call_id in calls:
                    calls[call_id]["failed"] = True
                    failed_keys.add(calls[call_id]["key"])
            if call_id in calls:
                calls[call_id]["settled"] = True
        elif kind == "run_start":
            starts.append(event.seq)
            start_time = _timestamp(data)
        elif kind == "run_end":
            ends.append(event.seq)
            finish_time = _timestamp(data)
            lifecycle_status = _status(data)
        elif kind == "error":
            # Runtime/model errors are represented in lifecycle evidence but
            # are not tool errors unless they arrived as a tool_result.
            lifecycle_status = "failed"

    # An explicit retry_of can identify a retry even when the first call was
    # omitted from the trace (for example, a provider reconnect artifact).
    for retry_of in explicit_retry_ids:
        if retry_of not in metrics.retry_call_ids:
            metrics.retry_call_ids.append(retry_of)
            metrics.retry_count += 1

    known_call_order = [call_id for call_id in metrics.call_order if call_id in set(result_call_ids)]
    known_result_order = [call_id for call_id in metrics.result_order if call_id in set(metrics.call_order)]
    metrics.out_of_order_results = _out_of_order(known_call_order, known_result_order)
    metrics.order_preserved = known_call_order == known_result_order

    if starts and starts[0] == artifact.events[0].seq and len(starts) == 1 and len(ends) == 1 and ends[0] == artifact.events[-1].seq:
        lifecycle_valid = True
        lifecycle_reason = "valid"
    elif not artifact.events:
        lifecycle_valid = False
        lifecycle_reason = "empty"
    elif not starts or not ends:
        lifecycle_valid = False
        lifecycle_reason = "missing_boundary"
    elif len(starts) != 1 or len(ends) != 1:
        lifecycle_valid = False
        lifecycle_reason = "duplicate_boundary"
    elif starts[0] != artifact.events[0].seq:
        lifecycle_valid = False
        lifecycle_reason = "start_not_first"
    else:
        lifecycle_valid = False
        lifecycle_reason = "end_not_last"
    if not ends and lifecycle_status == "running":
        lifecycle_status = "running"
    metrics.lifecycle = {
        "valid": lifecycle_valid,
        "reason": lifecycle_reason,
        "status": lifecycle_status,
        "started": bool(starts),
        "ended": bool(ends),
        "start_count": len(starts),
        "end_count": len(ends),
    }
    if start_time is not None:
        metrics.lifecycle["started_at_ms"] = start_time
    if finish_time is not None:
        metrics.lifecycle["finished_at_ms"] = finish_time
    if start_time is not None and finish_time is not None:
        metrics.lifecycle["duration_ms"] = finish_time - start_time
    return metrics


def _out_of_order(expected: list[str], actual: list[str]) -> list[str]:
    if not expected or not actual:
        return []
    expected_positions = {call_id: index for index, call_id in enumerate(expected)}
    previous = -1
    out: list[str] = []
    for call_id in actual:
        position = expected_positions[call_id]
        if position < previous:
            out.append(call_id)
        previous = max(previous, position)
    return out


metrics_for_trace = extract_metrics

