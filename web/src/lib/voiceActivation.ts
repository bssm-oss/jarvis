import { getLogs, type LogEvent } from './api';
import type { SSEEvent } from '../types/api';

export const VOICE_ACTIVATION_EVENT = 'zeroclaw-voice-activation';
export const DEFAULT_VOICE_ACK = '네 주인님 무엇을 도와드릴까요?';

export interface VoiceActivationSignal {
  phase: string;
  ackText: string;
  amplitude: number | null;
  createdAt: number;
}

export interface VoiceActivationSignalEnvelope {
  key: string;
  signal: VoiceActivationSignal;
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

function readTimestampMs(value: unknown): number | null {
  if (typeof value !== 'string' || value.length === 0) return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function signalFromAttributes(
  attrs: Record<string, unknown>,
  createdAt: number,
): VoiceActivationSignal | null {
  if (!readString(attrs.voice_activation)) return null;

  return {
    phase: readString(attrs.phase) ?? 'unknown',
    ackText: readString(attrs.ack_text) ?? DEFAULT_VOICE_ACK,
    amplitude: readNumber(attrs.energy),
    createdAt,
  };
}

export function signalFromSseEvent(event: SSEEvent | undefined): VoiceActivationSignal | null {
  if (!event) return null;

  const attrs = readAttributes(event);
  if (!attrs) return null;

  return signalFromAttributes(
    attrs,
    readTimestampMs(event['@timestamp']) ?? readTimestampMs(event.timestamp) ?? Date.now(),
  );
}

export function signalFromLogEvent(event: LogEvent | undefined): VoiceActivationSignal | null {
  if (!event?.attributes) return null;
  return signalFromAttributes(
    event.attributes,
    readTimestampMs(event['@timestamp']) ?? Date.now(),
  );
}

export async function fetchVoiceActivationSignalsSince(
  sinceMs: number,
): Promise<VoiceActivationSignalEnvelope[]> {
  const sinceTs = new Date(Math.max(0, sinceMs - 1000)).toISOString();
  const response = await getLogs({
    q: 'voice_activation',
    since_ts: sinceTs,
    limit: 50,
  });

  return response.events
    .slice()
    .reverse()
    .map((event) => {
      const signal = signalFromLogEvent(event);
      if (!signal) return null;
      return {
        key: `${event.id}:${signal.phase}`,
        signal,
      };
    })
    .filter((item): item is VoiceActivationSignalEnvelope => Boolean(item));
}

export function dispatchVoiceActivationSignal(signal: VoiceActivationSignal) {
  window.dispatchEvent(new CustomEvent(VOICE_ACTIVATION_EVENT, { detail: signal }));
}
