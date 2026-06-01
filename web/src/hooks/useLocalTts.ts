import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getLocalTtsStatus,
  playLocalTtsResponse,
  speakLocalTts,
  type LocalTtsRuntimeStatus,
} from '@/lib/localTts';

export function useLocalTts(autoStart = true) {
  const [status, setStatus] = useState<LocalTtsRuntimeStatus>('idle');
  const [error, setError] = useState<string | null>(null);
  const queueRef = useRef<Promise<void>>(Promise.resolve());
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (!autoStart) return;

    let cancelled = false;
    let timer: number | undefined;

    const poll = async () => {
      if (cancelled) return;
      setStatus((current) => (current === 'speaking' ? current : 'starting'));
      try {
        const response = await getLocalTtsStatus();
        if (cancelled) return;
        setError(null);
        setStatus((current) => (current === 'speaking' ? current : response.status));
        if (response.status !== 'ready') {
          timer = window.setTimeout(poll, 3000);
        }
      } catch (e) {
        if (cancelled) return;
        setError(errorMessage(e));
        setStatus('error');
      }
    };

    void poll();

    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [autoStart]);

  const speak = useCallback((text: string): Promise<void> => {
    const trimmed = text.trim();
    if (!trimmed) return Promise.resolve();

    const run = async () => {
      if (!mountedRef.current) return;
      setError(null);
      setStatus('speaking');
      try {
        const response = await speakLocalTts(trimmed);
        await playLocalTtsResponse(response);
        if (mountedRef.current) setStatus('ready');
      } catch (e) {
        if (!mountedRef.current) return;
        setError(errorMessage(e));
        setStatus('error');
      }
    };

    queueRef.current = queueRef.current.catch(() => undefined).then(run);
    return queueRef.current;
  }, []);

  return {
    status,
    error,
    speaking: status === 'speaking',
    speak,
  };
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
