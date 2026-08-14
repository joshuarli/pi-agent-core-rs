"""Focused contract tests for the dependency-free quality trace layer."""

from __future__ import annotations

import json
import unittest

from .diff import diff_traces
from .fingerprint import normalize_request, request_semantic_fingerprint
from .metrics import extract_metrics
from .report import human_report, report_json
from .schema import TRACE_SCHEMA_VERSION, TraceArtifact, TraceEvent, trace_from_events


def _trace(*events: dict) -> TraceArtifact:
    return trace_from_events(events, trace_id="test")


class CanonicalTraceTests(unittest.TestCase):
    def test_schema_assigns_sequence_and_retains_unknown_tool_fields(self) -> None:
        trace = _trace(
            {"kind": "run_start", "run": "x"},
            {
                "kind": "tool_call",
                "call_id": "a",
                "name": "read",
                "arguments": {"path": "a.txt"},
                "custom_tool_field": ["retained", 1],
            },
            {"kind": "run_end", "status": "completed"},
        )
        self.assertEqual(trace.schema_version, TRACE_SCHEMA_VERSION)
        self.assertEqual([event.seq for event in trace.events], [0, 1, 2])
        self.assertEqual(trace.to_dict()["events"][1]["custom_tool_field"], ["retained", 1])
        self.assertEqual(TraceArtifact.from_dict(json.loads(trace.canonical_json())), trace)


class FingerprintTests(unittest.TestCase):
    def test_object_key_order_and_transport_ids_do_not_change_fingerprint(self) -> None:
        first = {
            "request_id": "one",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "read", "description": "Read", "parameters": {"type": "object"}}],
        }
        second = {
            "timestamp_ms": 99,
            "tools": [{"parameters": {"type": "object"}, "description": "Read", "name": "read"}],
            "messages": [{"content": "hi", "role": "user"}],
            "request_id": "two",
        }
        self.assertEqual(request_semantic_fingerprint(first), request_semantic_fingerprint(second))

    def test_message_and_tool_order_and_tool_fields_are_semantic(self) -> None:
        base = {"messages": [{"role": "user", "content": "one"}, {"role": "user", "content": "two"}], "tools": [{"name": "a", "parameters": {}}, {"name": "b", "parameters": {}}]}
        reordered_messages = {**base, "messages": list(reversed(base["messages"]))}
        reordered_tools = {**base, "tools": list(reversed(base["tools"]))}
        changed_tool = {**base, "tools": [{"name": "a", "parameters": {"required": ["x"]}}, base["tools"][1]]}
        self.assertNotEqual(request_semantic_fingerprint(base), request_semantic_fingerprint(reordered_messages))
        self.assertNotEqual(request_semantic_fingerprint(base), request_semantic_fingerprint(reordered_tools))
        self.assertNotEqual(request_semantic_fingerprint(base), request_semantic_fingerprint(changed_tool))
        # A nested tool timestamp is a field, not a transport envelope field.
        with_timestamp = {**base, "tools": [{**base["tools"][0], "timestamp_ms": 1}, base["tools"][1]]}
        self.assertNotEqual(request_semantic_fingerprint(base), request_semantic_fingerprint(with_timestamp))
        self.assertEqual(normalize_request(base)["messages"][0]["content"], "one")


class DiffTests(unittest.TestCase):
    def test_first_divergence_reports_kind_and_surrounding_context(self) -> None:
        left = _trace(
            {"kind": "run_start"},
            {"kind": "request", "request": {"messages": []}},
            {"kind": "tool_call", "call_id": "a", "name": "read", "arguments": {}},
            {"kind": "run_end"},
        )
        right = _trace(
            {"kind": "run_start"},
            {"kind": "request", "request": {"messages": []}},
            {"kind": "tool_result", "call_id": "a", "name": "read", "output": "ok"},
            {"kind": "run_end"},
        )
        difference = diff_traces(left, right, context=1)
        self.assertFalse(difference.equal)
        self.assertEqual(difference.first_divergence_index, 2)
        self.assertEqual(difference.divergence.reason, "kind")
        self.assertEqual(len(difference.left_context), 3)

    def test_trailing_event_is_a_missing_event_divergence(self) -> None:
        difference = diff_traces(_trace({"kind": "run_start"}), _trace({"kind": "run_start"}, {"kind": "run_end"}))
        self.assertEqual(difference.divergence.reason, "missing_left_event")


class MetricsTests(unittest.TestCase):
    def test_tool_errors_retries_completion_order_and_lifecycle(self) -> None:
        trace = _trace(
            {"kind": "run_start", "timestamp_ms": 10},
            {"kind": "request", "request": {"messages": [{"role": "user", "content": "go"}]}},
            {"kind": "tool_call", "call_id": "a", "name": "read", "arguments": {"path": "a"}},
            {"kind": "tool_call", "call_id": "b", "name": "read", "arguments": {"path": "b"}},
            {"kind": "tool_result", "call_id": "a", "name": "read", "error": {"class": "TimeoutError", "message": "deadline"}},
            {"kind": "tool_result", "call_id": "b", "name": "read", "output": "ok"},
            {"kind": "tool_call", "call_id": "c", "name": "read", "arguments": {"path": "a"}, "retry": True},
            {"kind": "tool_result", "call_id": "c", "name": "read", "output": "ok"},
            {"kind": "run_end", "status": "completed", "timestamp_ms": 20},
        )
        metrics = extract_metrics(trace)
        self.assertEqual(metrics.tool_error_classes, {"timeout": 1})
        self.assertEqual(metrics.retry_count, 1)
        self.assertEqual(metrics.retry_call_ids, ["c"])
        self.assertTrue(metrics.order_preserved)
        self.assertEqual(metrics.lifecycle["status"], "completed")
        self.assertEqual(metrics.lifecycle["duration_ms"], 10)

    def test_parallel_completion_order_is_visible(self) -> None:
        metrics = extract_metrics(
            _trace(
                {"kind": "run_start"},
                {"kind": "tool_call", "call_id": "a", "name": "x", "arguments": {}},
                {"kind": "tool_call", "call_id": "b", "name": "x", "arguments": {}},
                {"kind": "tool_call", "call_id": "c", "name": "x", "arguments": {}},
                {"kind": "tool_result", "call_id": "c", "name": "x", "output": "c"},
                {"kind": "tool_result", "call_id": "a", "name": "x", "output": "a"},
                {"kind": "tool_result", "call_id": "b", "name": "x", "output": "b"},
                {"kind": "run_end"},
            )
        )
        self.assertFalse(metrics.order_preserved)
        self.assertEqual(metrics.call_order, ["a", "b", "c"])
        self.assertEqual(metrics.result_order, ["c", "a", "b"])


class ReportTests(unittest.TestCase):
    def test_json_and_human_reports_are_deterministic_and_concise(self) -> None:
        left = _trace({"kind": "run_start"}, {"kind": "run_end"})
        right = _trace({"kind": "run_start"}, {"kind": "run_end", "status": "failed"})
        first = report_json(left, right)
        second = report_json(left, right)
        self.assertEqual(first, second)
        self.assertEqual(json.loads(first)["schema_version"], "pi-quality-trace-report/v1")
        text = human_report(left, right)
        self.assertIn("DIVERGED", text)
        self.assertIn("first divergence", text)


if __name__ == "__main__":
    unittest.main()

