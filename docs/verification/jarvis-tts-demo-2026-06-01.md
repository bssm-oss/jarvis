# Jarvis Local GPT-SoVITS TTS Verification - 2026-06-01

## Scope

Flow under test:

`/voice-activation` loads -> wake confirmation event displays the Jarvis acknowledgement -> local GPT-SoVITS speaks through the web audio path -> repeated text reuses the cached WAV -> the demo video shows a short real conversation flow after wake.

## Artifacts

- Demo video with Yuni TTS audio track: `output/playwright/jarvis-tts-demo.mp4`
- Conversation demo video with user/Jarvis turns: `output/playwright/jarvis-tts-conversation-demo.mp4`
- Raw Playwright capture: `output/playwright/jarvis-tts-demo-raw.webm`
- Raw conversation capture: `output/playwright/jarvis-tts-conversation-demo-raw.webm`
- Idle screenshot: `output/playwright/jarvis-tts-demo-01-idle.png`
- Wake-confirmed screenshot: `output/playwright/jarvis-tts-demo-02-wake-confirmed.png`
- Cache-proof screenshot: `output/playwright/jarvis-tts-demo-03-cache.png`
- Conversation screenshot: `output/playwright/jarvis-tts-conversation-demo.png`
- Machine-readable proof: `output/playwright/jarvis-tts-demo-proof.json`
- Conversation proof: `output/playwright/jarvis-tts-conversation-demo-proof.json`

## Runtime Evidence

- Gateway URL: `http://127.0.0.1:42617/voice-activation`
- TTS endpoint: `http://127.0.0.1:9880/`
- Local GPT-SoVITS runtime used by the app: `/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits`
- TTS bind check: `lsof` reported `TCP 127.0.0.1:9880 (LISTEN)`.
- TTS cache directory: `/Users/heodongun/.zeroclaw/data/cache/yuni-tts`
- Public health reported `channel:voice_wake.jarvis.status = ok`, `channels.status = ok`, `gateway.status = ok`, and `local_tts.yuni.status = ok`.
- `launchctl print gui/501/ai.zeroclaw.jarvis-daemon` reported the daemon running from `target/debug/zeroclaw daemon --host 127.0.0.1 --port 42617`.
- `/api/tts/status` reported `status=ready`, `bind_host=127.0.0.1`, and `port=9880`.
- Same text cache test:
  - First request: `cached=false`
  - Second request: `cached=true`

## Commands Run

```bash
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin /opt/homebrew/bin/cargo check -p zeroclaw-gateway
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin /opt/homebrew/bin/cargo test -p zeroclaw-gateway api_tts
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin /opt/homebrew/bin/cargo build --bin zeroclaw
npm --prefix web run build
npm --prefix web audit --audit-level=critical
curl -sS http://127.0.0.1:42617/api/tts/status
curl -sS -X POST http://127.0.0.1:42617/api/tts/speak -H 'Content-Type: application/json' -d '{"text":"시연 기록용 테스트 문장입니다. 로컬 캐시가 다시 쓰이는지 확인합니다."}'
curl -sS -X POST http://127.0.0.1:42617/api/tts/speak -H 'Content-Type: application/json' -d '{"text":"최종 검증용 문장입니다. 로컬 GPT SoVITS 캐시가 정상 동작하는지 확인합니다."}'
curl -sS -X POST http://127.0.0.1:42617/api/tts/speak -H 'Content-Type: application/json' -d '{"text":"런타임 경로 변경 후 최종 합성 확인입니다. 같은 문장은 캐시로 다시 재생됩니다."}'
curl -sS http://127.0.0.1:42617/health
launchctl print gui/501/ai.zeroclaw.jarvis-daemon
lsof -nP -iTCP:9880 -sTCP:LISTEN
ffprobe -v error -show_entries format=duration,size -show_streams -of json output/playwright/jarvis-tts-demo.mp4
ffprobe -v error -show_entries format=duration,size -show_streams -of json output/playwright/jarvis-tts-conversation-demo.mp4
ffmpeg -hide_banner -i output/playwright/jarvis-tts-conversation-demo.mp4 -af volumedetect -vn -sn -dn -f null -
```

## Results

| Check | Result |
| --- | --- |
| Rust gateway check | PASS |
| TTS unit tests | PASS, 4 passed |
| Web production build | PASS |
| Critical npm audit | PASS, 0 vulnerabilities |
| Page identity | PASS, title `ZeroClaw` at `/voice-activation` |
| Blank-page check | PASS, Jarvis Live2D avatar and status text rendered |
| Framework overlay | PASS, none visible |
| Console health | PASS for app errors; only PixiJS/Live2D logs and WebGL performance warnings during capture |
| Interaction proof | PASS, wake event changed visible text to `네 주인님 무엇을 도와드릴까요?` and invoked the audio play path once |
| Local bind invariant | PASS, GPT-SoVITS listens on `127.0.0.1:9880` |
| Cache invariant | PASS, second identical text reused cached WAV |
| Voice wake build/runtime | PASS, default build includes `voice-wake`; health exposes `channel:voice_wake.jarvis` as `ok` |
| Launchd daemon | PASS, `ai.zeroclaw.jarvis-daemon` is running and bound to `127.0.0.1:42617` |
| Demo video | PASS, 1280x720 MP4, 11.88s, H.264 video + AAC mono audio |
| Conversation demo video | PASS, 1280x720 MP4, 34.28s, H.264 video + AAC mono audio |
| Conversation audio | PASS, includes macOS Korean user voice turns and local GPT-SoVITS Jarvis reply turns; `volumedetect` reported mean volume `-27.3 dB`, max volume `-4.8 dB` |
| Conversation cache invariant | PASS, repeated Jarvis status reply reused cached WAV (`cached=true`) |

## Notes

- The first TTS demo fell back to Playwright when the in-app browser route was unavailable. The later conversation update used the in-app Browser for page identity/screenshot verification, then Playwright CLI plus `ffmpeg` for recording and audio muxing.
- The demo MP4 overlays short status labels for readability. The app UI underneath is the live `/voice-activation` route.
- The MP4 includes the generated Yuni acknowledgement WAV as an audio track delayed to line up with the wake-confirmed step.
- The conversation MP4 includes the wake sequence plus two user/Jarvis turns. User utterances are generated with the local macOS Korean voice; Jarvis utterances are generated through the local GPT-SoVITS server and cached by text.
- The original Desktop GPT-SoVITS tree is still the source asset, but the app runtime now uses a copied local tree under `~/.zeroclaw/runtimes/yuni-gpt-sovits`. This avoids macOS background-process stalls while opening Desktop paths from the launchd daemon.
