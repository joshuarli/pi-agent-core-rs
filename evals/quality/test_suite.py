"""Focused provider-free tests for quality-suite lowering and artifacts."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from .suite import compile_core_fixture, load_core_cases, run_fast


class QualitySuiteTest(unittest.TestCase):
    def test_lowered_fixtures_have_shared_v0_contract(self) -> None:
        for _path, manifest in load_core_cases():
            fixture = compile_core_fixture(manifest)
            self.assertEqual(fixture["format_version"], 1)
            self.assertEqual(fixture["kind"], "declarative_parity_fixture")
            self.assertTrue(fixture["model_script"])
            for turn in fixture["model_script"]:
                self.assertIn(turn["chunks"][-1]["kind"], {"done", "error"})

    def test_known_fixture_runs_through_the_rust_runner(self) -> None:
        with tempfile.TemporaryDirectory(prefix="tea-quality-test-") as temporary:
            status, summary = run_fast(case_ids=["unknown-tool"], out=Path(temporary))
            self.assertEqual(status, 0, summary)
            self.assertEqual(summary["matches"], 1)
            artifact = Path(temporary) / "unknown-tool"
            self.assertTrue((artifact / "report.json").is_file())
            self.assertTrue((artifact / "rust-trace.json").is_file())


if __name__ == "__main__":
    unittest.main()
