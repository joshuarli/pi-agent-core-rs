"""Contract checks for the deterministic core quality-case manifests.

This test intentionally validates only the declarative case inventory and its
provenance. It does not execute an adapter or claim an upstream result.
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path


ROOT = Path(__file__).parent
REPOSITORY = "https://github.com/earendil-works/pi.git"
UPSTREAM_COMMIT = "9d2ec7ffabe927bfad2214c1cee25b6632a78dcf"
ENABLED_CASES = {
    "malformed-empty-tool-name",
    "unknown-tool",
    "invalid-tool-args-recovery",
    "repeated-invalid-call",
    "stream-error-settlement",
    "tool-use-zero-calls",
    "partial-tool-call-stream-error",
    "parallel-tool-ordering",
    "abort-during-tool",
    "abort-during-parallel-tools",
}
EXCLUDED_CASES = {"orphan-tool-result"}


class CoreCaseManifestTest(unittest.TestCase):
    def _manifests(self) -> dict[str, dict]:
        paths = sorted(ROOT.glob("*/manifest.json"))
        manifests = {}
        for path in paths:
            value = json.loads(path.read_text())
            self.assertEqual(value["kind"], "quality_core_case", path)
            self.assertEqual(value["id"], path.parent.name, path)
            manifests[path.parent.name] = value
        return manifests

    def test_required_inventory_and_excluded_optional_probe(self) -> None:
        manifests = self._manifests()
        self.assertEqual(set(manifests), ENABLED_CASES | EXCLUDED_CASES)
        self.assertEqual(
            {case_id for case_id, value in manifests.items() if value.get("status") != "excluded"},
            ENABLED_CASES,
        )
        self.assertEqual(manifests["orphan-tool-result"]["status"], "excluded")

    def test_provenance_and_adapter_mapping_are_explicit(self) -> None:
        for case_id, manifest in self._manifests().items():
            source = manifest["source"]
            self.assertEqual(source["repository"], REPOSITORY, case_id)
            self.assertEqual(source["upstream_commit_tested"], UPSTREAM_COMMIT, case_id)
            self.assertEqual(source["role"], "scenario_inspiration", case_id)
            self.assertIs(source["historical_behavior_is_oracle"], False, case_id)
            self.assertIn("adapter_fixture", manifest, case_id)

            mapping = manifest["adapter_fixture"]
            if mapping.startswith("parity/"):
                self.assertTrue((ROOT.parents[3] / mapping).is_file(), mapping)
            elif manifest.get("status") == "excluded":
                self.assertEqual(mapping, "excluded", case_id)
            else:
                self.assertEqual(mapping, "generated", case_id)

    def test_enabled_cases_carry_exact_script_and_observation_contract(self) -> None:
        for case_id in ENABLED_CASES:
            manifest = self._manifests()[case_id]
            self.assertEqual(manifest["scope"], "core", case_id)
            self.assertEqual(manifest["parity"], "strict", case_id)
            self.assertEqual(manifest["oracle"]["kind"], "current_pinned_upstream_capture", case_id)
            self.assertEqual(manifest["oracle"]["historical_issue_expectation"], "not_an_oracle", case_id)
            self.assertTrue(manifest["execution"]["deterministic"], case_id)
            self.assertTrue(manifest["model_script"], case_id)
            self.assertIn("setup", manifest, case_id)
            self.assertIn("host", manifest, case_id)
            observations = manifest["observations"]
            self.assertTrue(observations["measure"], case_id)
            self.assertTrue(observations["metrics"], case_id)


if __name__ == "__main__":
    unittest.main()
