#!/usr/bin/env python3
"""Small local Whisper HTTP bridge for ZeroClaw's local_whisper provider."""

from __future__ import annotations

import argparse
from email import policy
from email.parser import BytesParser
import json
import os
import subprocess
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class WhisperHandler(BaseHTTPRequestHandler):
    server_version = "ZeroClawLocalWhisper/1.0"

    def do_GET(self) -> None:
        if self.path == "/health":
            self.send_json(200, {"ok": True})
            return
        self.send_json(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path not in {"/v1/transcribe", "/v1/audio/transcriptions"}:
            self.send_json(404, {"error": "not found"})
            return

        if self.server.bearer_token:
            expected = f"Bearer {self.server.bearer_token}"
            if self.headers.get("Authorization") != expected:
                self.send_json(401, {"error": "unauthorized"})
                return

        try:
            field = self.read_file_field()
            text = self.transcribe(field["name"], field["data"])
            self.send_json(200, {"text": text})
        except Exception as exc:  # noqa: BLE001 - convert every local failure to JSON.
            self.send_json(500, {"error": str(exc)})

    def read_file_field(self) -> dict[str, bytes | str]:
        content_type = self.headers.get("Content-Type", "")
        if "multipart/form-data" not in content_type:
            raise ValueError("multipart/form-data body is required")

        try:
            content_length = int(self.headers.get("Content-Length", "0"))
        except ValueError as exc:
            raise ValueError("invalid Content-Length header") from exc
        if content_length <= 0:
            raise ValueError("request body is empty")

        body = self.rfile.read(content_length)
        header_block = (
            f"Content-Type: {content_type}\r\n"
            f"Content-Length: {content_length}\r\n"
            "MIME-Version: 1.0\r\n"
            "\r\n"
        ).encode("utf-8")
        message = BytesParser(policy=policy.default).parsebytes(header_block + body)
        if not message.is_multipart():
            raise ValueError("multipart/form-data body is required")

        for part in message.iter_parts():
            if part.get_param("name", header="content-disposition") != "file":
                continue

            filename = os.path.basename(part.get_filename() or "audio.wav")
            data = part.get_payload(decode=True) or b""
            if not data:
                raise ValueError("uploaded audio is empty")
            return {"name": filename, "data": data}

        raise ValueError("multipart field `file` is required")

    def transcribe(self, filename: str, data: bytes) -> str:
        stem, ext = os.path.splitext(filename)
        if not ext:
            ext = ".wav"
        safe_stem = "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in stem) or "audio"

        with tempfile.TemporaryDirectory(prefix="zeroclaw-whisper-") as tmpdir:
            audio_path = os.path.join(tmpdir, f"{safe_stem}{ext}")
            with open(audio_path, "wb") as audio_file:
                audio_file.write(data)

            cmd = [
                self.server.whisper_bin,
                audio_path,
                "--model",
                self.server.model,
                "--output_dir",
                tmpdir,
                "--output_format",
                "json",
                "--verbose",
                "False",
                "--fp16",
                "False",
            ]
            if self.server.language:
                cmd.extend(["--language", self.server.language])
            if self.server.initial_prompt:
                cmd.extend(["--initial_prompt", self.server.initial_prompt])
            if self.server.threads:
                cmd.extend(["--threads", str(self.server.threads)])

            completed = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=self.server.timeout,
                check=False,
            )
            if completed.returncode != 0:
                detail = completed.stderr.strip() or completed.stdout.strip()
                raise RuntimeError(f"whisper failed with exit {completed.returncode}: {detail}")

            json_path = os.path.join(tmpdir, f"{safe_stem}.json")
            with open(json_path, "r", encoding="utf-8") as result_file:
                result = json.load(result_file)
            return str(result.get("text", "")).strip()

    def send_json(self, status: int, payload: dict[str, object]) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        if self.server.verbose:
            super().log_message(fmt, *args)


class WhisperServer(ThreadingHTTPServer):
    def __init__(self, server_address: tuple[str, int], args: argparse.Namespace) -> None:
        super().__init__(server_address, WhisperHandler)
        self.whisper_bin = args.whisper_bin
        self.model = args.model
        self.language = args.language
        self.initial_prompt = args.initial_prompt
        self.threads = args.threads
        self.timeout = args.timeout
        self.bearer_token = args.bearer_token
        self.verbose = args.verbose


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--model", default="base")
    parser.add_argument("--language", default="ko")
    parser.add_argument("--initial-prompt", default="자비스 Jarvis")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--bearer-token", default="")
    parser.add_argument("--whisper-bin", default="whisper")
    parser.add_argument("--verbose", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    server = WhisperServer((args.host, args.port), args)
    print(f"local whisper server listening on http://{args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
