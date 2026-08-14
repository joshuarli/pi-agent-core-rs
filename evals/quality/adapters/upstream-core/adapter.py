#!/usr/bin/env python3
"""JSON adapter for the pinned upstream-core parity runner.

The adapter is intentionally a small process boundary.  It accepts one JSON
request on stdin and emits one JSON response on stdout.  The fixture is the
only input selected by the caller; the runner itself owns fixture validation
and canonical normalization.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any


PROTOCOL = "pi-agent-quality-adapter/v0"
ADAPTER = "upstream-core"
PINNED_COMMIT = "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf"
UPSTREAM_REPOSITORY = "https://github.com/earendil-works/pi.git"
UPSTREAM_PACKAGE = "@earendil-works/pi-agent-core"
UPSTREAM_PACKAGE_VERSION = "0.84.1"


class ContractError(Exception):
    """An invalid adapter request or unavailable pinned runner."""


def fail(message: str) -> int:
    print(f"{ADAPTER} adapter: {message}", file=sys.stderr)
    return 2


def repository_root() -> Path:
    # adapter.py lives at evals/quality/adapters/upstream-core/.
    return Path(__file__).resolve().parents[4]


def read_request() -> dict[str, Any]:
    raw = sys.stdin.read()
    if not raw.strip():
        raise ContractError("stdin must contain one JSON request object")
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ContractError(f"stdin is not valid JSON: {error.msg}") from error
    if not isinstance(value, dict):
        raise ContractError("request must be a JSON object")
    protocol = value.get("protocol", PROTOCOL)
    if protocol != PROTOCOL:
        raise ContractError(f"unsupported protocol {protocol!r}")
    if value.get("operation", "run") != "run":
        raise ContractError("operation must be 'run'")
    fixture = value.get("fixture")
    if not isinstance(fixture, str) or not fixture:
        raise ContractError("request.fixture must be a non-empty path string")
    return {"fixture": fixture}


def explicit_fixture(root: Path, value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        path = root / path
    path = path.resolve()
    if not path.is_file():
        raise ContractError(f"fixture is not a regular file: {path}")
    return path


def zero_usage() -> dict[str, int]:
    return {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0, "total_tokens": 0}


def translate_quality_case(
    path: Path,
) -> tuple[Path, tempfile.TemporaryDirectory[str] | None, str, list[dict[str, Any]]]:
    """Translate the quality-case ``emit`` vocabulary at the adapter boundary.

    The checked-in parity runner intentionally accepts the older closed
    ``chunks`` vocabulary.  Quality cases use provider-shaped start/delta/end
    records, so this adapter translates only the lossless subset needed by the
    deterministic cases and keeps the existing runner unchanged.
    """

    try:
        source = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"could not read quality fixture {path}: {error}") from error
    if not isinstance(source, dict):
        raise ContractError("fixture must be a JSON object")
    kind = source.get("kind")
    if kind == "declarative_parity_fixture":
        return path, None, kind, []
    if kind != "quality_core_case":
        raise ContractError(
            "fixture.kind must be declarative_parity_fixture or quality_core_case; "
            "quality_core_case uses model_script.emit and is translated by this adapter"
        )
    turns = source.get("model_script")
    if not isinstance(turns, list) or not turns:
        raise ContractError("quality_core_case.model_script must be a non-empty array")
    translated_turns: list[dict[str, Any]] = []
    expectations: list[dict[str, Any]] = []
    host = source.get("host", {"tools": []})
    if not isinstance(host, dict):
        raise ContractError("quality_core_case.host must be an object")
    unsupported_host = sorted(set(host) - {"tools", "before_tool_call", "after_tool_call", "should_stop_after_turn", "observer"})
    if unsupported_host:
        raise ContractError(
            "quality_core_case.host uses unsupported deterministic controls "
            f"{unsupported_host}; the existing parity runners have no barrier scheduler. "
            "Use a declarative parity fixture or add an adapter-owned translation for this control."
        )
    host_tools = host.get("tools", [])
    if not isinstance(host_tools, list):
        raise ContractError("quality_core_case.host.tools must be an array")
    for tool_index, raw_tool in enumerate(host_tools):
        if not isinstance(raw_tool, dict):
            raise ContractError(f"host.tools[{tool_index}] must be an object")
        calls = raw_tool.get("calls", [])
        if not isinstance(calls, list):
            raise ContractError(f"host.tools[{tool_index}].calls must be an array")
        for call_index, raw_call in enumerate(calls):
            if not isinstance(raw_call, dict):
                raise ContractError(f"host.tools[{tool_index}].calls[{call_index}] must be an object")
            unsupported_call = sorted(
                set(raw_call)
                - {"arguments", "result", "yield_once", "updates", "cancel_after_update", "enqueue_during_execution"}
            )
            if unsupported_call:
                raise ContractError(
                    f"host.tools[{tool_index}].calls[{call_index}] uses unsupported controls "
                    f"{unsupported_call}; barrier/cancellation scheduling is not in the existing parity runners"
                )
    for turn_index, raw_turn in enumerate(turns):
        if not isinstance(raw_turn, dict):
            raise ContractError(f"model_script[{turn_index}] must be an object")
        raw_expectation = raw_turn.get("expect_request")
        if raw_expectation is not None:
            if not isinstance(raw_expectation, dict):
                raise ContractError(f"model_script[{turn_index}].expect_request must be an object")
            ordinal = raw_expectation.get("ordinal")
            if ordinal != turn_index + 1:
                raise ContractError(
                    f"model_script[{turn_index}].expect_request.ordinal must be {turn_index + 1}"
                )
            roles = raw_expectation.get("message_roles")
            if roles is not None and (not isinstance(roles, list) or not all(isinstance(role, str) for role in roles)):
                raise ContractError(
                    f"model_script[{turn_index}].expect_request.message_roles must be an array of strings"
                )
            # Existing parity runners do not expose a request-role assertion
            # hook. Preserve it in response metadata rather than pretending it
            # was checked by the translated fixture.
            expectation = {"ordinal": ordinal}
            if roles is not None:
                expectation["message_roles"] = roles
            expectations.append(expectation)
        emits = raw_turn.get("emit")
        if not isinstance(emits, list) or not emits:
            raise ContractError(f"model_script[{turn_index}].emit must be a non-empty array")
        chunks: list[dict[str, Any]] = []
        calls: dict[str, dict[str, Any]] = {}
        for emit_index, raw_emit in enumerate(emits):
            if not isinstance(raw_emit, dict):
                raise ContractError(f"model_script[{turn_index}].emit[{emit_index}] must be an object")
            emit_type = raw_emit.get("type")
            if emit_type == "text_delta":
                text = raw_emit.get("text")
                if not isinstance(text, str):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].text must be a string")
                chunks.append({"kind": "text_delta", "text": text})
            elif emit_type == "tool_call_start":
                call_id = raw_emit.get("id")
                name = raw_emit.get("name")
                arguments = raw_emit.get("arguments", {})
                if not isinstance(call_id, str) or not call_id:
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].id must be non-empty")
                if not isinstance(name, str):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].name must be a string")
                if not isinstance(arguments, dict):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].arguments must be an object")
                call = {"kind": "tool_call", "id": call_id, "name": name, "arguments": arguments}
                calls[call_id] = call
                chunks.append(call)
            elif emit_type == "tool_call_delta":
                call_id = raw_emit.get("id")
                if not isinstance(call_id, str) or call_id not in calls:
                    raise ContractError(
                        f"model_script[{turn_index}].emit[{emit_index}] tool_call_delta references no prior tool_call_start"
                    )
                delta = raw_emit.get("arguments_delta", raw_emit.get("arguments", raw_emit.get("delta")))
                if isinstance(delta, dict):
                    calls[call_id]["arguments"].update(delta)
                elif isinstance(delta, str):
                    try:
                        parsed = json.loads(delta)
                    except json.JSONDecodeError as error:
                        raise ContractError(
                            f"model_script[{turn_index}].emit[{emit_index}].delta must be a complete JSON object"
                        ) from error
                    if not isinstance(parsed, dict):
                        raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].delta must decode to an object")
                    calls[call_id]["arguments"].update(parsed)
                else:
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}] delta must be an object or JSON string")
            elif emit_type == "tool_call_end":
                call_id = raw_emit.get("id")
                if not isinstance(call_id, str) or call_id not in calls:
                    raise ContractError(
                        f"model_script[{turn_index}].emit[{emit_index}] tool_call_end references no prior tool_call_start"
                    )
            elif emit_type == "done":
                stop_reason = raw_emit.get("stop_reason")
                if stop_reason == "tool_use":
                    stop_reason = "tool_call"
                if stop_reason not in {"stop", "tool_call", "length"}:
                    raise ContractError(
                        f"model_script[{turn_index}].emit[{emit_index}].stop_reason must be stop, tool_use, or length"
                    )
                usage = raw_emit.get("usage", zero_usage())
                if not isinstance(usage, dict):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].usage must be an object")
                chunks.append({"kind": "done", "stop_reason": stop_reason, "usage": usage})
            elif emit_type in {"stream_error", "error"}:
                reason = raw_emit.get("reason", "error")
                if reason in {"transport", "cancelled"}:
                    reason = "aborted" if reason == "cancelled" else "error"
                if reason not in {"error", "aborted"}:
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].reason must be error or aborted")
                message = raw_emit.get("message", "fixture stream error")
                if not isinstance(message, str):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].message must be a string")
                usage = raw_emit.get("usage", zero_usage())
                if not isinstance(usage, dict):
                    raise ContractError(f"model_script[{turn_index}].emit[{emit_index}].usage must be an object")
                chunks.append({"kind": "error", "reason": reason, "message": message, "usage": usage})
            else:
                raise ContractError(
                    f"model_script[{turn_index}].emit[{emit_index}] has unsupported type {emit_type!r}; "
                    "supported types are text_delta, tool_call_start/delta/end, done, stream_error"
                )
        if not chunks or chunks[-1].get("kind") not in {"done", "error"}:
            raise ContractError(f"model_script[{turn_index}].emit must end with done or stream_error")
        translated_turn = {"chunks": chunks}
        if "cancel_after" in raw_turn:
            translated_turn["cancel_after"] = raw_turn["cancel_after"]
        translated_turns.append(translated_turn)
    translated = {
        "format_version": 1,
        "kind": "declarative_parity_fixture",
        "id": source.get("id"),
        "description": source.get("description", "translated quality core case"),
        "setup": source.get("setup"),
        "actions": source.get("actions"),
        "model_script": translated_turns,
        "host": host,
    }
    if not isinstance(translated["id"], str) or not isinstance(translated["setup"], dict) or not isinstance(translated["actions"], list):
        raise ContractError("quality_core_case requires string id, object setup, and array actions")
    temp = tempfile.TemporaryDirectory(prefix="pi-quality-upstream-")
    translated_path = Path(temp.name) / f"{translated['id'].replace('/', '__')}.json"
    translated_path.write_text(json.dumps(translated), encoding="utf-8")
    return translated_path, temp, kind, expectations


def check_pin(source: Path) -> None:
    if not (source / ".git").exists():
        raise ContractError(f"pinned upstream checkout is missing: {source}")
    try:
        actual = subprocess.check_output(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            text=True,
            stderr=subprocess.PIPE,
        ).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise ContractError("could not inspect the pinned upstream checkout") from error
    if actual != PINNED_COMMIT:
        raise ContractError(f"upstream checkout is {actual}, expected {PINNED_COMMIT}")
    for args in (
        ["git", "-C", str(source), "diff", "--quiet"],
        ["git", "-C", str(source), "diff", "--cached", "--quiet"],
    ):
        try:
            subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        except (OSError, subprocess.CalledProcessError) as error:
            raise ContractError("pinned upstream checkout has local changes") from error


def run_runner(root: Path, fixture: Path) -> tuple[int, Any]:
    source = root / "parity" / "upstream" / "source"
    runner = root / "parity" / "upstream" / "agent-runner.mts"
    tsx = source / "node_modules" / ".bin" / "tsx"
    if not runner.is_file():
        raise ContractError(f"missing upstream runner: {runner}")
    if not tsx.is_file() or not os.access(tsx, os.X_OK):
        raise ContractError(f"missing pinned tsx executable: {tsx}")
    check_pin(source)
    completed = subprocess.run(
        [str(tsx), str(runner), str(fixture)],
        cwd=source,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if completed.stderr:
        print(completed.stderr, file=sys.stderr, end="")
    if completed.returncode not in (0, 1):
        raise ContractError(f"pinned upstream runner exited with status {completed.returncode}")
    try:
        result = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ContractError(f"upstream runner did not emit one JSON result: {error.msg}") from error
    if not isinstance(result, dict):
        raise ContractError("upstream runner result must be a JSON object")
    return completed.returncode, result


def main() -> int:
    temporary: tempfile.TemporaryDirectory[str] | None = None
    try:
        request = read_request()
        root = repository_root()
        input_fixture = explicit_fixture(root, request["fixture"])
        fixture, temporary, input_kind, expectations = translate_quality_case(input_fixture)
        status, result = run_runner(root, fixture)
        response = {
            "protocol": PROTOCOL,
            "adapter": ADAPTER,
            "metadata": {
                "repository": UPSTREAM_REPOSITORY,
                "commit": PINNED_COMMIT,
                "package": UPSTREAM_PACKAGE,
                "package_version": UPSTREAM_PACKAGE_VERSION,
                "runner": "parity/upstream/agent-runner.mts",
                "fixture_sha256": hashlib.sha256(input_fixture.read_bytes()).hexdigest(),
                "input_kind": input_kind,
                "translation": "quality_core_case.emit_to_declarative_parity_fixture.chunks"
                if input_kind == "quality_core_case"
                else None,
                "request_expectations": expectations,
                "tui": False,
                "ambient_discovery": False,
                "network": False,
            },
            "runner_status": status,
            "result": result,
        }
        print(json.dumps(response, separators=(",", ":")))
        return status
    except (ContractError, OSError, ValueError) as error:
        return fail(str(error))
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
