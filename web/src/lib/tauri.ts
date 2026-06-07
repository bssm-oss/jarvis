// Tauri detection utilities for ZeroClaw Desktop.

declare global {
  interface Window {
    __TAURI__?: { core?: TauriCore };
    __ZEROCLAW_GATEWAY__?: string;
  }
}

export type TauriCore = {
  invoke?: <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** Returns true when running inside a Tauri WebView. */
export const isTauri = (): boolean => '__TAURI__' in window;

/** Tauri core bridge, when the React app is running inside the desktop WebView. */
export const getTauriCore = (): TauriCore | null => window.__TAURI__?.core ?? null;

/** Gateway base URL when running inside Tauri (defaults to localhost). */
export const tauriGatewayUrl = (): string =>
  window.__ZEROCLAW_GATEWAY__ ?? 'http://127.0.0.1:42617';
