#!/usr/bin/env python3
"""Local protocol fixture server for real Claude Code and Codex CLI smoke runs."""

from __future__ import annotations

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qsl, urlsplit


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    fixture_dir: Path
    capture_dir: Path
    request_paths: dict[str, list[str]] = {"claude-code": [], "codex-cli": []}
    request_paths_lock = threading.Lock()
    provider_keys_by_model: dict[str, str] = {}
    provider_credentials: set[str] = {"fixture-provider-key"}
    client_credentials: set[str] = {"fixture-client-key"}
    captured_requests: list[dict[str, object]] = []

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _send(self, status: int, content_type: str, body: bytes) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json_body(self) -> dict[str, object]:
        size = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(size)
        value = json.loads(raw or b"{}")
        if not isinstance(value, dict):
            raise ValueError("request body must be a JSON object")
        return value

    def _capture(self, name: str, body: dict[str, object]) -> None:
        target_url = urlsplit(self.path)
        query = parse_qsl(target_url.query)
        headers = list(self.headers.items())
        credential_headers = [
            (key.lower(), value) for key, value in self.headers.items()
            if key.lower() in ("authorization", "x-api-key", "x-goog-api-key", "cookie")
        ]
        serialized_request = json.dumps(
            {"headers": headers, "query": query, "body": body}, sort_keys=True
        )
        expected_key = self.provider_keys_by_model.get(str(body.get("model")), "fixture-provider-key")
        expected_headers = [("x-api-key", expected_key)] if target_url.path.startswith("/v1/messages") else [("authorization", f"Bearer {expected_key}")]
        tools = body.get("tools")
        tool_types = sorted(
            {
                tool["type"]
                for tool in tools if isinstance(tool, dict) and isinstance(tool.get("type"), str)
            }
        ) if isinstance(tools, list) else []
        include = body.get("include")
        reasoning = body.get("reasoning")
        with self.request_paths_lock:
            request_sequence = list(self.request_paths[name])
        capture = {
            "path": target_url.path,
            "query_names": sorted({key for key, _value in query}),
            "request_sequence": request_sequence,
            "model": body.get("model"),
            "stream": body.get("stream"),
            "store_is_false": body.get("store") is False,
            "tools_count": len(tools) if isinstance(tools, list) else 0,
            "has_function_tool": "function" in tool_types,
            "tool_types": tool_types,
            "has_reasoning": isinstance(body.get("reasoning"), dict),
            "has_thinking": isinstance(body.get("thinking"), dict),
            "reasoning_effort_is_low": isinstance(reasoning, dict)
            and reasoning.get("effort") == "low",
            "includes_encrypted_reasoning": isinstance(include, list)
            and "reasoning.encrypted_content" in include,
            "provider_credential_matches_fixture": credential_headers == expected_headers and not any(
                secret in serialized_request for secret in self.provider_credentials if secret != expected_key
            ),
            "client_credential_absent": all(value not in serialized_request for value in self.client_credentials),
            "header_names": sorted(
                name
                for name in (
                    "anthropic-beta",
                    "anthropic-version",
                    "x-cc-original-request-id",
                    "x-cc-retry-after-too-long",
                    "x-claude-code-agent-id",
                    "x-claude-code-agent-type",
                    "x-claude-code-compaction",
                    "x-claude-code-context-compacted",
                    "x-claude-code-parent-agent-id",
                    "x-claude-code-prompt-id",
                    "x-claude-code-request-class",
                    "x-claude-code-session-id",
                    "x-stainless-retry-count",
                    "x-codex-session-id",
                )
                if self.headers.get(name) is not None
            ),
        }
        with self.request_paths_lock:
            self.captured_requests.append(capture)
        target = self.capture_dir / f"{name}.json"
        target.write_text(json.dumps(capture, indent=2, sort_keys=True) + "\n")

    def _record_path(self) -> None:
        path = urlsplit(self.path).path
        user_agent = self.headers.get("user-agent", "").lower()
        if path.startswith("/v1/messages") or "claude" in user_agent:
            client = "claude-code"
        elif path.startswith("/v1/responses") or "codex" in user_agent:
            client = "codex-cli"
        else:
            return
        with self.request_paths_lock:
            self.request_paths[client].append(path)

    def do_HEAD(self) -> None:  # noqa: N802
        self._record_path()
        self._send(404, "application/json", b'{"error":"not_found"}')

    def do_GET(self) -> None:  # noqa: N802
        self._record_path()
        if self.path.startswith("/v1/models"):
            body = json.dumps(
                {
                    "object": "list",
                    "data": [
                        {
                            "id": "claude-test",
                            "object": "model",
                            "display_name": "Claude Test",
                            "owned_by": "fixture",
                        },
                        {
                            "id": "company-default",
                            "object": "model",
                            "display_name": "Company Default",
                            "owned_by": "fixture",
                        },
                    ],
                }
            ).encode()
            self._send(200, "application/json", body)
            return
        self._send(404, "application/json", b'{"error":"not_found"}')

    def do_POST(self) -> None:  # noqa: N802
        self._record_path()
        try:
            body = self._json_body()
        except (ValueError, json.JSONDecodeError):
            self._send(400, "application/json", b'{"error":"invalid_json"}')
            return

        if self.path.startswith("/v1/messages/count_tokens"):
            self._capture("claude-code", body)
            self._send(200, "application/json", b'{"input_tokens":1}')
            return
        if self.path.startswith("/v1/messages"):
            self._capture("claude-code", body)
            payload = (self.fixture_dir / "anthropic-response.sse").read_bytes()
            self._send(200, "text/event-stream", payload)
            return
        if self.path.startswith("/v1/responses"):
            self._capture("codex-cli", body)
            payload = (self.fixture_dir / "openai-response.sse").read_bytes()
            self._send(200, "text/event-stream", payload)
            return
        self._send(404, "application/json", b'{"error":"not_found"}')


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--fixture-dir", required=True, type=Path)
    parser.add_argument("--capture-dir", required=True, type=Path)
    args = parser.parse_args()
    args.capture_dir.mkdir(parents=True, exist_ok=True)
    FixtureHandler.fixture_dir = args.fixture_dir
    FixtureHandler.capture_dir = args.capture_dir
    ThreadingHTTPServer(("127.0.0.1", args.port), FixtureHandler).serve_forever()


if __name__ == "__main__":
    main()
