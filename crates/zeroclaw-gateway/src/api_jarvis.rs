//! Jarvis-specific local control endpoints.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::AppState;
use super::api::require_auth;

const JARVIS_ALIAS: &str = "jarvis";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JarvisTuning {
    pub energy_threshold: f32,
    pub clap_energy_threshold: f32,
    pub clap_window_ms: u32,
    pub clap_cooldown_ms: u32,
}

#[derive(Debug, Serialize)]
struct JarvisError {
    error: String,
}

pub async fn handle_tuning_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read();
    let Some(channel) = config.channels.voice_wake.get(JARVIS_ALIAS) else {
        return (
            StatusCode::NOT_FOUND,
            Json(JarvisError {
                error: "channels.voice_wake.jarvis is not configured".into(),
            }),
        )
            .into_response();
    };

    Json(JarvisTuning {
        energy_threshold: channel.energy_threshold,
        clap_energy_threshold: channel.clap_energy_threshold,
        clap_window_ms: channel.clap_window_ms,
        clap_cooldown_ms: channel.clap_cooldown_ms,
    })
    .into_response()
}

pub async fn handle_tuning_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JarvisTuning>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if !(0.0001..=0.1).contains(&body.energy_threshold) {
        return invalid("energy_threshold must be between 0.0001 and 0.1");
    }
    if !(0.001..=0.2).contains(&body.clap_energy_threshold) {
        return invalid("clap_energy_threshold must be between 0.001 and 0.2");
    }
    if !(200..=5_000).contains(&body.clap_window_ms) {
        return invalid("clap_window_ms must be between 200 and 5000");
    }
    if !(20..=1_000).contains(&body.clap_cooldown_ms) {
        return invalid("clap_cooldown_ms must be between 20 and 1000");
    }

    let mut updated = state.config.read().clone();
    let Some(channel) = updated.channels.voice_wake.get_mut(JARVIS_ALIAS) else {
        return (
            StatusCode::NOT_FOUND,
            Json(JarvisError {
                error: "channels.voice_wake.jarvis is not configured".into(),
            }),
        )
            .into_response();
    };

    channel.energy_threshold = body.energy_threshold;
    channel.clap_energy_threshold = body.clap_energy_threshold;
    channel.clap_window_ms = body.clap_window_ms;
    channel.clap_cooldown_ms = body.clap_cooldown_ms;

    if let Err(err) = updated.save_dirty().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JarvisError {
                error: format!("save Jarvis tuning failed: {err:#}"),
            }),
        )
            .into_response();
    }

    *state.config.write() = updated;
    Json(body).into_response()
}

fn invalid(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(JarvisError {
            error: message.to_string(),
        }),
    )
        .into_response()
}
