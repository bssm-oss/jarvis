import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import Live2DJarvisAvatar from '../components/Live2DJarvisAvatar';
import { useSSE } from '../hooks/useSSE';
import {
  DEFAULT_VOICE_ACK,
  VOICE_ACTIVATION_EVENT,
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
  if (signal.phase === 'utterance_dispatched') return '처리 중';
  if (signal.phase === 'double_clap_detected' || signal.phase === 'wake_name_audio_started') {
    return '자비스';
  }
  return '대기 중';
}

export default function VoiceActivation() {
  const { events } = useSSE({ filterTypes: ['message'], maxEvents: 40 });
  const [signal, setSignal] = useState<VoiceActivationSignal | null>(() => ({
    phase: 'idle',
    ackText: sessionStorage.getItem('zeroclaw_voice_ack') ?? DEFAULT_VOICE_ACK,
    amplitude: null,
    createdAt: Date.now(),
  }));
  const spokenRef = useRef('');
  const [isJarvisSpeaking, setIsJarvisSpeaking] = useState(false);
  const micLevel = useMicrophoneLevel(true);

  useEffect(() => {
    const latest = signalFromSseEvent(events[events.length - 1]);
    if (latest) setSignal(latest);
  }, [events]);

  useEffect(() => {
    const onActivation = (event: Event) => {
      const custom = event as CustomEvent<VoiceActivationSignal>;
      if (custom.detail) setSignal(custom.detail);
    };
    window.addEventListener(VOICE_ACTIVATION_EVENT, onActivation);
    return () => window.removeEventListener(VOICE_ACTIVATION_EVENT, onActivation);
  }, []);

  useEffect(() => {
    if (!signal || signal.phase !== 'wake_confirmed') return;
    const key = `${signal.createdAt}:${signal.ackText}`;
    if (spokenRef.current === key) return;
    spokenRef.current = key;
    sessionStorage.setItem('zeroclaw_voice_ack', signal.ackText);

    if ('speechSynthesis' in window) {
      window.speechSynthesis.cancel();
      const utterance = new SpeechSynthesisUtterance(signal.ackText);
      utterance.lang = 'ko-KR';
      utterance.rate = 0.96;
      utterance.pitch = 0.86;
      utterance.onstart = () => setIsJarvisSpeaking(true);
      utterance.onend = () => setIsJarvisSpeaking(false);
      utterance.onerror = () => setIsJarvisSpeaking(false);
      window.speechSynthesis.speak(utterance);
    }
  }, [signal]);

  const level = useMemo(() => {
    const eventLevel = signal?.amplitude ? Math.min(1, signal.amplitude * 2.5) : 0;
    const baseline = signal?.phase === 'wake_confirmed' || isJarvisSpeaking ? 0.18 : 0.08;
    return Math.max(baseline, micLevel, eventLevel);
  }, [isJarvisSpeaking, micLevel, signal]);

  const style = {
    '--voice-level': level.toFixed(3),
  } as CSSProperties;

  return (
    <main className="voice-activation-screen" style={style}>
      <div className="voice-activation-frame">
        <div className="voice-activation-kicker">JARVIS</div>
        <Live2DJarvisAvatar
          level={level}
          phase={signal?.phase ?? 'idle'}
          speaking={isJarvisSpeaking}
        />
        <p className="voice-activation-text">{labelForPhase(signal)}</p>
      </div>
    </main>
  );
}
