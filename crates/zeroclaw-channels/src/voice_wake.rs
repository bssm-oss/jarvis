//! Voice Wake Word detection channel.
//!
//! Listens on the default microphone via `cpal`, detects a configurable wake
//! word using energy-based VAD followed by transcription-based keyword matching,
//! then captures the subsequent utterance and dispatches it as a channel message.
//!
//! Gated behind the `voice-wake` Cargo feature.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::transcription::TranscriptionManager;
use zeroclaw_config::schema::TranscriptionConfig;
use zeroclaw_config::schema::VoiceWakeConfig;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

type TranscriptionProviderResolver = Arc<dyn Fn() -> String + Send + Sync>;

// ── State machine ──────────────────────────────────────────────

/// Internal states for the wake-word detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeState {
    /// Passively monitoring microphone energy levels.
    Listening,
    /// Energy spike detected — capturing a short window to check for wake word.
    Triggered,
    /// Wake word confirmed — capturing the full utterance that follows.
    Capturing,
    /// Captured audio is being transcribed.
    Processing,
}

impl std::fmt::Display for WakeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listening => write!(f, "Listening"),
            Self::Triggered => write!(f, "Triggered"),
            Self::Capturing => write!(f, "Capturing"),
            Self::Processing => write!(f, "Processing"),
        }
    }
}

/// Result of passing an audio-energy sample through the clap gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapGateEvent {
    None,
    FirstClap,
    Complete,
    Expired,
}

/// Energy-based detector for a short clap sequence before wake-word capture.
pub struct ClapDetector {
    required_count: u8,
    threshold: f32,
    window: Duration,
    cooldown: Duration,
    count: u8,
    first_clap_at: Option<Instant>,
    last_clap_at: Option<Instant>,
}

impl ClapDetector {
    #[must_use]
    pub fn new(required_count: u8, threshold: f32, window: Duration, cooldown: Duration) -> Self {
        Self {
            required_count: required_count.max(1),
            threshold,
            window,
            cooldown,
            count: 0,
            first_clap_at: None,
            last_clap_at: None,
        }
    }

    pub fn reset(&mut self) {
        self.count = 0;
        self.first_clap_at = None;
        self.last_clap_at = None;
    }

    pub fn observe(&mut self, energy: f32, now: Instant) -> ClapGateEvent {
        let mut expired = false;
        if let Some(first_clap_at) = self.first_clap_at
            && now.duration_since(first_clap_at) > self.window
        {
            self.reset();
            expired = true;
        }

        if !is_clap_energy(energy, self.threshold) {
            return if expired {
                ClapGateEvent::Expired
            } else {
                ClapGateEvent::None
            };
        }

        if let Some(last_clap_at) = self.last_clap_at
            && now.duration_since(last_clap_at) < self.cooldown
        {
            return ClapGateEvent::None;
        }

        if self.count == 0 {
            self.first_clap_at = Some(now);
        }

        self.last_clap_at = Some(now);
        self.count = self.count.saturating_add(1);

        if self.count >= self.required_count {
            self.reset();
            ClapGateEvent::Complete
        } else {
            ClapGateEvent::FirstClap
        }
    }
}

// ── Channel implementation ─────────────────────────────────────

/// Voice wake-word channel that activates on a spoken keyword.
pub struct VoiceWakeChannel {
    config: VoiceWakeConfig,
    transcription_config: TranscriptionConfig,
    /// Resolves the owning agent's current STT provider reference at use-time.
    transcription_provider_resolver: TranscriptionProviderResolver,
    /// The alias key under `[channels.voice_wake.<alias>]` this handle is
    /// bound to. Used for attribution.
    alias: String,
}

impl VoiceWakeChannel {
    /// Create a new `VoiceWakeChannel` from its config sections.
    pub fn new(
        alias: impl Into<String>,
        config: VoiceWakeConfig,
        transcription_config: TranscriptionConfig,
    ) -> Self {
        Self::new_with_transcription_provider_resolver(
            alias,
            config,
            transcription_config,
            Arc::new(String::new),
        )
    }

    /// Create a new `VoiceWakeChannel` with an agent-scoped STT resolver.
    pub fn new_with_transcription_provider_resolver(
        alias: impl Into<String>,
        config: VoiceWakeConfig,
        transcription_config: TranscriptionConfig,
        transcription_provider_resolver: TranscriptionProviderResolver,
    ) -> Self {
        Self {
            config,
            transcription_config,
            transcription_provider_resolver,
            alias: alias.into(),
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for VoiceWakeChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::VoiceWake,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for VoiceWakeChannel {
    fn name(&self) -> &str {
        "voice_wake"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        // Voice wake is input-only; outbound messages are not supported.
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let self_alias = self.alias.clone();
        let config = self.config.clone();
        let mut transcription_manager = TranscriptionManager::new(&self.transcription_config)?;
        let resolved_transcription_provider = (self.transcription_provider_resolver)();
        if resolved_transcription_provider.trim().is_empty() {
            let sole_provider = {
                let names = transcription_manager.available_providers();
                if names.len() == 1 {
                    Some(names[0].to_string())
                } else {
                    None
                }
            };
            if let Some(provider) = sole_provider {
                transcription_manager =
                    transcription_manager.with_agent_transcription_provider(provider);
            }
        } else {
            transcription_manager = transcription_manager
                .with_agent_transcription_provider(resolved_transcription_provider);
        }

        // Run the blocking audio capture loop on a dedicated thread.
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(4);

        let energy_threshold = config.energy_threshold;
        let silence_timeout = Duration::from_millis(u64::from(config.silence_timeout_ms));
        let max_capture = Duration::from_secs(u64::from(config.max_capture_secs));
        let clap_gate_enabled = config.clap_gate_enabled;
        let clap_gate_timeout = Duration::from_millis(u64::from(config.clap_gate_timeout_ms));
        let post_wake_capture_delay =
            Duration::from_millis(u64::from(config.post_wake_capture_delay_ms));
        let mut clap_detector = ClapDetector::new(
            config.clap_count,
            config.clap_energy_threshold,
            Duration::from_millis(u64::from(config.clap_window_ms)),
            Duration::from_millis(u64::from(config.clap_cooldown_ms)),
        );
        let sample_rate: u32;
        let channels_count: u16;

        // ── Initialise cpal stream ────────────────────────────
        {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();
            let device = host.default_input_device().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "No default audio input device available"
                );
                anyhow::Error::msg("No default audio input device available")
            })?;

            let supported = device.default_input_config()?;
            sample_rate = supported.sample_rate().0;
            channels_count = supported.channels();

            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"device": device.name().unwrap_or_default(), "sample_rate": sample_rate, "channels": channels_count})), "VoiceWake: opening audio input");

            let stream_config: cpal::StreamConfig = supported.into();
            let audio_tx_clone = audio_tx.clone();

            let stream = device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Non-blocking: try_send and drop if full.
                    let _ = audio_tx_clone.try_send(data.to_vec());
                },
                move |err| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                        "VoiceWake: audio stream error"
                    );
                },
                None,
            )?;

            stream.play()?;

            // Keep the stream alive for the lifetime of the channel.
            // We leak it intentionally — the channel runs until the daemon shuts down.
            std::mem::forget(stream);
        }

        // Drop the extra sender so the channel closes when the stream sender drops.
        drop(audio_tx);

        // ── Main detection loop ───────────────────────────────
        let wake_word = config.wake_word.to_lowercase();
        let mut state = WakeState::Listening;
        let mut capture_buf: Vec<f32> = Vec::new();
        let mut last_voice_at = Instant::now();
        let mut capture_start = Instant::now();
        let mut capture_not_before = Instant::now();
        let mut capture_has_voice = false;
        let mut msg_counter: u64 = 0;
        let mut clap_gate_armed = false;
        let mut triggered_has_voice = false;
        let mut last_energy_probe_log_at = Instant::now();

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "wake_word": wake_word,
                    "clap_gate_enabled": clap_gate_enabled,
                    "clap_count": config.clap_count,
                })
            ),
            "VoiceWake: entering listen loop"
        );

        while let Some(chunk) = audio_rx.recv().await {
            let energy = compute_rms_energy(&chunk);

            match state {
                WakeState::Listening => {
                    if clap_gate_enabled {
                        let now = Instant::now();
                        if energy >= config.clap_energy_threshold * 0.5
                            && now.duration_since(last_energy_probe_log_at)
                                >= Duration::from_millis(250)
                        {
                            last_energy_probe_log_at = now;
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "voice_activation": "gesture",
                                    "phase": "energy_probe",
                                    "energy": energy,
                                    "clap_energy_threshold": config.clap_energy_threshold,
                                })),
                                "VoiceWake: energy probe near clap threshold"
                            );
                        }
                        match clap_detector.observe(energy, Instant::now()) {
                            ClapGateEvent::FirstClap => {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "voice_activation": "gesture",
                                            "phase": "first_clap",
                                            "energy": energy,
                                        })
                                    ),
                                    "VoiceWake: first clap detected"
                                );
                            }
                            ClapGateEvent::Complete => {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "voice_activation": "gesture",
                                            "phase": "double_clap_detected",
                                            "energy": energy,
                                        })
                                    ),
                                    "VoiceWake: clap gesture complete — waiting for wake word"
                                );
                                state = WakeState::Triggered;
                                clap_gate_armed = true;
                                triggered_has_voice = false;
                                capture_buf.clear();
                                last_voice_at = Instant::now();
                                capture_start = Instant::now();
                            }
                            ClapGateEvent::Expired => {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "voice_activation": "gesture",
                                            "phase": "clap_window_expired",
                                        })
                                    ),
                                    "VoiceWake: clap window expired"
                                );
                            }
                            ClapGateEvent::None => {}
                        }
                    } else if energy >= energy_threshold {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"energy": energy})),
                            "VoiceWake: energy spike — transitioning to Triggered"
                        );
                        state = WakeState::Triggered;
                        clap_gate_armed = false;
                        triggered_has_voice = true;
                        capture_buf.clear();
                        capture_buf.extend_from_slice(&chunk);
                        last_voice_at = Instant::now();
                        capture_start = Instant::now();
                    }
                }
                WakeState::Triggered => {
                    let now = Instant::now();
                    if !triggered_has_voice {
                        if energy >= energy_threshold {
                            triggered_has_voice = true;
                            last_voice_at = now;
                            capture_start = now;
                            capture_buf.clear();
                            capture_buf.extend_from_slice(&chunk);

                            if clap_gate_armed {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "voice_activation": "gesture",
                                            "phase": "wake_name_audio_started",
                                            "energy": energy,
                                        })
                                    ),
                                    "VoiceWake: wake-name audio started"
                                );
                            }
                        } else if clap_gate_armed && capture_start.elapsed() >= clap_gate_timeout {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "voice_activation": "gesture",
                                    "phase": "wake_name_timeout",
                                })),
                                "VoiceWake: wake-name timeout — back to Listening"
                            );
                            state = WakeState::Listening;
                            clap_gate_armed = false;
                            capture_buf.clear();
                        }
                        continue;
                    }

                    capture_buf.extend_from_slice(&chunk);

                    if energy >= energy_threshold {
                        last_voice_at = now;
                    }

                    let since_voice = now.duration_since(last_voice_at);
                    let since_start = now.duration_since(capture_start);

                    let should_finish_wake_capture = should_finish_wake_capture(
                        triggered_has_voice,
                        since_voice,
                        since_start,
                        silence_timeout,
                        max_capture,
                    );

                    // After the clap gate, do not send pure silence to Whisper.
                    // The user may need a moment to say the wake name after the
                    // second clap, and transcribing silence made the desktop feel
                    // like it ignored the gesture.
                    if should_finish_wake_capture && !triggered_has_voice {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "voice_activation": "gesture",
                                "phase": "wake_name_timeout_no_voice",
                                "wake_word": wake_word.as_str(),
                            })),
                            "VoiceWake: wake-name window closed without voice"
                        );
                        state = WakeState::Listening;
                        clap_gate_armed = false;
                        triggered_has_voice = false;
                        capture_has_voice = false;
                        capture_buf.clear();
                        continue;
                    }

                    // In the local Jarvis flow, the double-clap gate is the
                    // intentional gesture. Once speech follows that gesture,
                    // activate immediately instead of blocking the UI on a
                    // wake-name transcription pass.
                    if should_finish_wake_capture
                        && should_activate_clap_gated_voice_without_transcript(
                            clap_gate_armed,
                            triggered_has_voice,
                        )
                    {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "text": "[voice after clap]",
                                "voice_activation": "activated",
                                "phase": "wake_confirmed",
                                "wake_word": wake_word.as_str(),
                                "wake_match": "clap_voice_energy_fallback",
                                "ack_text": config.activation_ack_text.as_str(),
                                "post_wake_capture_delay_ms": config.post_wake_capture_delay_ms,
                                "energy": energy,
                            })),
                            "VoiceWake: clap-gated voice detected — capturing utterance"
                        );
                        let capture_ready_at = Instant::now() + post_wake_capture_delay;
                        state = WakeState::Capturing;
                        clap_gate_armed = false;
                        triggered_has_voice = false;
                        capture_has_voice = false;
                        capture_buf.clear();
                        capture_not_before = capture_ready_at;
                        last_voice_at = capture_ready_at;
                        capture_start = capture_ready_at;
                        continue;
                    }

                    // After enough silence or max time, transcribe to check for wake word.
                    if should_finish_wake_capture {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "voice_activation": "gesture",
                                "phase": "wake_check_transcribing",
                            })),
                            "VoiceWake: Triggered window closed — transcribing for wake word"
                        );

                        let wav_bytes =
                            encode_wav_from_f32(&capture_buf, sample_rate, channels_count);

                        match transcription_manager
                            .transcribe(&wav_bytes, "wake_check.wav")
                            .await
                        {
                            Ok(text) => {
                                let lower = text.to_lowercase();
                                let matched_by_wake_word = wake_word_matches(&lower, &wake_word);
                                let matched_by_clap_voice_fallback = clap_voice_fallback_matches(
                                    &lower,
                                    &wake_word,
                                    clap_gate_armed,
                                );
                                if matched_by_wake_word || matched_by_clap_voice_fallback {
                                    ::zeroclaw_log::record!(
                                        INFO,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(
                                            ::serde_json::json!({
                                                "text": text,
                                                "voice_activation": "activated",
                                                "phase": "wake_confirmed",
                                                "wake_word": wake_word.as_str(),
                                                "wake_match": if matched_by_wake_word { "wake_word" } else { "clap_voice_fallback" },
                                                "ack_text": config.activation_ack_text.as_str(),
                                                "post_wake_capture_delay_ms": config.post_wake_capture_delay_ms,
                                                "energy": energy,
                                            })
                                        ),
                                        "VoiceWake: wake word detected — capturing utterance"
                                    );
                                    let capture_ready_at = Instant::now() + post_wake_capture_delay;
                                    state = WakeState::Capturing;
                                    clap_gate_armed = false;
                                    triggered_has_voice = false;
                                    capture_has_voice = false;
                                    capture_buf.clear();
                                    capture_not_before = capture_ready_at;
                                    last_voice_at = capture_ready_at;
                                    capture_start = capture_ready_at;
                                } else {
                                    ::zeroclaw_log::record!(
                                        INFO,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(
                                            ::serde_json::json!({
                                                "voice_activation": "gesture",
                                                "phase": "no_wake_word",
                                                "text": text,
                                                "wake_word": wake_word.as_str(),
                                            })
                                        ),
                                        "VoiceWake: no wake word — back to Listening"
                                    );
                                    state = WakeState::Listening;
                                    clap_gate_armed = false;
                                    triggered_has_voice = false;
                                    capture_has_voice = false;
                                    capture_buf.clear();
                                }
                            }
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "VoiceWake: transcription error during wake check"
                                );
                                state = WakeState::Listening;
                                clap_gate_armed = false;
                                triggered_has_voice = false;
                                capture_has_voice = false;
                                capture_buf.clear();
                            }
                        }
                    }
                }
                WakeState::Capturing => {
                    let now = Instant::now();
                    if now < capture_not_before {
                        continue;
                    }

                    capture_buf.extend_from_slice(&chunk);

                    if energy >= energy_threshold {
                        capture_has_voice = true;
                        last_voice_at = now;
                    }

                    let since_voice = now.duration_since(last_voice_at);
                    let since_start = now.duration_since(capture_start);

                    let should_transcribe = should_finish_utterance_capture(
                        capture_has_voice,
                        since_voice,
                        since_start,
                        silence_timeout,
                        max_capture,
                    );

                    if should_transcribe {
                        if !capture_has_voice {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "voice_activation": "activated",
                                    "phase": "utterance_timeout_no_voice",
                                    "max_capture_secs": config.max_capture_secs,
                                })),
                                "VoiceWake: utterance capture timed out before voice started"
                            );
                            state = WakeState::Listening;
                            clap_gate_armed = false;
                            triggered_has_voice = false;
                            capture_buf.clear();
                            continue;
                        }

                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "voice_activation": "activated",
                                "phase": "utterance_transcribing",
                            })),
                            "VoiceWake: utterance capture complete — transcribing"
                        );

                        let wav_bytes =
                            encode_wav_from_f32(&capture_buf, sample_rate, channels_count);

                        match transcription_manager
                            .transcribe(&wav_bytes, "utterance.wav")
                            .await
                        {
                            Ok(text) => {
                                let next_msg_counter = msg_counter.saturating_add(1);
                                let ts = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();

                                if let Some(msg) = build_voice_wake_message(
                                    &self_alias,
                                    next_msg_counter,
                                    &text,
                                    ts,
                                ) {
                                    msg_counter = next_msg_counter;
                                    let dispatch_text = msg.content.clone();
                                    let content_len = dispatch_text.len();

                                    match tx.send(msg).await {
                                        Ok(()) => {
                                            ::zeroclaw_log::record!(
                                                INFO,
                                                ::zeroclaw_log::Event::new(
                                                    module_path!(),
                                                    ::zeroclaw_log::Action::Note
                                                )
                                                .with_attrs(::serde_json::json!({
                                                    "voice_activation": "task",
                                                    "phase": "utterance_dispatched",
                                                    "content_len": content_len,
                                                    "text": dispatch_text,
                                                    "raw_text": text,
                                                })),
                                                "VoiceWake: utterance dispatched"
                                            );
                                        }
                                        Err(e) => {
                                            ::zeroclaw_log::record!(
                                                WARN,
                                                ::zeroclaw_log::Event::new(
                                                    module_path!(),
                                                    ::zeroclaw_log::Action::Note
                                                )
                                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                                .with_attrs(::serde_json::json!({
                                                    "error": format!("{}", e)
                                                })),
                                                "VoiceWake: failed to dispatch message"
                                            );
                                        }
                                    }
                                } else {
                                    ::zeroclaw_log::record!(
                                        INFO,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(
                                            ::serde_json::json!({
                                                "voice_activation": "task",
                                                "phase": "utterance_empty",
                                                "raw_text": text,
                                            })
                                        ),
                                        "VoiceWake: utterance transcription was empty"
                                    );
                                }
                            }
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "VoiceWake: transcription error for utterance"
                                );
                            }
                        }

                        state = WakeState::Listening;
                        clap_gate_armed = false;
                        triggered_has_voice = false;
                        capture_has_voice = false;
                        capture_buf.clear();
                    }
                }
                WakeState::Processing => {
                    // Should not receive chunks while processing, but just buffer them.
                    // State transitions happen above synchronously after transcription.
                }
            }
        }

        bail!("VoiceWake: audio stream ended unexpectedly");
    }
}

// ── Audio utilities ────────────────────────────────────────────

/// Compute RMS (root-mean-square) energy of an audio chunk.
pub fn compute_rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn should_finish_utterance_capture(
    capture_has_voice: bool,
    since_voice: Duration,
    since_start: Duration,
    silence_timeout: Duration,
    max_capture: Duration,
) -> bool {
    (capture_has_voice && since_voice >= silence_timeout) || since_start >= max_capture
}

fn should_finish_wake_capture(
    triggered_has_voice: bool,
    since_voice: Duration,
    since_start: Duration,
    silence_timeout: Duration,
    max_capture: Duration,
) -> bool {
    (triggered_has_voice && since_voice >= silence_timeout) || since_start >= max_capture
}

fn should_activate_clap_gated_voice_without_transcript(
    clap_gate_armed: bool,
    triggered_has_voice: bool,
) -> bool {
    clap_gate_armed && triggered_has_voice
}

fn normalize_voice_utterance_text(text: &str) -> String {
    text.trim()
        .replace("최적과", "최저가")
        .replace("최적가", "최저가")
}

fn build_voice_wake_message(
    channel_alias: &str,
    msg_counter: u64,
    raw_text: &str,
    timestamp: u64,
) -> Option<ChannelMessage> {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut msg = ChannelMessage::new(
        format!("voice_wake_{msg_counter}"),
        "voice_user",
        "voice_user",
        normalize_voice_utterance_text(trimmed),
        "voice_wake",
        timestamp,
    );
    msg.channel_alias = Some(channel_alias.to_string());
    Some(msg)
}

/// Return whether an RMS energy sample should be counted as a clap peak.
#[must_use]
pub fn is_clap_energy(energy: f32, threshold: f32) -> bool {
    energy >= threshold
}

/// Return whether a transcript contains the configured wake word.
///
/// The default Jarvis wake name is often spoken in Korean on this machine, and
/// Whisper may emit either the English spelling or the Korean transliteration.
#[must_use]
pub fn wake_word_matches(transcript_lower: &str, wake_word_lower: &str) -> bool {
    if transcript_lower.contains(wake_word_lower) {
        return true;
    }

    if wake_word_lower == "jarvis" {
        if transcript_lower.chars().filter(|ch| *ch == '자').count() >= 2 {
            return true;
        }

        return ["javis", "자비스", "쟈비스", "차비스"]
            .iter()
            .any(|alias| transcript_lower.contains(alias));
    }

    false
}

/// Return whether spoken audio after the clap gate is enough to proceed.
///
/// This is intentionally restricted to the local Jarvis flow. Very short Korean
/// wake-name clips can be hallucinated by Whisper as unrelated stock phrases, so
/// after the explicit double-clap gesture a non-empty voice segment is treated
/// as a wake intent.
#[must_use]
pub fn clap_voice_fallback_matches(
    transcript_lower: &str,
    wake_word_lower: &str,
    clap_gate_armed: bool,
) -> bool {
    if !clap_gate_armed || wake_word_lower != "jarvis" {
        return false;
    }

    transcript_lower
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .count()
        >= 2
}

/// Encode raw f32 PCM samples as a WAV byte buffer (16-bit PCM).
///
/// This produces a minimal valid WAV file that Whisper-compatible APIs accept.
pub fn encode_wav_from_f32(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = u32::from(channels) * sample_rate * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    #[allow(clippy::cast_possible_truncation)]
    let data_len = (samples.len() * 2) as u32; // 16-bit = 2 bytes per sample; max ~25 MB
    let file_len = 36 + data_len;

    let mut buf = Vec::with_capacity(file_len as usize + 8);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_len.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let pcm16 = (clamped * 32767.0) as i16; // clamped to [-1,1] so fits i16
        buf.extend_from_slice(&pcm16.to_le_bytes());
    }

    buf
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::traits::ChannelConfig;

    // ── State machine tests ────────────────────────────────

    #[test]
    fn wake_state_display() {
        assert_eq!(WakeState::Listening.to_string(), "Listening");
        assert_eq!(WakeState::Triggered.to_string(), "Triggered");
        assert_eq!(WakeState::Capturing.to_string(), "Capturing");
        assert_eq!(WakeState::Processing.to_string(), "Processing");
    }

    #[test]
    fn wake_state_equality() {
        assert_eq!(WakeState::Listening, WakeState::Listening);
        assert_ne!(WakeState::Listening, WakeState::Triggered);
    }

    // ── Energy computation tests ───────────────────────────

    #[test]
    fn rms_energy_of_silence_is_zero() {
        let silence = vec![0.0f32; 1024];
        assert_eq!(compute_rms_energy(&silence), 0.0);
    }

    #[test]
    fn rms_energy_of_empty_is_zero() {
        assert_eq!(compute_rms_energy(&[]), 0.0);
    }

    #[test]
    fn rms_energy_of_constant_signal() {
        // Constant signal at 0.5 → RMS should be 0.5
        let signal = vec![0.5f32; 100];
        let energy = compute_rms_energy(&signal);
        assert!((energy - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rms_energy_above_threshold() {
        let loud = vec![0.8f32; 256];
        let energy = compute_rms_energy(&loud);
        assert!(energy > 0.01, "Loud signal should exceed default threshold");
    }

    #[test]
    fn rms_energy_below_threshold_for_quiet() {
        let quiet = vec![0.001f32; 256];
        let energy = compute_rms_energy(&quiet);
        assert!(
            energy < 0.01,
            "Very quiet signal should be below default threshold"
        );
    }

    #[test]
    fn utterance_capture_waits_for_voice_before_silence_timeout() {
        let silence_timeout = Duration::from_millis(1200);
        let max_capture = Duration::from_secs(8);

        assert!(
            !should_finish_utterance_capture(
                false,
                Duration::from_millis(1500),
                Duration::from_millis(1500),
                silence_timeout,
                max_capture,
            ),
            "silence before the user starts speaking must not end capture"
        );

        assert!(should_finish_utterance_capture(
            true,
            Duration::from_millis(1500),
            Duration::from_secs(3),
            silence_timeout,
            max_capture,
        ));

        assert!(should_finish_utterance_capture(
            false,
            Duration::from_secs(9),
            Duration::from_secs(9),
            silence_timeout,
            max_capture,
        ));
    }

    #[test]
    fn wake_capture_waits_for_voice_after_clap_gate() {
        let silence_timeout = Duration::from_millis(1200);
        let max_capture = Duration::from_secs(8);

        assert!(
            !should_finish_wake_capture(
                false,
                Duration::from_millis(1500),
                Duration::from_millis(1500),
                silence_timeout,
                max_capture,
            ),
            "silence after the second clap should leave time to say the wake name"
        );

        assert!(should_finish_wake_capture(
            true,
            Duration::from_millis(1500),
            Duration::from_millis(2500),
            silence_timeout,
            max_capture,
        ));

        assert!(should_finish_wake_capture(
            false,
            Duration::from_secs(9),
            Duration::from_secs(9),
            silence_timeout,
            max_capture,
        ));
    }

    #[test]
    fn clap_gated_voice_can_activate_without_waiting_for_wake_transcript() {
        assert!(should_activate_clap_gated_voice_without_transcript(
            true, true
        ));
        assert!(!should_activate_clap_gated_voice_without_transcript(
            true, false
        ));
        assert!(!should_activate_clap_gated_voice_without_transcript(
            false, true
        ));
    }

    #[test]
    fn normalize_voice_utterance_repairs_common_price_mishearing() {
        assert_eq!(
            normalize_voice_utterance_text("최적과 소고기를 찾아줘"),
            "최저가 소고기를 찾아줘"
        );
        assert_eq!(
            normalize_voice_utterance_text(" 최적가 상품 찾아줘 "),
            "최저가 상품 찾아줘"
        );
    }

    #[test]
    fn voice_wake_message_sets_alias_and_normalized_content() {
        let msg = build_voice_wake_message("jarvis", 7, " 최적과 소고기를 찾아줘 ", 1_780_000_000)
            .expect("non-empty voice text should dispatch");

        assert_eq!(msg.id, "voice_wake_7");
        assert_eq!(msg.sender, "voice_user");
        assert_eq!(msg.reply_target, "voice_user");
        assert_eq!(msg.content, "최저가 소고기를 찾아줘");
        assert_eq!(msg.channel, "voice_wake");
        assert_eq!(msg.channel_alias.as_deref(), Some("jarvis"));
        assert_eq!(msg.timestamp, 1_780_000_000);

        assert!(build_voice_wake_message("jarvis", 8, "   ", 1).is_none());
    }

    #[test]
    fn jarvis_double_clap_wake_and_price_command_flow_dispatches_task() {
        let config = VoiceWakeConfig {
            wake_word: "jarvis".into(),
            clap_gate_enabled: true,
            clap_count: 2,
            clap_energy_threshold: 0.015,
            clap_window_ms: 1200,
            clap_cooldown_ms: 90,
            silence_timeout_ms: 1200,
            energy_threshold: 0.002,
            max_capture_secs: 8,
            post_wake_capture_delay_ms: 3000,
            ..VoiceWakeConfig::default()
        };
        let start = Instant::now();
        let mut detector = ClapDetector::new(
            config.clap_count,
            config.clap_energy_threshold,
            Duration::from_millis(u64::from(config.clap_window_ms)),
            Duration::from_millis(u64::from(config.clap_cooldown_ms)),
        );

        assert_eq!(
            detector.observe(0.020, start),
            ClapGateEvent::FirstClap,
            "first clap should arm the gesture gate"
        );
        assert_eq!(
            detector.observe(0.023, start + Duration::from_millis(160)),
            ClapGateEvent::Complete,
            "second clap inside the configured window should complete the gate"
        );

        let wake_text = "Jarvis";
        let lower = wake_text.to_lowercase();
        assert!(wake_word_matches(&lower, &config.wake_word));

        let silence_timeout = Duration::from_millis(u64::from(config.silence_timeout_ms));
        let max_capture = Duration::from_secs(u64::from(config.max_capture_secs));
        assert!(
            !should_finish_utterance_capture(
                false,
                Duration::from_millis(1500),
                Duration::from_millis(1500),
                silence_timeout,
                max_capture,
            ),
            "post-wake silence before the user speaks must not dispatch an empty command"
        );
        assert!(should_finish_utterance_capture(
            true,
            Duration::from_millis(1300),
            Duration::from_millis(3600),
            silence_timeout,
            max_capture,
        ));

        let msg = build_voice_wake_message("jarvis", 1, "최적과 소고기를 찾아줘", 1)
            .expect("recognized command should dispatch");
        assert_eq!(msg.channel_alias.as_deref(), Some("jarvis"));
        assert_eq!(msg.content, "최저가 소고기를 찾아줘");
    }

    // ── Clap gate tests ───────────────────────────────────

    #[test]
    fn clap_energy_uses_threshold() {
        assert!(is_clap_energy(0.25, 0.25));
        assert!(!is_clap_energy(0.249, 0.25));
    }

    #[test]
    fn wake_word_matches_jarvis_transliterations() {
        assert!(wake_word_matches("hey jarvis", "jarvis"));
        assert!(wake_word_matches("javis", "jarvis"));
        assert!(wake_word_matches("자비스", "jarvis"));
        assert!(wake_word_matches("차비스", "jarvis"));
        assert!(wake_word_matches("자, 자, 자", "jarvis"));
        assert!(!wake_word_matches("서비스", "jarvis"));
    }

    #[test]
    fn clap_voice_fallback_requires_jarvis_clap_gate_and_voice_text() {
        assert!(clap_voice_fallback_matches(
            "수고하셨습니다 수고하셨습니다",
            "jarvis",
            true
        ));
        assert!(!clap_voice_fallback_matches(
            "수고하셨습니다",
            "hey zeroclaw",
            true
        ));
        assert!(!clap_voice_fallback_matches(
            "수고하셨습니다",
            "jarvis",
            false
        ));
        assert!(!clap_voice_fallback_matches(" , ", "jarvis", true));
    }

    #[test]
    fn clap_detector_completes_after_two_peaks_inside_window() {
        let start = Instant::now();
        let mut detector = ClapDetector::new(
            2,
            0.25,
            Duration::from_millis(900),
            Duration::from_millis(120),
        );

        assert_eq!(detector.observe(0.30, start), ClapGateEvent::FirstClap);
        assert_eq!(
            detector.observe(0.31, start + Duration::from_millis(200)),
            ClapGateEvent::Complete
        );
    }

    #[test]
    fn clap_detector_respects_cooldown() {
        let start = Instant::now();
        let mut detector = ClapDetector::new(
            2,
            0.25,
            Duration::from_millis(900),
            Duration::from_millis(120),
        );

        assert_eq!(detector.observe(0.30, start), ClapGateEvent::FirstClap);
        assert_eq!(
            detector.observe(0.32, start + Duration::from_millis(50)),
            ClapGateEvent::None
        );
        assert_eq!(
            detector.observe(0.32, start + Duration::from_millis(150)),
            ClapGateEvent::Complete
        );
    }

    #[test]
    fn clap_detector_expires_old_partial_gesture() {
        let start = Instant::now();
        let mut detector = ClapDetector::new(
            2,
            0.25,
            Duration::from_millis(900),
            Duration::from_millis(120),
        );

        assert_eq!(detector.observe(0.30, start), ClapGateEvent::FirstClap);
        assert_eq!(
            detector.observe(0.01, start + Duration::from_millis(901)),
            ClapGateEvent::Expired
        );
        assert_eq!(
            detector.observe(0.30, start + Duration::from_millis(1_000)),
            ClapGateEvent::FirstClap
        );
    }

    // ── WAV encoding tests ─────────────────────────────────

    #[test]
    fn wav_header_is_valid() {
        let samples = vec![0.0f32; 100];
        let wav = encode_wav_from_f32(&samples, 16000, 1);

        // RIFF header
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");

        // fmt chunk
        assert_eq!(&wav[12..16], b"fmt ");
        let fmt_size = u32::from_le_bytes(wav[16..20].try_into().unwrap());
        assert_eq!(fmt_size, 16);

        // PCM format
        let format = u16::from_le_bytes(wav[20..22].try_into().unwrap());
        assert_eq!(format, 1);

        // Channels
        let channels = u16::from_le_bytes(wav[22..24].try_into().unwrap());
        assert_eq!(channels, 1);

        // Sample rate
        let sr = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(sr, 16000);

        // data chunk
        assert_eq!(&wav[36..40], b"data");
        let data_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_size, 200); // 100 samples * 2 bytes each
    }

    #[test]
    fn wav_total_size_correct() {
        let samples = vec![0.0f32; 50];
        let wav = encode_wav_from_f32(&samples, 44100, 2);
        // header (44 bytes) + data (50 * 2 = 100 bytes)
        assert_eq!(wav.len(), 144);
    }

    #[test]
    fn wav_encodes_clipped_samples() {
        // Samples outside [-1, 1] should be clamped
        let samples = vec![-2.0f32, 2.0, 0.0];
        let wav = encode_wav_from_f32(&samples, 16000, 1);

        let s0 = i16::from_le_bytes(wav[44..46].try_into().unwrap());
        let s1 = i16::from_le_bytes(wav[46..48].try_into().unwrap());
        let s2 = i16::from_le_bytes(wav[48..50].try_into().unwrap());

        assert_eq!(s0, -32767); // clamped to -1.0
        assert_eq!(s1, 32767); // clamped to 1.0
        assert_eq!(s2, 0);
    }

    // ── Config parsing tests ───────────────────────────────

    #[test]
    fn voice_wake_config_defaults() {
        let config = VoiceWakeConfig::default();
        assert_eq!(config.wake_word, "hey zeroclaw");
        assert_eq!(config.silence_timeout_ms, 2000);
        assert!((config.energy_threshold - 0.01).abs() < f32::EPSILON);
        assert_eq!(config.max_capture_secs, 30);
        assert!(!config.clap_gate_enabled);
        assert_eq!(config.clap_count, 2);
        assert!((config.clap_energy_threshold - 0.25).abs() < f32::EPSILON);
        assert_eq!(config.clap_window_ms, 900);
        assert_eq!(config.clap_cooldown_ms, 120);
        assert_eq!(config.clap_gate_timeout_ms, 5000);
        assert_eq!(config.activation_ack_text, "네 주인님 무엇을 도와드릴까요?");
        assert_eq!(config.post_wake_capture_delay_ms, 1800);
    }

    #[test]
    fn voice_wake_config_deserialize_partial() {
        let toml_str = r#"
            wake_word = "okay agent"
            max_capture_secs = 60
        "#;
        let config: VoiceWakeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.wake_word, "okay agent");
        assert_eq!(config.max_capture_secs, 60);
        // Defaults preserved for unset fields
        assert_eq!(config.silence_timeout_ms, 2000);
        assert!((config.energy_threshold - 0.01).abs() < f32::EPSILON);
        assert_eq!(config.clap_count, 2);
        assert_eq!(config.activation_ack_text, "네 주인님 무엇을 도와드릴까요?");
        assert_eq!(config.post_wake_capture_delay_ms, 1800);
    }

    #[test]
    fn voice_wake_config_deserialize_all_fields() {
        let toml_str = r#"
            wake_word = "hello bot"
            silence_timeout_ms = 3000
            energy_threshold = 0.05
            max_capture_secs = 15
            clap_gate_enabled = true
            clap_count = 2
            clap_energy_threshold = 0.35
            clap_window_ms = 700
            clap_cooldown_ms = 100
            clap_gate_timeout_ms = 4000
            activation_ack_text = "네 주인님 무엇을 도와드릴까요?"
            post_wake_capture_delay_ms = 2500
        "#;
        let config: VoiceWakeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.wake_word, "hello bot");
        assert_eq!(config.silence_timeout_ms, 3000);
        assert!((config.energy_threshold - 0.05).abs() < f32::EPSILON);
        assert_eq!(config.max_capture_secs, 15);
        assert!(config.clap_gate_enabled);
        assert_eq!(config.clap_count, 2);
        assert!((config.clap_energy_threshold - 0.35).abs() < f32::EPSILON);
        assert_eq!(config.clap_window_ms, 700);
        assert_eq!(config.clap_cooldown_ms, 100);
        assert_eq!(config.clap_gate_timeout_ms, 4000);
        assert_eq!(config.activation_ack_text, "네 주인님 무엇을 도와드릴까요?");
        assert_eq!(config.post_wake_capture_delay_ms, 2500);
    }

    #[test]
    fn voice_wake_config_channel_config_trait() {
        assert_eq!(VoiceWakeConfig::name(), "VoiceWake");
        assert_eq!(VoiceWakeConfig::desc(), "voice wake word detection");
    }

    // ── State transition logic tests ───────────────────────

    #[test]
    fn energy_threshold_determines_trigger() {
        let threshold = 0.01f32;
        let quiet_energy = compute_rms_energy(&vec![0.005f32; 256]);
        let loud_energy = compute_rms_energy(&vec![0.5f32; 256]);

        assert!(quiet_energy < threshold, "Quiet should not trigger");
        assert!(loud_energy >= threshold, "Loud should trigger");
    }

    #[test]
    fn state_transitions_are_deterministic() {
        // Verify that the state enum values are distinct and copyable
        let states = [
            WakeState::Listening,
            WakeState::Triggered,
            WakeState::Capturing,
            WakeState::Processing,
        ];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn channel_config_impl() {
        // VoiceWakeConfig implements ChannelConfig
        assert_eq!(VoiceWakeConfig::name(), "VoiceWake");
        assert!(!VoiceWakeConfig::desc().is_empty());
    }

    #[test]
    fn voice_wake_channel_name() {
        let config = VoiceWakeConfig::default();
        let transcription_config = TranscriptionConfig::default();
        let channel = VoiceWakeChannel::new("testbot", config, transcription_config);
        assert_eq!(channel.name(), "voice_wake");
    }

    #[test]
    fn voice_wake_channel_keeps_provider_selection_as_resolver() {
        let config = VoiceWakeConfig::default();
        let transcription_config = TranscriptionConfig::default();
        let channel = VoiceWakeChannel::new_with_transcription_provider_resolver(
            "jarvis",
            config,
            transcription_config,
            Arc::new(|| "openai.jarvis".to_string()),
        );
        assert_eq!((channel.transcription_provider_resolver)(), "openai.jarvis");
    }
}
