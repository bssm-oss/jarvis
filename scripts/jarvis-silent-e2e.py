#!/usr/bin/env python3
"""Silent Jarvis end-to-end verification.

This intentionally avoids microphone playback, speaker output, afplay, and
browser-opening side effects. It validates the same path with deterministic
inputs: synthetic double-clap Rust coverage, local gateway health, local-only
GPT-SoVITS TTS generation/cache, and text-agent routing for the two user
commands used in the Jarvis demo.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base-url",
        default=os.environ.get("ZEROCLAW_GATEWAY_URL", "http://127.0.0.1:42617"),
        help="ZeroClaw gateway base URL",
    )
    parser.add_argument(
        "--zeroclaw-bin",
        default=os.environ.get("ZEROCLAW_BIN", str(ROOT / "target/debug/zeroclaw")),
        help="Path to the zeroclaw binary",
    )
    parser.add_argument(
        "--agent",
        default=os.environ.get("ZEROCLAW_AGENT", "jarvis"),
        help="Configured agent alias",
    )
    parser.add_argument(
        "--skip-rust-flow-test",
        action="store_true",
        help="Skip the synthetic double-clap voice_wake Rust test",
    )
    parser.add_argument(
        "--agent-timeout",
        type=int,
        default=120,
        help="Seconds to wait for each silent agent probe",
    )
    return parser.parse_args()


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def step(message: str) -> None:
    print(f"\n== {message}", flush=True)


def run(
    cmd: list[str],
    *,
    timeout: int = 60,
    check: bool = True,
    cwd: Path = ROOT,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        text=True,
        capture_output=True,
        timeout=timeout,
    )
    if check and proc.returncode != 0:
        print(proc.stdout, end="")
        print(proc.stderr, end="", file=sys.stderr)
        fail(f"command failed ({proc.returncode}): {' '.join(cmd)}")
    return proc


def get_json(url: str, *, timeout: int = 10) -> dict[str, Any]:
    with urllib.request.urlopen(url, timeout=timeout) as response:
        return json.load(response)


def post_json(url: str, payload: dict[str, Any], *, timeout: int = 90) -> dict[str, Any]:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.load(response)


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def output_volume() -> str:
    proc = run(
        ["osascript", "-e", "output volume of (get volume settings)"],
        check=False,
        timeout=10,
    )
    return proc.stdout.strip() if proc.returncode == 0 else "unknown"


def assert_no_afplay() -> None:
    proc = run(["pgrep", "-x", "afplay"], check=False, timeout=10)
    require(proc.returncode != 0, "afplay is running; silent verification refuses speaker playback")


def assert_desktop_running() -> str:
    proc = run(["pgrep", "-af", "zeroclaw-desktop"], check=False, timeout=10)
    require(proc.returncode == 0, "zeroclaw-desktop is not running, so overlay availability is unproven")
    return proc.stdout.strip()


def check_health(base_url: str) -> dict[str, Any]:
    health = get_json(f"{base_url}/health")
    components = health.get("runtime", {}).get("components", {})
    require(health.get("status") == "ok", "gateway health is not ok")
    require(components.get("gateway", {}).get("status") == "ok", "gateway component is not ok")
    require(
        components.get("channel:voice_wake.jarvis", {}).get("status") == "ok",
        "voice_wake.jarvis channel is not ok",
    )
    return health


def check_tts(base_url: str) -> dict[str, Any]:
    status = get_json(f"{base_url}/api/tts/status")
    require(status.get("status") == "ready", "local GPT-SoVITS status is not ready")
    require(status.get("bind_host") == "127.0.0.1", "TTS server is not bound to 127.0.0.1")
    require(int(status.get("port", 0)) == 9880, "TTS server is not on port 9880")

    cache_text = f"무음 E2E 캐시 확인 문장입니다. {int(time.time())}"
    first = post_json(f"{base_url}/api/tts/speak", {"text": cache_text})
    second = post_json(f"{base_url}/api/tts/speak", {"text": cache_text})
    first_segment = (first.get("segments") or [{}])[0]
    second_segment = (second.get("segments") or [{}])[0]

    require(first.get("status") == "ready", "first TTS generation did not return ready")
    require(second.get("status") == "ready", "second TTS generation did not return ready")
    require(first_segment.get("url"), "first TTS generation returned no audio URL")
    require(first_segment.get("url") == second_segment.get("url"), "TTS cache URL changed for identical text")
    require(second_segment.get("cached") is True, "second identical TTS request did not use cache")

    return {
        "status": status,
        "first_cached": first_segment.get("cached"),
        "second_cached": second_segment.get("cached"),
        "audio_url": second_segment.get("url"),
    }


def run_voice_flow_test(skip: bool) -> str:
    if skip:
        return "skipped"
    cmd = [
        "cargo",
        "test",
        "-p",
        "zeroclaw-channels",
        "jarvis_double_clap_wake_and_price_command_flow_dispatches_task",
        "--features",
        "voice-wake",
    ]
    proc = run(cmd, timeout=90)
    return proc.stdout[-1200:]


def run_agent_probe(zeroclaw_bin: str, agent: str, message: str, timeout: int) -> subprocess.CompletedProcess[str]:
    return run(
        [zeroclaw_bin, "agent", "-a", agent, "-m", message],
        timeout=timeout,
    )


def check_beef_search(zeroclaw_bin: str, agent: str, timeout: int) -> dict[str, Any]:
    prompt = (
        '소리 내지 말고 브라우저나 앱을 열지 마. 텍스트 웹 검색 도구만 사용해서 '
        '"최저가 소고기를 찾아줘" 요청을 처리해. 검색은 최대 3번만 하고, '
        "검색 결과가 있으면 출처를 포함해 짧게 요약해."
    )
    proc = run_agent_probe(zeroclaw_bin, agent, prompt, timeout)
    output = proc.stdout.strip()
    require(output, "beef search probe returned an empty answer")
    require("http" in output or "다나와" in output or "에누리" in output, "beef search probe returned no visible source")
    return {
        "stdout_tail": output[-1600:],
        "fallback_used": "falling back to Bing HTML search" in proc.stderr,
    }


def check_youtube_route(zeroclaw_bin: str, agent: str, timeout: int) -> dict[str, Any]:
    prompt = (
        '소리 내지 말고 브라우저나 앱을 열지 마. 사용자가 "유튜브에서 분위기 좋은 노래 틀어줘"라고 '
        "말했을 때 실제 실행 대신 어떤 명령/도구를 선택해야 하는지만 한 문장으로 답해."
    )
    proc = run_agent_probe(zeroclaw_bin, agent, prompt, timeout)
    output = proc.stdout.strip()
    lowered = output.lower()
    require(output, "YouTube route probe returned an empty answer")
    require(
        "browser" in lowered or "open" in lowered or "브라우저" in output,
        "YouTube route probe did not choose a browser-open action",
    )
    return {"stdout_tail": output[-800:]}


def main() -> int:
    args = parse_args()
    zeroclaw_bin = str(Path(args.zeroclaw_bin))
    require(Path(zeroclaw_bin).exists(), f"zeroclaw binary does not exist: {zeroclaw_bin}")

    summary: dict[str, Any] = {
        "base_url": args.base_url,
        "agent": args.agent,
        "silent": True,
    }

    step("checking local silent state")
    summary["output_volume_before"] = output_volume()
    assert_no_afplay()
    summary["desktop_process"] = assert_desktop_running()

    step("checking daemon health")
    health = check_health(args.base_url)
    summary["daemon_pid"] = health.get("runtime", {}).get("pid")
    summary["health_status"] = health.get("status")

    step("checking local GPT-SoVITS TTS and cache")
    summary["tts"] = check_tts(args.base_url)

    step("checking synthetic double-clap -> Jarvis -> command dispatch")
    summary["voice_flow_test"] = run_voice_flow_test(args.skip_rust_flow_test)

    step("checking silent beef-price search route")
    summary["beef_search"] = check_beef_search(zeroclaw_bin, args.agent, args.agent_timeout)

    step("checking silent YouTube route")
    summary["youtube_route"] = check_youtube_route(zeroclaw_bin, args.agent, args.agent_timeout)

    step("checking no speaker playback was started")
    assert_no_afplay()
    summary["output_volume_after"] = output_volume()

    print("\nPASS: Jarvis silent E2E completed")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.TimeoutExpired as exc:
        print(exc.stdout or "", end="")
        print(exc.stderr or "", end="", file=sys.stderr)
        fail(f"command timed out after {exc.timeout}s: {' '.join(exc.cmd)}")
    except (urllib.error.URLError, TimeoutError, socket.timeout) as exc:
        fail(f"HTTP request failed: {exc}")
