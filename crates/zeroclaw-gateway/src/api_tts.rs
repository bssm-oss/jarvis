//! Local GPT-SoVITS bridge for the web dashboard.

use super::AppState;
use super::api::require_auth;
use anyhow::{Context, Result, bail};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path as FsPath, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, time};

const TTS_COMPONENT: &str = "local_tts.yuni";
const TTS_HOST: &str = "127.0.0.1";
const TTS_PORT: u16 = 9880;
const TTS_ENDPOINT: &str = "http://127.0.0.1:9880/";
const CONDA_BIN: &str = "/opt/homebrew/bin/conda";
const GPT_SOVITS_ROOT: &str = "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits";
const GPT_SOVITS_API: &str = "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits/api.py";
const SOVITS_MODEL: &str = "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits/SoVITS_weights_v2ProPlus/yuni_vocals_v2proplus_fresh_e5_s1360.pth";
const GPT_MODEL: &str = "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits/GPT_weights_v2ProPlus/yuni_vocals_v2proplus_fresh-e5.ckpt";
const REF_WAV: &str =
    "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits/reference/general_ref.wav";
const REF_TEXT_PATH: &str =
    "/Users/heodongun/.zeroclaw/runtimes/yuni-gpt-sovits/reference/general_ref.txt";
const REF_TEXT: &str =
    "그러면은 이 사보에서 뭐 하고 싶은 거 있어? 그냥 자유로운 영혼이 되어서 여기저기 분탕 치는 거";
const TTS_DEVICE: &str = "cpu";
const MAX_SEGMENT_CHARS: usize = 220;

static TTS_MANAGER: OnceLock<LocalTtsManager> = OnceLock::new();

#[derive(Debug)]
pub struct LocalTtsManager {
    cache_dir: PathBuf,
    client: reqwest::Client,
    child: parking_lot::Mutex<Option<Child>>,
}

#[derive(Debug, Serialize)]
pub struct TtsStatusResponse {
    pub status: &'static str,
    pub endpoint: &'static str,
    pub cache_dir: String,
    pub bind_host: &'static str,
    pub port: u16,
}

#[derive(Debug, Serialize)]
pub struct TtsSpeakResponse {
    pub status: &'static str,
    pub endpoint: &'static str,
    pub cache_dir: String,
    pub segments: Vec<TtsSegmentResponse>,
}

#[derive(Debug, Serialize)]
pub struct TtsSegmentResponse {
    pub text: String,
    pub url: String,
    pub cached: bool,
}

#[derive(Debug, Deserialize)]
pub struct TtsSpeakBody {
    pub text: String,
}

#[derive(Debug, Serialize)]
struct TtsErrorResponse {
    status: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct GptSovitsRequest<'a> {
    text: &'a str,
    text_language: &'static str,
    cut_punc: &'static str,
    top_k: u32,
    top_p: f32,
    temperature: f32,
    speed: f32,
    sample_steps: u32,
    if_sr: bool,
}

impl LocalTtsManager {
    fn new(cache_dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("reqwest client");

        Self {
            cache_dir,
            client,
            child: parking_lot::Mutex::new(None),
        }
    }

    async fn start_if_needed(&self) -> Result<TtsStatusResponse> {
        fs::create_dir_all(&self.cache_dir)
            .with_context(|| format!("create TTS cache dir {}", self.cache_dir.display()))?;
        self.reap_child()?;

        if Self::is_listening().await {
            zeroclaw_runtime::health::mark_component_ok(TTS_COMPONENT);
            return Ok(self.status("ready"));
        }

        self.spawn_if_needed()?;
        if self.wait_until_ready(Duration::from_secs(2)).await {
            zeroclaw_runtime::health::mark_component_ok(TTS_COMPONENT);
            Ok(self.status("ready"))
        } else {
            Ok(self.status("starting"))
        }
    }

    async fn ensure_ready(&self) -> Result<()> {
        let status = self.start_if_needed().await?;
        if status.status == "ready" {
            return Ok(());
        }

        if self.wait_until_ready(Duration::from_secs(90)).await {
            zeroclaw_runtime::health::mark_component_ok(TTS_COMPONENT);
            Ok(())
        } else {
            let message = "GPT-SoVITS server did not become ready on 127.0.0.1:9880";
            zeroclaw_runtime::health::mark_component_error(TTS_COMPONENT, message);
            bail!(message)
        }
    }

    async fn speak(&self, text: &str, path_prefix: &str) -> Result<TtsSpeakResponse> {
        self.ensure_ready().await?;

        let segments = split_text(text);
        let mut responses = Vec::with_capacity(segments.len());
        for segment in segments {
            let file_name = cache_file_name(&segment);
            let file_path = self.cache_dir.join(&file_name);
            let cached = usable_cached_audio(&file_path);

            if !cached {
                let audio = self
                    .synthesize_segment(&segment)
                    .await
                    .with_context(|| format!("synthesize TTS segment: {segment}"))?;
                fs::write(&file_path, audio)
                    .with_context(|| format!("write TTS cache {}", file_path.display()))?;
            }

            responses.push(TtsSegmentResponse {
                text: segment,
                url: format!("{path_prefix}/api/tts/audio/{file_name}"),
                cached,
            });
        }

        zeroclaw_runtime::health::mark_component_ok(TTS_COMPONENT);
        Ok(TtsSpeakResponse {
            status: "ready",
            endpoint: TTS_ENDPOINT,
            cache_dir: self.cache_dir.display().to_string(),
            segments: responses,
        })
    }

    fn audio_path(&self, file_name: &str) -> PathBuf {
        self.cache_dir.join(file_name)
    }

    fn status(&self, status: &'static str) -> TtsStatusResponse {
        TtsStatusResponse {
            status,
            endpoint: TTS_ENDPOINT,
            cache_dir: self.cache_dir.display().to_string(),
            bind_host: TTS_HOST,
            port: TTS_PORT,
        }
    }

    fn reap_child(&self) -> Result<()> {
        let mut child = self.child.lock();
        if let Some(running) = child.as_mut()
            && running.try_wait()?.is_some()
        {
            *child = None;
        }
        Ok(())
    }

    fn spawn_if_needed(&self) -> Result<()> {
        validate_runtime_files()?;

        let mut child = self.child.lock();
        if let Some(running) = child.as_mut()
            && running.try_wait()?.is_none()
        {
            return Ok(());
        }

        let port = TTS_PORT.to_string();
        let log_path = self.cache_dir.join("yuni-tts.log");
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("open GPT-SoVITS log {}", log_path.display()))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| format!("clone GPT-SoVITS log {}", log_path.display()))?;

        let process = Command::new(CONDA_BIN)
            .args(["run", "--no-capture-output", "-n", "GPTSoVits"])
            .args(["python", GPT_SOVITS_API])
            .args(["-s", SOVITS_MODEL])
            .args(["-g", GPT_MODEL])
            .args(["-dr", REF_WAV])
            .arg("-dt")
            .arg(REF_TEXT)
            .args(["-dl", "ko"])
            .args(["-d", TTS_DEVICE])
            .args(["-a", TTS_HOST])
            .args(["-p", port.as_str()])
            .arg("-fp")
            .args(["-mt", "wav"])
            .current_dir(GPT_SOVITS_ROOT)
            .env("HOST", TTS_HOST)
            .env("PORT", TTS_PORT.to_string())
            .env("SOVITS", SOVITS_MODEL)
            .env("GPT", GPT_MODEL)
            .env("REF_WAV", REF_WAV)
            .env("REF_TEXT_PATH", REF_TEXT_PATH)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONPATH", GPT_SOVITS_ROOT)
            .env(
                "PATH",
                "/opt/homebrew/bin:/opt/homebrew/Caskroom/miniconda/base/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .with_context(|| format!("start GPT-SoVITS API via {CONDA_BIN}"))?;

        *child = Some(process);
        Ok(())
    }

    async fn is_listening() -> bool {
        matches!(
            time::timeout(
                Duration::from_millis(400),
                TcpStream::connect((TTS_HOST, TTS_PORT)),
            )
            .await,
            Ok(Ok(_))
        )
    }

    async fn wait_until_ready(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if Self::is_listening().await {
                return true;
            }
            time::sleep(Duration::from_millis(500)).await;
        }
        false
    }

    async fn synthesize_segment(&self, text: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .post(TTS_ENDPOINT)
            .json(&GptSovitsRequest {
                text,
                text_language: "ko",
                cut_punc: ".?!。？！",
                top_k: 15,
                top_p: 1.0,
                temperature: 1.0,
                speed: 1.0,
                sample_steps: 32,
                if_sr: false,
            })
            .send()
            .await
            .context("POST GPT-SoVITS")?;

        let status = response.status();
        let bytes = response.bytes().await.context("read GPT-SoVITS response")?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            bail!("GPT-SoVITS returned {status}: {body}");
        }
        if bytes.len() < 44 {
            bail!("GPT-SoVITS returned an empty WAV response");
        }

        Ok(bytes.to_vec())
    }
}

pub async fn handle_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match manager_for_state(&state).start_if_needed().await {
        Ok(status) => Json(status).into_response(),
        Err(err) => {
            zeroclaw_runtime::health::mark_component_error(TTS_COMPONENT, err.to_string());
            error_response(StatusCode::SERVICE_UNAVAILABLE, err)
        }
    }
}

pub async fn handle_speak(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TtsSpeakBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let text = body.text.trim();
    if text.is_empty() {
        return Json(TtsSpeakResponse {
            status: "ready",
            endpoint: TTS_ENDPOINT,
            cache_dir: manager_for_state(&state).cache_dir.display().to_string(),
            segments: Vec::new(),
        })
        .into_response();
    }

    match manager_for_state(&state)
        .speak(text, &state.path_prefix)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(err) => {
            zeroclaw_runtime::health::mark_component_error(TTS_COMPONENT, err.to_string());
            error_response(StatusCode::SERVICE_UNAVAILABLE, err)
        }
    }
}

pub async fn handle_audio(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(file_name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    if !is_safe_cache_file(&file_name) {
        return error_response(StatusCode::BAD_REQUEST, "invalid TTS cache file name");
    }

    match fs::read(manager_for_state(&state).audio_path(&file_name)) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "audio/wav"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            error_response(StatusCode::NOT_FOUND, "TTS cache file not found")
        }
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

fn manager_for_state(state: &AppState) -> &'static LocalTtsManager {
    let cache_dir = state.config.read().data_dir.join("cache").join("yuni-tts");
    TTS_MANAGER.get_or_init(|| LocalTtsManager::new(cache_dir))
}

fn validate_runtime_files() -> Result<()> {
    for path in [
        CONDA_BIN,
        GPT_SOVITS_API,
        SOVITS_MODEL,
        GPT_MODEL,
        REF_WAV,
        REF_TEXT_PATH,
    ] {
        if !FsPath::new(path).is_file() {
            bail!("missing GPT-SoVITS runtime file: {path}");
        }
    }
    if !FsPath::new(GPT_SOVITS_ROOT).is_dir() {
        bail!("missing GPT-SoVITS runtime root: {GPT_SOVITS_ROOT}");
    }
    Ok(())
}

fn usable_cached_audio(path: &FsPath) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.len() > 44)
}

fn cache_file_name(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"yuni-gpt-sovits-v1\0");
    hasher.update(text.as_bytes());
    format!("{}.wav", hex::encode(hasher.finalize()))
}

fn is_safe_cache_file(file_name: &str) -> bool {
    file_name.len() == 68
        && file_name.ends_with(".wav")
        && file_name[..64].chars().all(|c| c.is_ascii_hexdigit())
}

fn split_text(text: &str) -> Vec<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentences = Vec::new();
    let mut current = String::new();

    for ch in normalized.chars() {
        current.push(ch);
        if is_sentence_boundary(ch) {
            push_split_sentence(&mut sentences, current.trim());
            current.clear();
        }
    }
    push_split_sentence(&mut sentences, current.trim());

    let mut segments = Vec::new();
    let mut buffer = String::new();
    let mut sentence_count = 0;

    for sentence in sentences {
        let combined_chars =
            char_count(&buffer) + char_count(&sentence) + usize::from(!buffer.is_empty());
        if !buffer.is_empty() && (sentence_count >= 2 || combined_chars > MAX_SEGMENT_CHARS) {
            segments.push(buffer);
            buffer = sentence;
            sentence_count = 1;
        } else {
            if !buffer.is_empty() {
                buffer.push(' ');
            }
            buffer.push_str(&sentence);
            sentence_count += 1;
        }
    }
    if !buffer.trim().is_empty() {
        segments.push(buffer);
    }

    segments
}

fn push_split_sentence(out: &mut Vec<String>, sentence: &str) {
    if sentence.is_empty() {
        return;
    }
    if char_count(sentence) <= MAX_SEGMENT_CHARS {
        out.push(sentence.to_string());
        return;
    }

    let mut current = String::new();
    for word in sentence.split_whitespace() {
        if char_count(word) > MAX_SEGMENT_CHARS {
            if !current.trim().is_empty() {
                out.push(current);
                current = String::new();
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if char_count(&chunk) >= MAX_SEGMENT_CHARS {
                    out.push(chunk);
                    chunk = String::new();
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                out.push(chunk);
            }
            continue;
        }

        let projected = char_count(&current) + char_count(word) + usize::from(!current.is_empty());
        if !current.is_empty() && projected > MAX_SEGMENT_CHARS {
            out.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
}

fn is_sentence_boundary(ch: char) -> bool {
    matches!(ch, '.' | '?' | '!' | '。' | '？' | '！' | '\n')
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn error_response(status: StatusCode, message: impl ToString) -> Response {
    (
        status,
        Json(TtsErrorResponse {
            status: "error",
            message: message.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_text_packs_one_or_two_sentences() {
        let segments = split_text("첫 문장입니다. 둘째 문장입니다. 셋째 문장입니다!");
        assert_eq!(
            segments,
            vec!["첫 문장입니다. 둘째 문장입니다.", "셋째 문장입니다!"]
        );
    }

    #[test]
    fn split_text_splits_long_sentences() {
        let long = "가나다 ".repeat(90);
        let segments = split_text(&long);
        assert!(segments.len() > 1);
        assert!(
            segments
                .iter()
                .all(|segment| char_count(segment) <= MAX_SEGMENT_CHARS)
        );
    }

    #[test]
    fn cache_file_name_is_safe_and_stable() {
        let first = cache_file_name("네 주인님 무엇을 도와드릴까요?");
        let second = cache_file_name("네 주인님 무엇을 도와드릴까요?");
        assert_eq!(first, second);
        assert!(is_safe_cache_file(&first));
    }

    #[test]
    fn cache_file_name_rejects_traversal() {
        assert!(!is_safe_cache_file("../secret.wav"));
        assert!(!is_safe_cache_file("not-hex.wav"));
    }
}
