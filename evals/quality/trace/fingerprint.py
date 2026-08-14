"""Stable semantic fingerprints for model requests.

Only transport-generated bookkeeping is removed.  Object-key order is
canonicalized, while every array remains in its input order.  In particular,
the order and complete fields of ``messages`` and ``tools`` are part of the
fingerprint; sorting tools or reducing them to names can hide a real parity
regression.
"""

from __future__ import annotations

from collections.abc import Mapping
import copy
import hashlib
import json
from typing import Any


# These fields identify a transport envelope or measurement rather than the
# request's meaning.  The list is intentionally narrow.  Generic ``id`` and
# all fields nested inside tool definitions are retained because they can
# affect provider semantics and are useful when diagnosing a diff.
NON_SEMANTIC_REQUEST_FIELDS = frozenset(
    {
        "request_id",
        "trace_id",
        "span_id",
        "parent_span_id",
        "timestamp",
        "timestamp_ms",
        "started_at",
        "started_at_ms",
        "finished_at",
        "finished_at_ms",
        "duration_ms",
        "elapsed_ms",
    }
)


def _normalize(value: Any, *, root: bool = False) -> Any:
    if isinstance(value, Mapping):
        output: dict[str, Any] = {}
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError("request objects require string keys")
            # A transport envelope's metadata is not a semantic request.  Do
            # not recurse with root=True: nested tool/message fields called
            # ``metadata`` or ``timestamp`` are preserved unless they are one
            # of the explicit, globally non-semantic transport fields above.
            if root and key in NON_SEMANTIC_REQUEST_FIELDS:
                continue
            output[key] = _normalize(copy.deepcopy(item), root=False)
        return {key: output[key] for key in sorted(output)}
    if isinstance(value, (list, tuple)):
        # Deliberately preserve list order.  Never use sorted() here.
        return [_normalize(copy.deepcopy(item), root=False) for item in value]
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    raise TypeError(f"request value of type {type(value).__name__} is not JSON-compatible")


def normalize_request(request: Mapping[str, Any]) -> dict[str, Any]:
    """Return a detached, deterministic request representation.

    The function accepts either a request object directly or an adapter's
    ``{"request": {...}}`` envelope.  The envelope is not unwrapped: keeping
    its shape makes a fingerprint explainable in a report, and callers that
    need just the nested request can pass that object explicitly.
    """

    if not isinstance(request, Mapping):
        raise TypeError("request must be a JSON object")
    normalized = _normalize(request, root=True)
    if not isinstance(normalized, dict):  # pragma: no cover - _normalize guarantees this
        raise TypeError("normalized request must be an object")
    return normalized


def normalized_request_json(request: Mapping[str, Any]) -> str:
    """Serialize a normalized request with stable bytes suitable for hashing."""

    return json.dumps(
        normalize_request(request),
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )


def request_semantic_fingerprint(request: Mapping[str, Any]) -> str:
    """Return the SHA-256 fingerprint of a normalized request as 64 hex chars."""

    payload = normalized_request_json(request).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


# Friendly aliases for adapters and callers that used the shorter term first.
request_fingerprint = request_semantic_fingerprint
fingerprint_request = request_semantic_fingerprint
