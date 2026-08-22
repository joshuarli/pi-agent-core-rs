#!/usr/bin/env python3
"""Validate the small, deterministic normalized fx UI fixture corpus."""

import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent
MANIFEST_PATH = ROOT / "manifest.json"


def fail(path: Path, message: str) -> None:
    raise SystemExit(f"{path}: {message}")


def require(value, path: Path, name: str):
    if name not in value:
        fail(path, f"missing {name}")
    return value[name]


def validate_style(style, path: Path, where: str) -> None:
    if not isinstance(style, dict):
        fail(path, f"{where}.style must be an object")
    if set(style) != {"foreground", "background", "attributes"}:
        fail(path, f"{where}.style has unknown or missing fields")
    if style["foreground"] is not None and not isinstance(style["foreground"], str):
        fail(path, f"{where}.foreground must be a string or null")
    if style["background"] is not None and not isinstance(style["background"], str):
        fail(path, f"{where}.background must be a string or null")
    if not isinstance(style["attributes"], list) or not all(
        isinstance(attribute, str) for attribute in style["attributes"]
    ):
        fail(path, f"{where}.attributes must be a list of strings")


def validate_fixture(path: Path, manifest_case: dict) -> None:
    try:
        fixture = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(path, f"cannot read JSON: {error}")
    if set(fixture) != {
        "format_version",
        "kind",
        "id",
        "description",
        "size",
        "cursor",
        "cell_defaults",
        "runs",
        "evidence",
    }:
        fail(path, "top-level fields do not match format_version 1")
    if fixture["format_version"] != 1 or fixture["kind"] != "normalized_grid_fixture":
        fail(path, "unsupported fixture kind or version")
    if fixture["id"] != manifest_case["id"]:
        fail(path, "fixture id differs from manifest")
    size = require(fixture, path, "size")
    if set(size) != {"columns", "rows"} or not all(
        isinstance(size[key], int) and size[key] >= 0 for key in size
    ):
        fail(path, "size must contain non-negative integer columns and rows")
    columns, rows = size["columns"], size["rows"]
    cursor = require(fixture, path, "cursor")
    if cursor is not None:
        if set(cursor) != {"row", "column", "visible"}:
            fail(path, "cursor fields are invalid")
        if not isinstance(cursor["row"], int) or not isinstance(cursor["column"], int):
            fail(path, "cursor coordinates must be integers")
        if not (0 <= cursor["row"] < rows and 0 <= cursor["column"] < columns):
            fail(path, "cursor is outside the declared grid")
        if not isinstance(cursor["visible"], bool):
            fail(path, "cursor.visible must be boolean")
    validate_style(fixture["cell_defaults"], path, "cell_defaults")
    runs = require(fixture, path, "runs")
    if not isinstance(runs, list):
        fail(path, "runs must be an array")
    occupied = set()
    for index, run in enumerate(runs):
        where = f"runs[{index}]"
        if set(run) - {"row", "column", "text", "repeat", "style"} or not {
            "row",
            "column",
            "text",
        } <= set(run):
            fail(path, f"{where} fields are invalid")
        row, column, text = run["row"], run["column"], run["text"]
        if not isinstance(row, int) or not isinstance(column, int) or not isinstance(text, str):
            fail(path, f"{where} has invalid types")
        repeat = run.get("repeat", 1)
        if not isinstance(repeat, int) or repeat < 1:
            fail(path, f"{where}.repeat must be a positive integer")
        if repeat > 1 and len(text) != 1:
            fail(path, f"{where}.repeat requires one character in text")
        if "\n" in text or "\r" in text or not text:
            fail(path, f"{where}.text must be non-empty and single-line")
        if "style" in run:
            validate_style(run["style"], path, where)
        expanded = text * repeat
        for offset, character in enumerate(expanded):
            coordinate = (row, column + offset)
            if not (0 <= row < rows and 0 <= coordinate[1] < columns):
                fail(path, f"{where} extends outside the declared grid")
            if coordinate in occupied:
                fail(path, f"{where} overlaps cell {coordinate}")
            occupied.add(coordinate)
            if len(character) != 1:
                fail(path, f"{where} contains an invalid scalar")
    evidence = require(fixture, path, "evidence")
    if evidence not in (
        {"text_grid": "captured", "cursor_position": "captured", "style": "not_captured"},
        {"text_grid": "captured", "cursor_position": "captured", "style": "captured"},
    ):
        fail(path, "evidence must state exactly what the source artifact proves")


def main() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text())
    if manifest["format_version"] != 1 or manifest["kind"] != "fx_ui_oracle_manifest":
        fail(MANIFEST_PATH, "unsupported manifest kind or version")
    cases = manifest.get("cases")
    if not isinstance(cases, list) or not cases:
        fail(MANIFEST_PATH, "manifest must contain captured cases")
    for case in cases:
        fixture_path = ROOT / case["fixture"]
        if not fixture_path.is_file():
            fail(MANIFEST_PATH, f"missing fixture {case['fixture']}")
        validate_fixture(fixture_path, case)
    tea_cases = manifest.get("tea_cases", [])
    if not isinstance(tea_cases, list):
        fail(MANIFEST_PATH, "tea_cases must be an array when present")
    for case in tea_cases:
        fixture_path = ROOT / case["fixture"]
        if not fixture_path.is_file():
            fail(MANIFEST_PATH, f"missing tea fixture {case['fixture']}")
        validate_fixture(fixture_path, case)
    print(f"fx-ui fixtures: {len(cases)} fx and {len(tea_cases)} tea cases valid")


if __name__ == "__main__":
    main()
