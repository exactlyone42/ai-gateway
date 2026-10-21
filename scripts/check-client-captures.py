#!/usr/bin/env python3
"""Validate that preserved client captures contain only the approved schema."""

from __future__ import annotations

import json
from pathlib import Path


CAPTURES = {
    Path("tests/fixtures/client-roundtrip/captures/claude-code-2.1.267.json"): {
        "path": "/v1/messages",
        "model": "claude-test",
        "query_names": ["beta"],
        "request_sequence": ["/v1/messages"],
        "has_reasoning": False,
        "has_thinking": True,
        "store_is_false": False,
        "has_function_tool": False,
        "tool_types": [],
        "reasoning_effort_is_low": False,
        "includes_encrypted_reasoning": False,
        "required_headers": {"anthropic-beta", "anthropic-version"},
    },
    Path("tests/fixtures/client-roundtrip/captures/codex-cli-0.153.0.json"): {
        "path": "/v1/responses",
        "model": "company-default",
        "query_names": [],
        "request_sequence": ["/v1/responses"],
        "has_reasoning": True,
        "has_thinking": False,
        "store_is_false": True,
        "has_function_tool": True,
        "tool_types": ["function", "namespace", "web_search"],
        "reasoning_effort_is_low": True,
        "includes_encrypted_reasoning": True,
        "required_headers": set(),
    },
}
ALLOWED_KEYS = {
    "client_credential_absent",
    "has_reasoning",
    "has_thinking",
    "has_function_tool",
    "header_names",
    "includes_encrypted_reasoning",
    "model",
    "path",
    "provider_credential_matches_fixture",
    "query_names",
    "reasoning_effort_is_low",
    "request_sequence",
    "store_is_false",
    "stream",
    "tool_types",
    "tools_count",
}


def main() -> None:
    failed = False
    for path, expected in CAPTURES.items():
        value = json.loads(path.read_text())
        extra = set(value) - ALLOWED_KEYS
        missing = ALLOWED_KEYS - set(value)
        errors = []
        if extra:
            errors.append(f"unapproved fields: {sorted(extra)}")
        if missing:
            errors.append(f"missing fields: {sorted(missing)}")
        for key in (
            "path",
            "model",
            "query_names",
            "request_sequence",
            "has_reasoning",
            "has_thinking",
            "store_is_false",
            "has_function_tool",
            "tool_types",
            "reasoning_effort_is_low",
            "includes_encrypted_reasoning",
        ):
            if value.get(key) != expected[key]:
                errors.append(f"unexpected {key}: {value.get(key)!r}")
        if value.get("stream") is not True:
            errors.append("stream was not true")
        if not isinstance(value.get("tools_count"), int) or value["tools_count"] <= 0:
            errors.append("tools_count must be a positive integer")
        if value.get("provider_credential_matches_fixture") is not True:
            errors.append("provider credential was not the fixture provider key")
        if value.get("client_credential_absent") is not True:
            errors.append("client credential reached the provider")
        header_names = value.get("header_names")
        if not isinstance(header_names, list) or not expected["required_headers"].issubset(
            set(header_names)
        ):
            errors.append("required protocol header names are missing")
        if errors:
            failed = True
            print(f"{path}: {'; '.join(errors)}")
    if failed:
        raise SystemExit(1)
    print("client capture schema ok")


if __name__ == "__main__":
    main()
