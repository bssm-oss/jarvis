# Jarvis Local GPT-SoVITS TTS Verification - 2026-06-01

## Scope

Flow under test:

`/voice-activation` loads -> wake confirmation event displays the Jarvis acknowledgement -> local GPT-SoVITS speaks through the web audio path -> repeated text reuses the cached WAV.

## Artifacts

- Demo video with Yuni TTS audio track: `output/playwright/jarvis-tts-demo.mp4`
- Raw Playwright capture: `output/playwright/jarvis-tts-demo-raw.webm`
- Idle screenshot: `output/playwright/jarvis-tts-demo-01-idle.png`
- Wake-confirmed screenshot: `output/playwright/jarvis-tts-demo-02-wake-confirmed.png`
- Cache-proof screenshot: `output/playwright/jarvis-tts-demo-03-cache.png`
- Machine-readable proof: `output/playwright/jarvis-tts-demo-proof.json`

## Runtime Evidence

- Gateway URL: `http://127.0.0.1:42617/voice-activation`
- TTS endpoint: `http://127.0.0.1:9880/`
- TTS bind check: `lsof` reported `TCP 127.0.0.1:9880 (LISTEN)`.
- TTS cache directory: `/Users/heodongun/.zeroclaw/data/cache/yuni-tts`
- Public health reported `local_tts.yuni.status = ok`.
- Same text cache test:
  - First request: `cached=false`
  - Second request: `cached=true`

## Commands Run

```bash
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin /opt/homebrew/bin/cargo check -p zeroclaw-gateway
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin /opt/homebrew/bin/cargo test -p zeroclaw-gateway api_tts
npm --prefix web run build
npm --prefix web audit --audit-level=critical
curl -sS http://127.0.0.1:42617/api/tts/status
curl -sS -X POST http://127.0.0.1:42617/api/tts/speak -H 'Content-Type: application/json' -d '{"text":"시연 기록용 테스트 문장입니다. 로컬 캐시가 다시 쓰이는지 확인합니다."}'
lsof -nP -iTCP:9880 -sTCP:LISTEN
ffprobe -v error -show_entries format=duration,size -show_streams -of json output/playwright/jarvis-tts-demo.mp4
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
| Demo video | PASS, 1280x720 MP4, 11.88s, H.264 video + AAC mono audio |

## Notes

- Browser plugin setup was attempted first, but the in-app browser route was unavailable for this session. Validation and recording used the available Playwright runtime.
- The demo MP4 overlays short status labels for readability. The app UI underneath is the live `/voice-activation` route.
- The MP4 includes the generated Yuni acknowledgement WAV as an audio track delayed to line up with the wake-confirmed step.
