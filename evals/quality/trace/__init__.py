"""Canonical quality-evaluation traces and differential reports.

The package intentionally uses only the Python standard library.  Adapters may
emit ordinary JSON objects and convert them at the boundary with
``coerce_trace``; the canonical representation then gives metrics, stable
fingerprints, and first-divergence reports one shared vocabulary.
"""

from .schema import (
    EVENT_KINDS,
    METRICS_SCHEMA_VERSION,
    REPORT_SCHEMA_VERSION,
    TRACE_SCHEMA_VERSION,
    CanonicalEvent,
    CanonicalTrace,
    Trace,
    TraceArtifact,
    TraceEvent,
    coerce_trace,
    event,
    trace_from_events,
)
from .fingerprint import (
    fingerprint_request,
    normalize_request,
    normalized_request_json,
    request_fingerprint,
    request_semantic_fingerprint,
)
from .metrics import TraceMetrics, extract_metrics, metrics_for_trace
from .diff import (
    Divergence,
    TraceDiff,
    diff_trace,
    diff_traces,
    first_divergence,
)
from .report import (
    build_report,
    human_report,
    json_report,
    report_json,
    trace_json,
)

__all__ = [
    "EVENT_KINDS",
    "TRACE_SCHEMA_VERSION",
    "METRICS_SCHEMA_VERSION",
    "REPORT_SCHEMA_VERSION",
    "CanonicalEvent",
    "CanonicalTrace",
    "Trace",
    "TraceArtifact",
    "TraceEvent",
    "coerce_trace",
    "event",
    "trace_from_events",
    "normalize_request",
    "normalized_request_json",
    "request_semantic_fingerprint",
    "request_fingerprint",
    "fingerprint_request",
    "TraceMetrics",
    "extract_metrics",
    "metrics_for_trace",
    "Divergence",
    "TraceDiff",
    "first_divergence",
    "diff_trace",
    "diff_traces",
    "build_report",
    "human_report",
    "json_report",
    "report_json",
    "trace_json",
]

