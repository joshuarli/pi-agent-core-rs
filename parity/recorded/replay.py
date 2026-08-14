#!/usr/bin/env python3
"""Validate and replay the checked-in OpenRouter recording without a provider.

This adapter consumes the upstream-oriented recording as immutable evidence and emits one
canonical parity result.  It deliberately has no network, subprocess, credential, or Pi CLI
authority.  The pin and package version are read from ``parity/UPSTREAM_COMMIT`` so a capture
cannot silently drift away from the checked-out upstream target.
"""

from __future__ import annotations

import argparse
from datetime import date
import json
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_FIXTURE = (
    ROOT
    / "parity"
    / "fixtures"
    / "recorded"
    / "openrouter"
    / "inclusionai-ling-3.0-tiny-free-unavailable.json"
)
DEFAULT_EXPECTED = DEFAULT_FIXTURE.with_name(
    "inclusionai-ling-3.0-tiny-free-unavailable.canonical.json"
)
UPSTREAM_PIN = ROOT / "parity" / "UPSTREAM_COMMIT"

EXPECTED_KIND = "recorded_pi_sdk_terminal_response"
EXPECTED_RUNNER = "pinned-source-agent-sdk"
EXPECTED_REDACTIONS = (
    "OPENROUTER_API_KEY",
    "authorization headers",
    "session id",
    "timestamps",
)
EVENT_TYPES = {
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_end",
    "message_update",
}
ROLES = {"user", "assistant", "toolResult", "tool_result"}
CONTENT_TYPES = {"text", "thinking", "toolCall", "tool_call", "image", "json"}
OPENROUTER_PRIVACY_404_HINT = (
    "OpenRouter excluded this model under privacy/data-policy guardrails. An account restricted "
    "to Zero Data Retention models can receive this 404 when the selected model has no eligible "
    "Zero Data Retention endpoint; check the OpenRouter privacy policy before treating it as an "
    "SDK or model-name failure."
)


class ContractError(ValueError):
    """The recording or upstream pin violates the recorded-evidence contract."""


def _object(value: Any, path: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{path} must be an object")
    return value


def _exact_keys(value: dict[str, Any], expected: set[str], path: str) -> None:
    actual = set(value)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        detail = []
        if missing:
            detail.append(f"missing {', '.join(missing)}")
        if extra:
            detail.append(f"unexpected {', '.join(extra)}")
        raise ContractError(f"{path}: {'; '.join(detail)}")


def _string(value: Any, path: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        raise ContractError(f"{path} must be a {'possibly empty' if allow_empty else 'non-empty'} string")
    return value


def _nonnegative_number(value: Any, path: str) -> int | float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ContractError(f"{path} must be a non-negative number")
    return value


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read JSON recording {path}: {error}") from error
    return _object(value, str(path))


def _upstream_pin() -> tuple[str, str]:
    try:
        text = UPSTREAM_PIN.read_text(encoding="utf-8")
    except OSError as error:
        raise ContractError(f"cannot read upstream pin {UPSTREAM_PIN}: {error}") from error
    commit_match = re.search(r"^Commit:\s*`([0-9a-f]{40})`\s*$", text, re.MULTILINE)
    version_match = re.search(
        r"^\* `@earendil-works/pi-agent-core`:\s*`([^`]+)`\s*$", text, re.MULTILINE
    )
    if commit_match is None or version_match is None:
        raise ContractError("UPSTREAM_COMMIT is missing the pinned commit or agent package version")
    return commit_match.group(1), version_match.group(1)


def _validate_content_part(value: Any, path: str) -> dict[str, Any]:
    part = _object(value, path)
    part_type = _string(part.get("type"), f"{path}.type")
    if part_type not in CONTENT_TYPES:
        raise ContractError(f"{path}.type has unsupported value {part_type!r}")
    if part_type == "text":
        _exact_keys(part, {"type", "text"}, path)
        _string(part["text"], f"{path}.text", allow_empty=True)
    elif part_type == "thinking":
        # OpenRouter's DeepSeek stream uses the SDK's provider-shaped `thinking` field and a
        # `thinkingSignature`; the canonical result below deliberately removes that provider
        # spelling while retaining the thought text as the protocol `text` field.
        _exact_keys(part, {"type", "thinking", "thinkingSignature"}, path)
        _string(part["thinking"], f"{path}.thinking", allow_empty=True)
        _string(part["thinkingSignature"], f"{path}.thinkingSignature", allow_empty=True)
    elif part_type in {"toolCall", "tool_call"}:
        # The current capture is text-only.  Validate the stable shape if a future recording
        # includes a call, but do not invent a replay interpretation for provider-specific keys.
        _exact_keys(part, {"type", "id", "name", "arguments"}, path)
        _string(part["id"], f"{path}.id")
        _string(part["name"], f"{path}.name")
        _object(part["arguments"], f"{path}.arguments")
    elif part_type == "image":
        raise ContractError(f"{path}: image content is outside this recorded V0 adapter")
    else:
        _exact_keys(part, {"type", "value"}, path)
    return part


def _validate_usage(value: Any, path: str) -> None:
    usage = _object(value, path)
    required = {"input", "output", "cacheRead", "cacheWrite", "totalTokens", "cost"}
    optional = {"reasoning"}
    actual = set(usage)
    missing = sorted(required - actual)
    extra = sorted(actual - required - optional)
    if missing or extra:
        detail = []
        if missing:
            detail.append(f"missing {', '.join(missing)}")
        if extra:
            detail.append(f"unexpected {', '.join(extra)}")
        raise ContractError(f"{path}: {'; '.join(detail)}")
    for key in ("input", "output", "cacheRead", "cacheWrite", "totalTokens"):
        _nonnegative_number(usage[key], f"{path}.{key}")
    if "reasoning" in usage:
        _nonnegative_number(usage["reasoning"], f"{path}.reasoning")
    cost = _object(usage["cost"], f"{path}.cost")
    _exact_keys(cost, {"input", "output", "cacheRead", "cacheWrite", "total"}, f"{path}.cost")
    for key, amount in cost.items():
        _nonnegative_number(amount, f"{path}.cost.{key}")


def validate_recording(recording: dict[str, Any], *, source: Path) -> None:
    """Validate a recording and its provenance/redaction contract."""

    _exact_keys(recording, {"format_version", "kind", "capture", "request", "events", "assistant"}, "recording")
    if recording["format_version"] != 1:
        raise ContractError("recording.format_version must be 1")
    if recording["kind"] != EXPECTED_KIND:
        raise ContractError(f"recording.kind must be {EXPECTED_KIND!r}")

    pinned_commit, pinned_version = _upstream_pin()
    capture = _object(recording["capture"], "capture")
    _exact_keys(
        capture,
        {
            "pi_agent_core_version",
            "pi_commit",
            "captured_on",
            "capture_runner",
            "provider",
            "model",
            "redaction",
        },
        "capture",
    )
    if capture["pi_agent_core_version"] != pinned_version:
        raise ContractError(
            "capture.pi_agent_core_version does not match the version recorded in UPSTREAM_COMMIT "
            f"({pinned_version})"
        )
    if capture["pi_commit"] != pinned_commit:
        raise ContractError("capture.pi_commit does not match the commit recorded in UPSTREAM_COMMIT")
    try:
        date.fromisoformat(_string(capture["captured_on"], "capture.captured_on"))
    except ValueError as error:
        raise ContractError("capture.captured_on must be an ISO-8601 date") from error
    if capture["capture_runner"] != EXPECTED_RUNNER:
        raise ContractError(f"capture.capture_runner must be {EXPECTED_RUNNER!r}")
    provider = _string(capture["provider"], "capture.provider")
    model = _string(capture["model"], "capture.model")
    redaction = _object(capture["redaction"], "capture.redaction")
    _exact_keys(redaction, {"removed"}, "capture.redaction")
    removed = redaction["removed"]
    if removed != list(EXPECTED_REDACTIONS):
        raise ContractError("capture.redaction.removed must enumerate the required redactions in order")

    request = _object(recording["request"], "request")
    _exact_keys(request, {"system_prompt", "user_text"}, "request")
    system_prompt = _string(request["system_prompt"], "request.system_prompt", allow_empty=True)
    user_text = _string(request["user_text"], "request.user_text", allow_empty=True)

    events = recording["events"]
    if not isinstance(events, list) or not events:
        raise ContractError("events must be a non-empty array")
    for index, value in enumerate(events):
        event = _object(value, f"events[{index}]")
        event_type = _string(event.get("type"), f"events[{index}].type")
        if event_type not in EVENT_TYPES:
            raise ContractError(f"events[{index}].type has unsupported value {event_type!r}")
        allowed = {"type"}
        if event_type in {"message_start", "message_end"}:
            allowed.add("role")
            role = _string(event.get("role"), f"events[{index}].role")
            if role not in ROLES:
                raise ContractError(f"events[{index}].role has unsupported value {role!r}")
        elif event_type == "message_update" and "role" in event:
            allowed.add("role")
            role = _string(event["role"], f"events[{index}].role")
            if role not in ROLES:
                raise ContractError(f"events[{index}].role has unsupported value {role!r}")
        if event_type == "turn_end":
            allowed.add("stop_reason")
            _string(event.get("stop_reason"), f"events[{index}].stop_reason")
        _exact_keys(event, allowed, f"events[{index}]")

    assistant = _object(recording["assistant"], "assistant")
    _exact_keys(
        assistant,
        {"api", "provider", "model", "stop_reason", "error_message", "content", "usage"},
        "assistant",
    )
    if assistant["provider"] != provider:
        raise ContractError("assistant.provider must match capture.provider")
    if assistant["model"] != model:
        raise ContractError("assistant.model must match capture.model")
    _string(assistant["api"], "assistant.api")
    stop_reason = _string(assistant["stop_reason"], "assistant.stop_reason")
    if stop_reason == "error" and not isinstance(assistant["error_message"], str):
        raise ContractError("assistant.error_message must be a string for an error response")
    if stop_reason != "error" and assistant["error_message"] is not None:
        raise ContractError("assistant.error_message must be null unless stop_reason is error")
    if not isinstance(assistant["content"], list):
        raise ContractError("assistant.content must be an array")
    for index, part in enumerate(assistant["content"]):
        validated_part = _validate_content_part(part, f"assistant.content[{index}]")
        if validated_part["type"] not in {"text", "thinking"}:
            raise ContractError(
                "assistant.content currently supports only text/thinking parts; "
                "a tool/image recording needs a separately scoped adapter"
            )
    _validate_usage(assistant["usage"], "assistant.usage")

    # This recording is intentionally a single no-tool prompt.  Keep the replay contract narrow
    # and reject a capture that would require fabricating message/tool state.
    expected_prefix = [
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "message_start",
    ]
    expected_suffix = [
        "message_end",
        "turn_end",
        "agent_end",
    ]
    actual_event_types = [event["type"] for event in events]
    message_updates = actual_event_types[len(expected_prefix) : -len(expected_suffix)]
    if (
        actual_event_types[: len(expected_prefix)] != expected_prefix
        or actual_event_types[-len(expected_suffix) :] != expected_suffix
        or any(event_type != "message_update" for event_type in message_updates)
    ):
        raise ContractError(
            "recorded V0 replay supports one user/assistant turn with optional assistant "
            f"message updates; event sequence was {actual_event_types!r}"
        )
    if events[2]["role"] != "user" or events[3]["role"] != "user":
        raise ContractError("the first recorded message must be the user message")
    if events[4]["role"] != "assistant" or events[-3]["role"] != "assistant":
        raise ContractError("the second recorded message must be the assistant message")
    if events[-2]["stop_reason"] != assistant["stop_reason"]:
        raise ContractError("turn_end.stop_reason must match assistant.stop_reason")
    if system_prompt == "" or user_text == "":
        raise ContractError("the recorded request must contain non-empty prompt text")


def _canonical_content(content: list[dict[str, Any]]) -> list[dict[str, Any]]:
    result = []
    for part in content:
        part_type = part["type"]
        if part_type == "toolCall":
            part_type = "tool_call"
        if part_type == "thinking":
            result.append({"type": "thinking", "text": part["thinking"]})
        else:
            result.append({"type": part_type, **{key: value for key, value in part.items() if key != "type"}})
    return result


def _error_hint(capture: dict[str, Any], assistant: dict[str, Any]) -> str | None:
    """Return a non-authoritative remediation hint for a known provider policy response.

    The raw provider diagnostic stays in ``error.message``. This hint is deliberately separate:
    it identifies a likely account-policy cause without claiming that the pinned Pi SDK inferred
    it or that changing the model alone will resolve the restriction.
    """

    message = assistant["error_message"]
    if (
        capture["provider"] == "openrouter"
        and isinstance(message, str)
        and "404" in message
        and "privacy" in message.lower()
        and "guardrail" in message.lower()
    ):
        return OPENROUTER_PRIVACY_404_HINT
    return None


def replay(recording: dict[str, Any], *, source: Path) -> dict[str, Any]:
    """Return the canonical, provider-free result for a validated recording."""

    validate_recording(recording, source=source)
    capture = recording["capture"]
    request = recording["request"]
    assistant = recording["assistant"]
    stop_reason = assistant["stop_reason"]
    outcome = "error" if stop_reason == "error" else "completed"
    events = []
    turn = 0
    for event in recording["events"]:
        event_type = event["type"]
        if event_type == "agent_start" or event_type == "agent_end":
            data: dict[str, Any] = {}
        elif event_type == "turn_start":
            data = {"turn": turn}
            turn += 1
        elif event_type in {"message_start", "message_end"}:
            role = "tool_result" if event["role"] == "toolResult" else event["role"]
            data = {"role": role}
        elif event_type == "message_update":
            if "role" in event:
                role = "tool_result" if event["role"] == "toolResult" else event["role"]
                data = {"role": role}
            else:
                data = {}
        else:
            data = {"stop_reason": event["stop_reason"]}
        events.append({"seq": len(events), "type": event_type, "data": data})
    events.append({"seq": len(events), "type": "agent_settled", "data": {"outcome": outcome}})
    error = None
    if outcome == "error":
        error = {"kind": "model", "message": assistant["error_message"], "retryable": None}
        if hint := _error_hint(capture, assistant):
            error["hint"] = hint
    return {
        "format_version": 1,
        "kind": "canonical_parity_result",
        "fixture_id": source.stem,
        "outcome": outcome,
        "settled": True,
        "state": {
            "system_prompt": request["system_prompt"],
            "model": {"provider": capture["provider"], "id": capture["model"]},
            "thinking_level": "off",
            "tool_names": [],
            "pending_tool_calls": [],
        },
        "events": events,
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": request["user_text"]}]},
            {"role": "assistant", "content": _canonical_content(assistant["content"])},
        ],
        "last_response": {"api": assistant["api"], "stop_reason": stop_reason},
        "usage": {
            "input": assistant["usage"]["input"],
            "output": assistant["usage"]["output"],
            "cache_read": assistant["usage"]["cacheRead"],
            "cache_write": assistant["usage"]["cacheWrite"],
            "total_tokens": assistant["usage"]["totalTokens"],
        },
        "error": error,
    }


def _canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", nargs="?", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument(
        "--check",
        action="store_true",
        help="also compare the replay with the checked-in canonical result",
    )
    parser.add_argument(
        "--expected",
        type=Path,
        default=DEFAULT_EXPECTED,
        help="canonical result to compare when --check is supplied",
    )
    args = parser.parse_args(argv)
    fixture = args.fixture.resolve()
    try:
        result = replay(_read_json(fixture), source=fixture)
        if args.check:
            expected_path = args.expected.resolve()
            expected = _read_json(expected_path)
            if result != expected:
                raise ContractError(f"replay differs from checked-in canonical result {expected_path}")
            print(f"ok {fixture.name}: pinned source, redaction, and canonical replay")
        else:
            sys.stdout.buffer.write(_canonical_bytes(result))
    except ContractError as error:
        print(f"recorded replay contract error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
