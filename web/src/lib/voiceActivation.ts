import type { SSEEvent } from '../types/api';

export const VOICE_ACTIVATION_EVENT = 'zeroclaw-voice-activation';
export const DEFAULT_VOICE_ACK = '네 주인님 무엇을 도와드릴까요?';

export interface VoiceActivationSignal {
  phase: string;
  ackText: string;
  amplitude: number | null;
  createdAt: number;
}

function readAttributes(event: SSEEvent): Record<string, unknown> | null {
  const attrs = event.attributes;
  if (!attrs || typeof attrs !== 'object' || Array.isArray(attrs)) return null;
  return attrs as Record<string, unknown>;
}

function readString(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function readNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function signalFromSseEvent(event: SSEEvent | undefined): VoiceActivationSignal | null {
  if (!event) return null;

  const attrs = readAttributes(event);
  if (!attrs || !readString(attrs.voice_activation)) return null;

  return {
    phase: readString(attrs.phase) ?? 'unknown',
    ackText: readString(attrs.ack_text) ?? DEFAULT_VOICE_ACK,
    amplitude: readNumber(attrs.energy),
    createdAt: Date.now(),
  };
}

export function dispatchVoiceActivationSignal(signal: VoiceActivationSignal) {
  window.dispatchEvent(new CustomEvent(VOICE_ACTIVATION_EVENT, { detail: signal }));
}
