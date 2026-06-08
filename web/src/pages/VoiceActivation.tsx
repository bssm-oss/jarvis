import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import Live2DJarvisAvatar from '../components/Live2DJarvisAvatar';
import { useSSE } from '../hooks/useSSE';
import { useLocalTts } from '../hooks/useLocalTts';
import {
  DEFAULT_VOICE_ACK,
  VOICE_ACTIVATION_EVENT,
  fetchVoiceActivationSignalsSince,
  loadRecentVoiceActivationSignal,
  saveVoiceActivationSignal,
  signalFromSseEvent,
  type VoiceActivationSignal,
} from '../lib/voiceActivation';

function useMicrophoneLevel(active: boolean): number {
  const [level, setLevel] = useState(0);

  useEffect(() => {
    if (!active || !navigator.mediaDevices?.getUserMedia) {
      setLevel(0);
      return;
    }

    let cancelled = false;
    let frame = 0;
    let stream: MediaStream | null = null;
    let audioContext: AudioContext | null = null;

    const start = async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true,
          },
        });
        if (cancelled) {
          stream.getTracks().forEach((track) => track.stop());
          return;
        }

        const AudioContextCtor =
          window.AudioContext ??
          (window as Window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
        if (!AudioContextCtor) return;

        audioContext = new AudioContextCtor();
        const analyser = audioContext.createAnalyser();
        analyser.fftSize = 1024;
        const source = audioContext.createMediaStreamSource(stream);
        source.connect(analyser);
        const samples = new Float32Array(analyser.fftSize);

        const tick = () => {
          analyser.getFloatTimeDomainData(samples);
          let sum = 0;
          for (const sample of samples) {
            sum += sample * sample;
          }
          const rms = Math.sqrt(sum / samples.length);
          setLevel(Math.min(1, rms * 7));
          frame = requestAnimationFrame(tick);
        };
        tick();
      } catch {
        if (!cancelled) setLevel(0);
      }
    };

    void start();

    return () => {
      cancelled = true;
      if (frame) cancelAnimationFrame(frame);
      stream?.getTracks().forEach((track) => track.stop());
      void audioContext?.close();
    };
  }, [active]);

  return level;
}

function labelForPhase(signal: VoiceActivationSignal | null): string {
  if (!signal) return 'JARVIS';
  if (signal.phase === 'wake_confirmed') return signal.ackText;
  if (signal.phase === 'utterance_transcribing') return '명령을 듣고 있습니다';
  if (signal.phase === 'utterance_dispatched') return '처리 중';
  if (signal.phase === 'double_clap_detected' || signal.phase === 'wake_name_audio_started') {
    return '자비스 기동 중';
  }
  if (signal.phase === 'wake_name_timeout' || signal.phase === 'wake_name_timeout_no_voice') {
    return '박수는 감지됐지만 음성이 없습니다';
  }
  if (signal.phase === 'no_wake_word') {
    return '호명 인식이 맞지 않습니다';
  }
  return '대기 중';
}

function statusForPhase(signal: VoiceActivationSignal | null, speaking: boolean) {
  if (speaking) return { label: '말하는 중', tone: 'speaking' };
  switch (signal?.phase) {
    case 'double_clap_detected':
      return { label: '자비스 확인', tone: 'armed' };
    case 'wake_confirmed':
      return { label: '듣는 중', tone: 'listening' };
    case 'utterance_transcribing':
    case 'utterance_dispatched':
      return { label: '처리 중', tone: 'processing' };
    case 'wake_name_timeout':
    case 'wake_name_timeout_no_voice':
    case 'no_wake_word':
      return { label: '확인 필요', tone: 'error' };
    default:
      return { label: '대기 중', tone: 'idle' };
  }
}

export default function VoiceActivation() {
  const { events } = useSSE({ filterTypes: ['message'], maxEvents: 40 });
  const [signal, setSignal] = useState<VoiceActivationSignal | null>(() => (
    loadRecentVoiceActivationSignal() ?? {
      phase: 'idle',
      ackText: sessionStorage.getItem('zeroclaw_voice_ack') ?? DEFAULT_VOICE_ACK,
      amplitude: null,
      createdAt: Date.now(),
    }
  ));
  const spokenRef = useRef('');
  const handledRef = useRef(new Set<string>());
  const lastPollMsRef = useRef(
    signal?.phase !== 'idle' ? signal?.createdAt ?? Date.now() : Date.now() - 15_000,
  );
  const localTts = useLocalTts(true);
  const micLevel = useMicrophoneLevel(true);

  const applySignal = (next: VoiceActivationSignal, key: string) => {
    if (handledRef.current.has(key)) return;
    handledRef.current.add(key);
    if (handledRef.current.size > 200) {
      handledRef.current = new Set(Array.from(handledRef.current).slice(-100));
    }
    lastPollMsRef.current = Math.max(lastPollMsRef.current, next.createdAt);
    saveVoiceActivationSignal(next);
    setSignal(next);
  };

  useEffect(() => {
    const latest = signalFromSseEvent(events[events.length - 1]);
    if (latest) applySignal(latest, `sse:${latest.createdAt}:${latest.phase}`);
  }, [events]);

  useEffect(() => {
    const onActivation = (event: Event) => {
      const custom = event as CustomEvent<VoiceActivationSignal>;
      if (custom.detail) {
        applySignal(custom.detail, `event:${custom.detail.createdAt}:${custom.detail.phase}`);
      }
    };
    window.addEventListener(VOICE_ACTIVATION_EVENT, onActivation);
    return () => window.removeEventListener(VOICE_ACTIVATION_EVENT, onActivation);
  }, []);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const poll = async () => {
      try {
        const envelopes = await fetchVoiceActivationSignalsSince(lastPollMsRef.current);
        if (!cancelled) {
          for (const envelope of envelopes) {
            applySignal(envelope.signal, `log:${envelope.key}`);
          }
        }
      } catch {
        // The overlay is still usable from live SSE and CustomEvent updates.
      } finally {
        if (!cancelled) {
          timer = setTimeout(() => void poll(), 750);
        }
      }
    };

    void poll();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (!signal || signal.phase !== 'wake_confirmed') return;
    const key = `${signal.createdAt}:${signal.ackText}`;
    if (spokenRef.current === key) return;
    spokenRef.current = key;
    sessionStorage.setItem('zeroclaw_voice_ack', signal.ackText);

    void localTts.speak(signal.ackText);
  }, [localTts.speak, signal]);

  const level = useMemo(() => {
    const eventLevel = signal?.amplitude ? Math.min(1, signal.amplitude * 2.5) : 0;
    const baseline = signal?.phase === 'wake_confirmed' || localTts.speaking ? 0.18 : 0.08;
    return Math.max(baseline, micLevel, eventLevel);
  }, [localTts.speaking, micLevel, signal]);
  const status = statusForPhase(signal, localTts.speaking);

  const style = {
    '--voice-level': level.toFixed(3),
  } as CSSProperties;

  return (
    <main className="voice-activation-screen" style={style} data-voice-tone={status.tone}>
      <div className="voice-activation-frame">
        <div className="voice-activation-kicker">JARVIS</div>
        <div className="voice-activation-state" data-tone={status.tone}>{status.label}</div>
        <Live2DJarvisAvatar
          level={level}
          phase={signal?.phase ?? 'idle'}
          speaking={localTts.speaking}
        />
        <p className="voice-activation-text">{labelForPhase(signal)}</p>
        {localTts.error && (
          <p className="voice-activation-tts-status">음성 오류: {localTts.error}</p>
        )}
      </div>
    </main>
  );
}
