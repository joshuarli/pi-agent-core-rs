"""Offline verification of a recorded deterministic core-quality artifact.

Replay does not call a provider or rerun either adapter.  It verifies that the
stored normalized model requests still produce the recorded semantic
fingerprints, then rebuilds the differential report from the stored traces.
That makes a checked-in or CI-retained artifact independently inspectable even
after the original temporary fixture directory is gone.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Mapping

from .trace import build_report, request_semantic_fingerprint


class ReplayError(ValueError):
    """A recorded core-quality artifact is incomplete or has been altered."""


def _read(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReplayError(f"cannot read replay artifact {path}: {error}") from error
    if not isinstance(value, dict):
        raise ReplayError("replay artifact must be a JSON object")
    return value


def _request_fingerprints(trace: Mapping[str, Any]) -> list[str]:
    events = trace.get("events")
    if not isinstance(events, list):
        raise ReplayError("trace has no event array")
    fingerprints: list[str] = []
    for event in events:
        if not isinstance(event, Mapping) or event.get("kind") != "request":
            continue
        request = event.get("request")
        if not isinstance(request, Mapping):
            raise ReplayError("request event has no normalized request object")
        fingerprints.append(request_semantic_fingerprint(request))
    return fingerprints


def replay_artifact(path: Path) -> dict[str, Any]:
    """Validate recorded request fingerprints and rebuild its trace report."""
    artifact = _read(path)
    traces = artifact.get("traces")
    if not isinstance(traces, Mapping):
        raise ReplayError("artifact has no normalized traces")
    upstream, rust = traces.get("upstream"), traces.get("rust")
    if not isinstance(upstream, Mapping) or not isinstance(rust, Mapping):
        raise ReplayError("artifact must contain upstream and rust traces")
    expected = artifact.get("recorded_request_fingerprints")
    if not isinstance(expected, Mapping):
        raise ReplayError("artifact has no recorded request fingerprints")
    observed = {"upstream": _request_fingerprints(upstream), "rust": _request_fingerprints(rust)}
    if observed != dict(expected):
        raise ReplayError("recorded request semantic fingerprints do not match the stored normalized requests")
    rebuilt = build_report(upstream, rust, left_label="upstream", right_label="rust")
    stored_report = artifact.get("report")
    if rebuilt != stored_report:
        raise ReplayError("stored differential report does not match a replay of the stored traces")
    return {
        "schema_version": "pi-agent-quality-replay/v1",
        "case_id": artifact.get("case_id"),
        "classification": artifact.get("classification"),
        "request_fingerprints": observed,
        "report_equal": rebuilt.get("equal"),
        "replayed": True,
    }
