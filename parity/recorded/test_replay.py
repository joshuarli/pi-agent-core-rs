from __future__ import annotations

import copy
import ast
import json
from pathlib import Path
import unittest

from . import replay


class RecordedReplayContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = replay._read_json(replay.DEFAULT_FIXTURE)

    def test_checked_in_capture_replays_to_checked_in_canonical_result(self) -> None:
        actual = replay.replay(self.fixture, source=replay.DEFAULT_FIXTURE)
        expected = replay._read_json(replay.DEFAULT_EXPECTED)
        self.assertEqual(actual, expected)
        self.assertEqual(actual["outcome"], "error")
        self.assertEqual(actual["error"]["kind"], "model")

    def test_successful_deepseek_capture_replays_thinking_updates(self) -> None:
        fixture = replay.DEFAULT_FIXTURE.with_name("deepseek-deepseek-v4-flash-0731-success.json")
        expected_path = fixture.with_name("deepseek-deepseek-v4-flash-0731-success.canonical.json")

        actual = replay.replay(replay._read_json(fixture), source=fixture)
        expected = replay._read_json(expected_path)

        self.assertEqual(actual, expected)
        self.assertEqual(actual["outcome"], "completed")
        self.assertIsNone(actual["error"])
        self.assertEqual(actual["messages"][1]["content"][0]["type"], "thinking")
        self.assertEqual(actual["messages"][1]["content"][1]["text"], "fixture capture succeeded.")

    def test_openrouter_privacy_404_has_a_non_authoritative_zero_retention_hint(self) -> None:
        fixture = replay.DEFAULT_FIXTURE.with_name("poolside-laguna-xs-2.1-privacy-restricted.json")
        recording = replay._read_json(fixture)
        actual = replay.replay(recording, source=fixture)

        self.assertEqual(actual["error"]["message"], recording["assistant"]["error_message"])
        self.assertEqual(actual["error"]["hint"], replay.OPENROUTER_PRIVACY_404_HINT)

    def test_capture_commit_and_version_must_match_upstream_pin(self) -> None:
        changed = copy.deepcopy(self.fixture)
        changed["capture"]["pi_commit"] = "0" * 40
        with self.assertRaises(replay.ContractError):
            replay.validate_recording(changed, source=replay.DEFAULT_FIXTURE)

        changed = copy.deepcopy(self.fixture)
        changed["capture"]["pi_agent_core_version"] = "999.0.0-stale"
        with self.assertRaises(replay.ContractError):
            replay.validate_recording(changed, source=replay.DEFAULT_FIXTURE)

    def test_required_redactions_and_provider_identity_are_not_inferred(self) -> None:
        changed = copy.deepcopy(self.fixture)
        changed["capture"]["redaction"]["removed"] = ["OPENROUTER_API_KEY"]
        with self.assertRaises(replay.ContractError):
            replay.validate_recording(changed, source=replay.DEFAULT_FIXTURE)

        changed = copy.deepcopy(self.fixture)
        changed["assistant"]["model"] = "another-model"
        with self.assertRaises(replay.ContractError):
            replay.validate_recording(changed, source=replay.DEFAULT_FIXTURE)

    def test_replay_has_no_external_authority(self) -> None:
        source = Path(replay.__file__).read_text(encoding="utf-8")
        tree = ast.parse(source)
        imports = {
            node.module.split(".", 1)[0]
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.module
        }
        imports.update(
            alias.name.split(".", 1)[0]
            for node in ast.walk(tree)
            if isinstance(node, ast.Import)
            for alias in node.names
        )
        self.assertNotIn("subprocess", imports)
        self.assertNotIn("urllib", imports)
        self.assertNotIn("socket", imports)

    def test_fixture_is_json_and_expected_id_is_stable(self) -> None:
        parsed = json.loads(replay.DEFAULT_FIXTURE.read_text(encoding="utf-8"))
        self.assertEqual(parsed["kind"], "recorded_pi_sdk_terminal_response")
        self.assertEqual(replay.DEFAULT_EXPECTED.stem, replay.DEFAULT_FIXTURE.stem + ".canonical")


if __name__ == "__main__":
    unittest.main()
