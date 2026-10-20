#!/usr/bin/env python3
"""Use the personal loopback Gateway without exposing its client credential."""
import argparse
import json
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    parser = argparse.ArgumentParser(description="透過本機 Gateway 問 AI")
    parser.add_argument("prompt", nargs="?", help="問題；省略則進入對話")
    parser.add_argument("--auto", action="store_true", help="讓 Jev 選模型；問題會送到 TypeSafe")
    parser.add_argument("--model", choices=["local-general", "local-code"], default="local-general")
    args = parser.parse_args()
    root = Path.home() / ".config/agent-gateway-jev"
    try:
        config = json.loads((root / "client.json").read_text())
        if args.auto and "JEV_ENABLED=true" not in (root / "server.env").read_text().splitlines():
            print("Jev 尚未啟用：請先設定 TypeSafe 金鑰並重新啟動服務。", file=sys.stderr)
            return 2
        messages = []
        while True:
            prompt = args.prompt if args.prompt is not None else input("你（/exit 離開、/clear 清除對話）> ")
            if prompt == "/exit":
                return 0
            if prompt == "/clear":
                messages.clear()
                continue
            if not prompt.strip():
                continue
            gate = subprocess.run(["/usr/local/libexec/ollama-memory-gate"], capture_output=True, timeout=10)
            if gate.returncode:
                print("本機記憶體檢查未通過，未送出問題。", file=sys.stderr)
                return 2
            messages.append({"role": "user", "content": prompt})
            body = {"model": "quality:premium" if args.auto else args.model,
                    "messages": messages, "stream": False, "max_tokens": 512}
            request = urllib.request.Request("http://127.0.0.1:18080/v1/chat/completions",
                data=json.dumps(body).encode(), headers={"Authorization": "Bearer " + config["api_key"], "Content-Type": "application/json"})
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
            with opener.open(request, timeout=180) as response:
                value = json.load(response)
            message = value["choices"][0]["message"]
            print(message.get("content") or "模型未回傳文字；請查看用量紀錄。")
            print("使用模型：" + str(value.get("model", "未回報")), file=sys.stderr)
            messages.append(message)
            if args.prompt is not None:
                return 0
    except urllib.error.HTTPError as exc:
        print(f"Gateway 回傳 HTTP {exc.code}；未重送。", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, EOFError, subprocess.TimeoutExpired):
        print("無法完成：請確認本機服務、設定檔與回應格式；未重送。", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
