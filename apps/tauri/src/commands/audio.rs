use reqwest::Url;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TTS_AUDIO_BYTES: usize = 20 * 1024 * 1024;

#[tauri::command]
pub async fn play_local_tts_audio(url: String) -> Result<(), String> {
    let url = validate_local_tts_url(&url)?;
    let audio = download_tts_audio(url).await?;
    let temp_path = unique_temp_audio_path();

    tokio::fs::write(&temp_path, audio)
        .await
        .map_err(|e| format!("write temporary TTS audio: {e}"))?;

    play_temp_audio(temp_path).await
}

fn validate_local_tts_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid TTS audio URL: {e}"))?;
    if url.scheme() != "http" {
        return Err("TTS audio URL must use http".into());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "TTS audio URL must include a host".to_string())?;
    if host != "127.0.0.1" && host != "localhost" {
        return Err("TTS audio URL must be local-only".into());
    }

    if !url.path().contains("/api/tts/audio/") {
        return Err("TTS audio URL must point to /api/tts/audio/".into());
    }

    Ok(url)
}

async fn download_tts_audio(url: Url) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("create local TTS audio client: {e}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download local TTS audio: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "download local TTS audio returned {}",
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("read local TTS audio: {e}"))?;
    if bytes.len() > MAX_TTS_AUDIO_BYTES {
        return Err("local TTS audio is too large to play safely".into());
    }

    Ok(bytes.to_vec())
}

fn unique_temp_audio_path() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("zeroclaw-tts-{}-{now}.wav", std::process::id()))
}

#[cfg(target_os = "macos")]
async fn play_temp_audio(path: PathBuf) -> Result<(), String> {
    let player_path = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("/usr/bin/afplay")
            .arg(&player_path)
            .status()
    })
    .await
    .map_err(|e| format!("join native audio player: {e}"))?;

    let _ = tokio::fs::remove_file(&path).await;

    let status = result.map_err(|e| format!("start native audio player: {e}"))?;
    if !status.success() {
        return Err(format!("native audio player exited with {status}"));
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn play_temp_audio(path: PathBuf) -> Result<(), String> {
    let _ = tokio::fs::remove_file(path).await;
    Err("native TTS audio playback is currently implemented for macOS only".into())
}

#[cfg(test)]
mod tests {
    use super::validate_local_tts_url;

    #[test]
    fn accepts_local_tts_audio_url() {
        assert!(validate_local_tts_url("http://127.0.0.1:42617/api/tts/audio/a.wav").is_ok());
        assert!(validate_local_tts_url("http://localhost:42617/api/tts/audio/a.wav").is_ok());
    }

    #[test]
    fn rejects_external_tts_audio_url() {
        assert!(validate_local_tts_url("https://127.0.0.1:42617/api/tts/audio/a.wav").is_err());
        assert!(validate_local_tts_url("http://example.com/api/tts/audio/a.wav").is_err());
        assert!(validate_local_tts_url("http://127.0.0.1:42617/other/a.wav").is_err());
    }
}
