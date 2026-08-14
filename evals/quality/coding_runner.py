"""Provider-opt-in ecological coding evaluations for the three pinned Express cases.

The adapter boundary intentionally reuses the repository's two explicit live
adapters: upstream is a headless, pinned-source SDK session and Rust is the
Smol-owned `pi-agent-eval` binary.  This module owns clean worktrees,
validation, artifacts, and process resource measurements; it never discovers
a model or a credential.  Callers inject OpenRouter credentials with
``vault OPENROUTER_API_KEY -- …`` through the concrete adapter command.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
from typing import Any, Iterable

from .coding_cases import (
    CodingCaseError,
    dependency_cache_path,
    load_cases,
    materialize_clean_worktree,
    remove_worktree,
    run_validator,
)


ROOT = Path(__file__).resolve().parents[2]
PROFILE = ROOT / "parity" / "profile" / "default-profile.json"
RESULT_SCHEMA = "pi-coding-eval-result/v0"
CODING_SCHEMA = "pi-agent-quality-coding-run/v1"


class CodingRunError(RuntimeError):
    """A live coding-evaluation process or artifact violated its contract."""


def _canonical(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical(value)).hexdigest()


def _safe_environment() -> dict[str, str]:
    return {
        key: value
        for key, value in {"PATH": os.environ.get("PATH", ""), "LANG": "C", "LC_ALL": "C"}.items()
        if value
    }


def _time_command(command: list[str]) -> tuple[list[str], str | None]:
    time = Path("/usr/bin/time")
    if not time.is_file():
        return command, None
    if sys.platform == "darwin":
        return [str(time), "-l", *command], "darwin"
    return [str(time), "-v", *command], "gnu"


def _peak_rss(stderr: str, style: str | None) -> int | None:
    if style is None:
        return None
    for line in stderr.splitlines():
        text = line.strip().lower()
        if style == "darwin" and text.endswith("maximum resident set size"):
            try:
                return int(text.split()[0])
            except (IndexError, ValueError):
                return None
        if style == "gnu" and "maximum resident set size" in text:
            try:
                return int(text.rsplit(":", 1)[1].strip()) * 1024
            except (IndexError, ValueError):
                return None
    return None


def _profile_capabilities() -> list[dict[str, Any]]:
    try:
        profile = json.loads(PROFILE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CodingRunError(f"cannot read pinned default coding profile: {error}") from error
    tools = profile.get("active_tools") if isinstance(profile, dict) else None
    if not isinstance(tools, list):
        raise CodingRunError("pinned default coding profile has no active_tools")
    capabilities: list[dict[str, Any]] = []
    for tool in tools:
        if not isinstance(tool, dict) or not isinstance(tool.get("name"), str):
            raise CodingRunError("pinned default coding profile contains an invalid tool")
        capabilities.append(
            {
                "name": tool["name"],
                "kind": "pi_default_tool",
                "description": tool.get("description"),
                "schema": tool.get("parameters"),
            }
        )
    if [item["name"] for item in capabilities] != ["read", "bash", "edit", "write"]:
        raise CodingRunError("pinned default coding profile must expose read/bash/edit/write in order")
    return capabilities


def _adapter_task(case: dict[str, Any], capabilities: list[dict[str, Any]]) -> dict[str, Any]:
    task = case["task"]
    return {
        "schema_version": "pi-coding-eval-task/v0",
        "task_id": case["id"],
        "task_version": 1,
        "kind": "coding",
        "prompt": task["prompt"],
        "initial_workspace": [],
        "capabilities": capabilities,
        "timeout_seconds": 180,
        "oracle_id": "quality-express-validator-v1",
    }


def _result_contract(result: Any, *, attempt_id: str, baseline_id: str) -> dict[str, Any]:
    if not isinstance(result, dict):
        raise CodingRunError("adapter result must be a JSON object")
    if result.get("schema_version") != RESULT_SCHEMA:
        raise CodingRunError("adapter result has the wrong schema_version")
    if result.get("attempt_id") != attempt_id or result.get("baseline_id") != baseline_id:
        raise CodingRunError("adapter result identity does not match its explicit invocation")
    terminal = result.get("terminal")
    if not isinstance(terminal, dict) or terminal.get("status") not in {"completed", "failed", "cancelled", "aborted"}:
        raise CodingRunError("adapter result has no valid terminal status")
    return result


def _run_process(command: list[str], *, cwd: Path, timeout_seconds: int) -> tuple[int | None, bool, str, str, int | None]:
    measured, style = _time_command(command)
    process = subprocess.Popen(
        measured,
        cwd=cwd,
        env=_safe_environment(),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=os.name == "posix",
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
        return process.returncode, False, stdout, stderr, _peak_rss(stderr, style)
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
            stdout, stderr = process.communicate()
        return None, True, stdout, stderr, _peak_rss(stderr, style)


def _adapter_command(adapter: str, model: str, task_path: Path, workspace: Path, capabilities_path: Path, result_path: Path, attempt_id: str) -> list[str]:
    script = ROOT / "evals" / ("run-upstream-live.sh" if adapter == "upstream" else "run-rust-live.sh")
    return [
        "vault",
        "OPENROUTER_API_KEY",
        "--",
        "bash",
        str(script),
        "--model",
        model,
        "--task-json",
        str(task_path),
        "--workspace",
        str(workspace),
        "--capabilities-json",
        str(capabilities_path),
        "--result-json",
        str(result_path),
        "--attempt-id",
        attempt_id,
        "--baseline-id",
        adapter,
    ]


def prepare_cache(*, cache_root: Path, case_ids: Iterable[str] | None = None) -> dict[str, Any]:
    """Populate exact bare repositories before any offline scoring attempt."""
    selected = set(case_ids or ())
    cases = [case for case in load_cases() if not selected or case["id"] in selected]
    missing = selected - {case["id"] for case in cases}
    if missing:
        raise CodingRunError(f"unknown coding case(s): {', '.join(sorted(missing))}")
    cached: list[str] = []
    for case in cases:
        # Materialization owns the cache protocol. This private import avoids
        # duplicating its repository/commit allowlist at the CLI boundary.
        from .coding_cases import cache_bare_repository

        cache_bare_repository(case["baseline"]["repository"], case["baseline"]["commit"], cache_root, populate=True)
        cache_bare_repository(case["baseline"]["repository"], case["baseline"]["fix_commit"], cache_root, populate=True)
        cached.append(case["id"])
    return {"schema_version": CODING_SCHEMA, "operation": "prepare-cache", "cases": cached, "cache_root": str(cache_root)}


def run_coding_cases(
    *,
    model: str,
    cache_root: Path,
    workspace_root: Path,
    out: Path,
    validator: str,
    case_ids: Iterable[str] | None = None,
) -> tuple[int, dict[str, Any]]:
    if validator not in {"fast", "full"}:
        raise CodingRunError("validator must be fast or full")
    if not model:
        raise CodingRunError("a model must be explicitly supplied")
    selected = set(case_ids or ())
    cases = [case for case in load_cases() if not selected or case["id"] in selected]
    missing = selected - {case["id"] for case in cases}
    if missing:
        raise CodingRunError(f"unknown coding case(s): {', '.join(sorted(missing))}")
    capabilities = _profile_capabilities()
    destination = out.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    records: list[dict[str, Any]] = []
    for case in cases:
        case_destination = destination / case["id"]
        case_destination.mkdir(parents=True, exist_ok=True)
        task = _adapter_task(case, capabilities)
        task_path = case_destination / "adapter-task.json"
        capabilities_path = case_destination / "capabilities.json"
        task_path.write_bytes(_canonical(task) + b"\n")
        capabilities_path.write_bytes(_canonical(capabilities) + b"\n")
        adapter_records: list[dict[str, Any]] = []
        for adapter in ("upstream", "rust"):
            worktree = materialize_clean_worktree(case, cache_root, workspace_root)
            try:
                npm_cache = dependency_cache_path(worktree.path, cache_root)
                result_path = case_destination / f"{adapter}-result.json"
                attempt_id = f"quality-{case['id']}-{adapter}"
                command = _adapter_command(adapter, model, task_path, worktree.path, capabilities_path, result_path, attempt_id)
                code, timed_out, stdout, stderr, peak_rss = _run_process(command, cwd=ROOT, timeout_seconds=180)
                result: dict[str, Any] | None = None
                contract_error: str | None = None
                if not timed_out and code == 0:
                    try:
                        result = _result_contract(json.loads(result_path.read_text(encoding="utf-8")), attempt_id=attempt_id, baseline_id=adapter)
                    except (OSError, json.JSONDecodeError, CodingRunError) as error:
                        contract_error = str(error)
                validator_result = run_validator(case, worktree.path, validator, dependency_cache=npm_cache)
                diff = subprocess.run(
                    ["git", "diff", "--binary", "--no-ext-diff"], cwd=worktree.path, text=True, capture_output=True, check=False
                )
                patch = diff.stdout
                (case_destination / f"{adapter}.patch").write_text(patch, encoding="utf-8")
                adapter_record = {
                    "adapter": adapter,
                    "attempt_id": attempt_id,
                    "command": ["vault", "OPENROUTER_API_KEY", "--", "bash", str(command[4]), "…"],
                    "process": {
                        "exit_code": code,
                        "timed_out": timed_out,
                        "peak_rss_bytes": peak_rss,
                        "peak_rss_source": "process_time" if peak_rss is not None else "unavailable",
                    },
                    "adapter_result": result,
                    "adapter_contract_error": contract_error,
                    "validator": {
                        "name": validator_result.name,
                        "passed": validator_result.passed,
                        "returncode": validator_result.returncode,
                        "timed_out": validator_result.timed_out,
                        "stdout": validator_result.stdout,
                        "stderr": validator_result.stderr,
                    },
                    "patch_sha256": hashlib.sha256(patch.encode("utf-8")).hexdigest(),
                    "passed": code == 0 and not timed_out and contract_error is None and validator_result.passed,
                }
                adapter_records.append(adapter_record)
                (case_destination / f"{adapter}-record.json").write_text(
                    json.dumps(adapter_record, indent=2, sort_keys=True) + "\n", encoding="utf-8"
                )
            finally:
                remove_worktree(worktree, workspace_root)
        record = {
            "id": case["id"],
            "source": case["source"],
            "baseline": case["baseline"],
            "validator": validator,
            "adapters": adapter_records,
            "passed": all(item["passed"] for item in adapter_records),
        }
        records.append(record)
        (case_destination / "record.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    summary = {
        "schema_version": CODING_SCHEMA,
        "tier": "coding",
        "model": model,
        "validator": validator,
        "case_count": len(records),
        "passed": sum(record["passed"] for record in records),
        "failed_cases": [record["id"] for record in records if not record["passed"]],
        "cases": records,
    }
    (destination / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return (0 if not summary["failed_cases"] else 1), summary
