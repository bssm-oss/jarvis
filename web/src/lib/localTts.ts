import { apiFetch } from './api';
import { apiOrigin } from './basePath';
import { getTauriCore, isTauri } from './tauri';

export type LocalTtsRuntimeStatus = 'idle' | 'starting' | 'ready' | 'speaking' | 'error';

export interface LocalTtsStatusResponse {
  status: 'starting' | 'ready';
  endpoint: string;
  cache_dir: string;
  bind_host: string;
  port: number;
}

export interface LocalTtsSegment {
  text: string;
  url: string;
  cached: boolean;
}

export interface LocalTtsSpeakResponse {
  status: 'ready';
  endpoint: string;
  cache_dir: string;
  segments: LocalTtsSegment[];
}

export function getLocalTtsStatus(): Promise<LocalTtsStatusResponse> {
  return apiFetch<LocalTtsStatusResponse>('/api/tts/status');
}

export function speakLocalTts(text: string): Promise<LocalTtsSpeakResponse> {
  return apiFetch<LocalTtsSpeakResponse>('/api/tts/speak', {
    method: 'POST',
    body: JSON.stringify({ text }),
  });
}

export async function playLocalTtsResponse(response: LocalTtsSpeakResponse): Promise<void> {
  for (const segment of response.segments) {
    await playTtsAudio(resolveAudioUrl(segment.url));
  }
}

export function resolveAudioUrl(url: string): string {
  if (/^https?:\/\//i.test(url)) return url;
  return `${apiOrigin}${url}`;
}

function playAudio(url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const audio = new Audio(url);
    let settled = false;

    const finish = () => {
      if (settled) return;
      settled = true;
      resolve();
    };

    const fail = () => {
      if (settled) return;
      settled = true;
      reject(new Error('Local TTS audio playback failed'));
    };

    audio.preload = 'auto';
    audio.onended = finish;
    audio.onerror = fail;

    const playPromise = audio.play();
    if (playPromise) {
      playPromise.catch((error) => {
        if (settled) return;
        settled = true;
        reject(error instanceof Error ? error : new Error(String(error)));
      });
    }
  });
}

async function playTtsAudio(url: string): Promise<void> {
  if (isTauri()) {
    try {
      await playNativeAudio(url);
      return;
    } catch (error) {
      console.warn('[ZeroClaw] Native TTS playback failed; falling back to Web Audio.', error);
    }
  }

  await playAudio(url);
}

async function playNativeAudio(url: string): Promise<void> {
  const core = getTauriCore();
  if (!isTauri() || !core?.invoke) {
    throw new Error('Native audio playback is unavailable outside Tauri');
  }

  await core.invoke('play_local_tts_audio', { url });
}
