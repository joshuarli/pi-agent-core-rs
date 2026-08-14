#!/usr/bin/env python3
"""JSON adapter for the Rust pi-agent-parity executable.

The adapter owns process setup only.  The Rust executable remains the
implementation of fixture parsing, execution, and canonical normalization.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any


PROTOCOL = "pi-agent-quality-adapter/v0"
ADAPTER = "rust-core"
TOOLCHAIN = "nightly-2026-07-24"


class ContractError(Exception):
    """An invalid adapter request or unavailable pinned runner."""


def fail(message: str) -> int:
    print(f"{ADAPTER} adapter: {message}", file=sys.stderr)
    return 2


def repository_root() -> Path:
    # adapter.py lives at evals/quality/adapters/rust-core/.
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


def translate_quality_case(
    root: Path, path: Path
) -> tuple[Path, tempfile.TemporaryDirectory[str] | None, str, list[dict[str, Any]]]:
    """Use the shared adapter-side ``emit`` to ``chunks`` translation.

    This imports only the pure translation helper from the sibling adapter;
    it does not run or otherwise invoke the upstream implementation. Keeping
    one translation definition avoids the two quality adapters accepting
    different fixture dialects while the existing Rust and upstream runners
    retain their intentionally closed ``chunks`` contract.
    """

    helper_path = root / "evals" / "quality" / "adapters" / "upstream-core" / "adapter.py"
    spec = importlib.util.spec_from_file_location("pi_quality_upstream_adapter", helper_path)
    if spec is None or spec.loader is None:
        raise ContractError(f"could not load quality fixture translator: {helper_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    try:
        return module.translate_quality_case(path)
    except Exception as error:
        # The helper is loaded as a module, so its ContractError class is not
        # identical to this adapter's class. Re-wrap it at this protocol
        # boundary to keep invalid quality input a status-2 contract error.
        raise ContractError(str(error)) from error


def check_toolchain(root: Path) -> None:
    toolchain = root / "rust-toolchain.toml"
    if not toolchain.is_file():
        raise ContractError(f"missing Rust toolchain pin: {toolchain}")
    marker = 'channel = "' + TOOLCHAIN + '"'
    if marker not in toolchain.read_text(encoding="utf-8"):
        raise ContractError(f"rust-toolchain.toml does not pin {TOOLCHAIN}")


def run_runner(root: Path, fixture: Path) -> tuple[int, Any]:
    runner = root / "crates" / "pi-agent-core" / "src" / "bin" / "pi-agent-parity.rs"
    if not runner.is_file():
        raise ContractError(f"missing Rust parity runner source: {runner}")
    check_toolchain(root)
    completed = subprocess.run(
        [
            "cargo",
            f"+{TOOLCHAIN}",
            "run",
            "--quiet",
            "-p",
            "pi-agent-core",
            "--features",
            "parity-runner",
            "--bin",
            "pi-agent-parity",
            "--",
            str(fixture),
        ],
        cwd=root,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if completed.stderr:
        print(completed.stderr, file=sys.stderr, end="")
    if completed.returncode not in (0, 1):
        raise ContractError(f"Rust parity runner exited with status {completed.returncode}")
    try:
        result = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ContractError(f"Rust parity runner did not emit one JSON result: {error.msg}") from error
    if not isinstance(result, dict):
        raise ContractError("Rust parity runner result must be a JSON object")
    return completed.returncode, result


def main() -> int:
    temporary: tempfile.TemporaryDirectory[str] | None = None
    try:
        request = read_request()
        root = repository_root()
        input_fixture = explicit_fixture(root, request["fixture"])
        fixture, temporary, input_kind, expectations = translate_quality_case(root, input_fixture)
        status, result = run_runner(root, fixture)
        response = {
            "protocol": PROTOCOL,
            "adapter": ADAPTER,
            "metadata": {
                "crate": "pi-agent-core",
                "runner": "crates/pi-agent-core/src/bin/pi-agent-parity.rs",
                "toolchain": TOOLCHAIN,
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
