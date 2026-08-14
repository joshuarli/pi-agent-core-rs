#!/usr/bin/env python3
"""Smoke-test both quality adapter process boundaries without a provider."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[3]
FIXTURE = "parity/fixtures/declarative/single-turn-text.json"
QUALITY_FIXTURE = "evals/quality/cases/core/unknown-tool/manifest.json"


class AdapterProtocolTest(unittest.TestCase):
    def run_adapter(self, name: str, fixture: str) -> tuple[int, dict[str, object], str]:
        adapter = ROOT / "evals" / "quality" / "adapters" / name / "adapter.py"
        request = {"protocol": "pi-agent-quality-adapter/v0", "operation": "run", "fixture": fixture}
        environment = dict(os.environ)
        environment["PYTHONDONTWRITEBYTECODE"] = "1"
        completed = subprocess.run(
            [str(adapter)],
            cwd=ROOT,
            input=json.dumps(request),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            check=False,
        )
        self.assertIn(completed.returncode, (0, 1), completed.stderr)
        documents = [line for line in completed.stdout.splitlines() if line.strip()]
        self.assertEqual(len(documents), 1, completed.stdout)
        response = json.loads(documents[0])
        self.assertIsInstance(response, dict)
        return completed.returncode, response, completed.stderr

    def test_upstream_direct_declarative_fixture(self) -> None:
        status, response, _ = self.run_adapter("upstream-core", FIXTURE)
        self.assertEqual(status, 0)
        self.assertEqual(response["protocol"], "pi-agent-quality-adapter/v0")
        self.assertEqual(response["adapter"], "upstream-core")
        self.assertEqual(response["metadata"]["commit"], "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf")
        self.assertEqual(response["result"]["fixture_id"], "single-turn-text")

    def test_rust_direct_declarative_fixture(self) -> None:
        status, response, _ = self.run_adapter("rust-core", FIXTURE)
        self.assertEqual(status, 0)
        self.assertEqual(response["protocol"], "pi-agent-quality-adapter/v0")
        self.assertEqual(response["adapter"], "rust-core")
        self.assertEqual(response["metadata"]["toolchain"], "nightly-2026-07-24")
        self.assertEqual(response["result"]["fixture_id"], "single-turn-text")

    def test_quality_emit_translation_reaches_both_runners(self) -> None:
        for name in ("upstream-core", "rust-core"):
            status, response, _ = self.run_adapter(name, QUALITY_FIXTURE)
            self.assertEqual(status, 0, name)
            self.assertEqual(response["metadata"]["input_kind"], "quality_core_case")
            self.assertEqual(response["metadata"]["translation"], "quality_core_case.emit_to_declarative_parity_fixture.chunks")
            self.assertEqual(response["result"]["fixture_id"], "unknown-tool")


if __name__ == "__main__":
    unittest.main()
