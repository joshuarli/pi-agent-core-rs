"""Provider-free contract checks for the small ecological coding suite."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from .coding_cases import CodingCaseError, cache_bare_repository, load_cases
from .coding_runner import _adapter_task, _profile_capabilities


class CodingCasesTest(unittest.TestCase):
    def test_exact_three_requested_cases_and_pins(self) -> None:
        cases = load_cases()
        self.assertEqual(
            [case["id"] for case in cases],
            ["express-3936-medium", "express-4205-hard", "express-4744-easy"],
        )
        for case in cases:
            self.assertEqual(case["setup"]["network"], False)
            self.assertEqual(case["setup"]["tools"], ["read", "bash", "edit", "write"])
            self.assertEqual(case["validators"]["full"]["audit_command"], "npm install && npm test")
            self.assertEqual(case["validators"]["fast"]["evidence"]["baseline"], "fails")
            self.assertEqual(case["validators"]["fast"]["evidence"]["known_correct"], "passes")

    def test_scoring_requires_a_prepopulated_bare_cache(self) -> None:
        case = load_cases()[0]
        with tempfile.TemporaryDirectory(prefix="pi-agent-quality-cache-") as temporary:
            with self.assertRaises(CodingCaseError):
                cache_bare_repository(case["baseline"]["repository"], case["baseline"]["commit"], Path(temporary))

    def test_adapter_task_uses_the_pinned_default_profile_tool_contract(self) -> None:
        capabilities = _profile_capabilities()
        task = _adapter_task(load_cases()[0], capabilities)
        self.assertEqual(task["capabilities"], capabilities)
        self.assertEqual([tool["name"] for tool in capabilities], ["read", "bash", "edit", "write"])


if __name__ == "__main__":
    unittest.main()
