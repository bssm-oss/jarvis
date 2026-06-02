//! Desktop-side bridge for voice activation events.

use crate::commands::window;
use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;
use tauri::{AppHandle, Runtime};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const SEEN_LIMIT: usize = 200;

pub fn spawn_voice_activation_poller<R: Runtime>(app: AppHandle<R>, state: SharedState) {
    tauri::async_runtime::spawn(async move {
        let mut seen = RecentEventIds::default();
        let mut seeded = false;

        loop {
            let (url, token) = {
                let s = state.read().await;
                (s.gateway_url.clone(), s.token.clone())
            };

            let client = GatewayClient::new(&url, token.as_deref());
            if let Ok(events) = client.get_voice_activation_logs(20).await {
                let mut ordered = events;
                ordered.reverse();

                for event in ordered {
                    let Some(id) = event_id(&event) else {
                        continue;
                    };
                    if !seen.insert(id) {
                        continue;
                    }

                    if seeded
                        && should_show_voice_activation(&event)
                        && let Some(signal) = signal_from_event(&event)
                    {
                        let _ = window::show_voice_activation_window(&app, Some(&signal));
                    }
                }

                seeded = true;
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

#[derive(Default)]
struct RecentEventIds {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl RecentEventIds {
    fn insert(&mut self, id: String) -> bool {
        if !self.set.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id.clone());
        while self.order.len() > SEEN_LIMIT {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

fn event_id(event: &Value) -> Option<String> {
    let id = event.get("id")?.as_str()?;
    let phase = event
        .get("attributes")
        .and_then(|attrs| attrs.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(format!("{id}:{phase}"))
}

fn should_show_voice_activation(event: &Value) -> bool {
    let Some(phase) = event
        .get("attributes")
        .and_then(|attrs| attrs.get("phase"))
        .and_then(Value::as_str)
    else {
        return false;
    };

    matches!(
        phase,
        "double_clap_detected" | "wake_name_audio_started" | "wake_confirmed"
    )
}

fn signal_from_event(event: &Value) -> Option<Value> {
    let attrs = event.get("attributes")?;
    let phase = attrs.get("phase")?.as_str()?;
    let ack_text = attrs
        .get("ack_text")
        .and_then(Value::as_str)
        .unwrap_or("네 주인님 무엇을 도와드릴까요?");
    let amplitude = attrs.get("energy").and_then(Value::as_f64);
    let created_at = event
        .get("@timestamp")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_default();

    Some(serde_json::json!({
        "phase": phase,
        "ackText": ack_text,
        "amplitude": amplitude,
        "createdAt": created_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::{should_show_voice_activation, signal_from_event};
    use serde_json::json;

    #[test]
    fn double_clap_event_shows_voice_activation() {
        let event = json!({
            "id": "1",
            "@timestamp": "2026-06-02T00:01:30.124Z",
            "attributes": {
                "phase": "double_clap_detected",
                "energy": 0.02,
                "voice_activation": "gesture"
            }
        });

        assert!(should_show_voice_activation(&event));
        let signal = signal_from_event(&event).unwrap();
        assert_eq!(signal["phase"], "double_clap_detected");
        assert_eq!(signal["ackText"], "네 주인님 무엇을 도와드릴까요?");
    }

    #[test]
    fn no_wake_word_does_not_show_voice_activation() {
        let event = json!({
            "id": "2",
            "attributes": {
                "phase": "no_wake_word",
                "voice_activation": "gesture"
            }
        });

        assert!(!should_show_voice_activation(&event));
    }
}
