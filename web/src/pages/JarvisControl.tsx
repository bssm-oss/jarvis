import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Activity,
  AlertTriangle,
  CheckCircle,
  Mic,
  Power,
  RefreshCcw,
  SlidersHorizontal,
  Volume2,
  XCircle,
} from 'lucide-react';
import {
  getHealth,
  getJarvisTuning,
  getLogs,
  putJarvisTuning,
  reloadDaemon,
  type LogEvent,
} from '@/lib/api';
import { getLocalTtsStatus, type LocalTtsStatusResponse } from '@/lib/localTts';
import { getTauriCore, isTauri } from '@/lib/tauri';
import type { ComponentHealth, HealthSnapshot } from '@/types/api';

type PermissionInfo = {
  name: string;
  label: string;
  status: string;
};

type LaunchAgentInfo = {
  label: string;
  pid: number | null;
  last_exit_status: number | null;
  running: boolean;
};

type TuningState = {
  energyThreshold: number;
  clapEnergyThreshold: number;
  clapWindowMs: number;
  clapCooldownMs: number;
};

const TUNING_DEFAULTS: TuningState = {
  energyThreshold: 0.002,
  clapEnergyThreshold: 0.015,
  clapWindowMs: 1200,
  clapCooldownMs: 90,
};

function textValue(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function numberValue(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function phaseLabel(phase: string): string {
  switch (phase) {
    case 'energy_probe':
      return '임계값 근처 소리';
    case 'first_clap':
      return '첫 번째 박수';
    case 'double_clap_detected':
      return '박수 두 번';
    case 'wake_confirmed':
      return 'Jarvis 활성화';
    case 'wake_name_audio_started':
      return '음성 감지';
    case 'utterance_transcribing':
      return '명령 인식 중';
    case 'utterance_dispatched':
      return 'ZeroClaw 전달';
    case 'wake_name_timeout':
    case 'wake_name_timeout_no_voice':
      return '음성 없음';
    case 'no_wake_word':
      return '호명 불일치';
    default:
      return phase || 'unknown';
  }
}

function failureReason(events: LogEvent[], health: HealthSnapshot | null, tts: LocalTtsStatusResponse | null, permissions: PermissionInfo[]) {
  const latest = events[0];
  const phase = textValue(latest?.attributes?.phase);
  const microphone = permissions.find((item) => item.name === 'microphone');
  const voiceWake = health?.components['channel:voice_wake.jarvis'];

  if (microphone && microphone.status !== 'granted' && microphone.status !== 'authorized') {
    return '마이크 권한이 아직 허용되지 않았습니다.';
  }
  if (voiceWake?.status && voiceWake.status !== 'ok') {
    return voiceWake.last_error || 'voice_wake.jarvis 채널이 정상 상태가 아닙니다.';
  }
  if (!tts || tts.status !== 'ready') {
    return 'TTS 서버가 아직 준비되지 않았습니다.';
  }
  if (phase === 'wake_name_timeout' || phase === 'wake_name_timeout_no_voice') {
    return '박수는 감지됐지만 뒤이어 들어온 음성이 없습니다.';
  }
  if (phase === 'no_wake_word') {
    return '음성은 감지됐지만 Jarvis 호출로 인식되지 않았습니다.';
  }
  return '현재 치명적인 실패 원인은 보이지 않습니다.';
}

function statusTone(status?: string | null): 'ok' | 'warn' | 'error' {
  if (!status) return 'warn';
  if (status.endsWith('%')) {
    return Number(status.replace('%', '')) > 0 ? 'ok' : 'error';
  }
  if (status === 'ok' || status === 'ready' || status === 'granted' || status === 'authorized') {
    return 'ok';
  }
  if (status === 'error' || status === 'denied' || status === 'notDetermined') return 'error';
  return 'warn';
}

function HealthPill({ label, status }: { label: string; status?: string | null }) {
  const tone = statusTone(status);
  const Icon = tone === 'ok' ? CheckCircle : tone === 'error' ? XCircle : AlertTriangle;
  return (
    <div className={`jarvis-pill is-${tone}`}>
      <Icon className="h-3.5 w-3.5" />
      <span>{label}</span>
      <strong>{status ?? 'unknown'}</strong>
    </div>
  );
}

function ComponentPill({ label, component }: { label: string; component?: ComponentHealth }) {
  return <HealthPill label={label} status={component?.status} />;
}

export default function JarvisControl() {
  const [tuning, setTuning] = useState<TuningState>(TUNING_DEFAULTS);
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  const [health, setHealth] = useState<HealthSnapshot | null>(null);
  const [tts, setTts] = useState<LocalTtsStatusResponse | null>(null);
  const [logs, setLogs] = useState<LogEvent[]>([]);
  const [permissions, setPermissions] = useState<PermissionInfo[]>([]);
  const [launchAgents, setLaunchAgents] = useState<LaunchAgentInfo[]>([]);
  const [outputVolume, setOutputVolume] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [tuningNext, healthNext, ttsNext, logsNext] =
        await Promise.all([
          getJarvisTuning(),
          getHealth(),
          getLocalTtsStatus().catch(() => null),
          getLogs({ q: 'voice_activation', limit: 24 }),
        ]);

      setTuning({
        energyThreshold: tuningNext.energy_threshold,
        clapEnergyThreshold: tuningNext.clap_energy_threshold,
        clapWindowMs: tuningNext.clap_window_ms,
        clapCooldownMs: tuningNext.clap_cooldown_ms,
      });
      setHealth(healthNext);
      setTts(ttsNext);
      setLogs(logsNext.events);

      const core = getTauriCore();
      if (isTauri() && core?.invoke) {
        const [permissionNext, volumeNext, launchNext] = await Promise.all([
          core.invoke<PermissionInfo[]>('get_permissions_status').catch(() => []),
          core.invoke<number | null>('get_output_volume').catch(() => null),
          core.invoke<LaunchAgentInfo[]>('get_launch_agent_statuses').catch(() => []),
        ]);
        setPermissions(permissionNext);
        setOutputVolume(volumeNext);
        setLaunchAgents(launchNext);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 4000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const saveTuning = async () => {
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      await putJarvisTuning({
        energy_threshold: tuning.energyThreshold,
        clap_energy_threshold: tuning.clapEnergyThreshold,
        clap_window_ms: Math.round(tuning.clapWindowMs),
        clap_cooldown_ms: Math.round(tuning.clapCooldownMs),
      });
      await reloadDaemon();
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2500);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const diagnosis = useMemo(
    () => failureReason(logs, health, tts, permissions),
    [health, logs, permissions, tts],
  );

  const latestPhase = textValue(logs[0]?.attributes?.phase);
  const components = health?.components ?? {};

  return (
    <div className="jarvis-control-page">
      <header className="jarvis-control-header">
        <div>
          <p className="jarvis-eyebrow">JARVIS CONTROL</p>
          <h1>박수 감지와 실행 상태</h1>
          <p>박수 두 번으로 바로 열리는 흐름을 기준으로 감도, 권한, TTS, 최근 로그를 한 화면에서 확인합니다.</p>
        </div>
        <button className="jarvis-ghost-button" onClick={() => void refresh()}>
          <RefreshCcw className="h-4 w-4" />
          새로고침
        </button>
      </header>

      {error && <div className="jarvis-alert is-error">{error}</div>}

      <section className="jarvis-status-grid">
        <div className="jarvis-panel jarvis-panel-wide">
          <div className="jarvis-panel-title">
            <Activity className="h-4 w-4" />
            실행 상태
          </div>
          <div className="jarvis-pill-grid">
            <ComponentPill label="Gateway" component={components.gateway} />
            <ComponentPill label="Voice Wake" component={components['channel:voice_wake.jarvis']} />
            <ComponentPill label="TTS" component={components['local_tts.yuni']} />
            <HealthPill label="GPT-SoVITS" status={tts?.status} />
          </div>
          <div className="jarvis-diagnosis">
            <span>실패 원인</span>
            <strong>{diagnosis}</strong>
          </div>
        </div>

        <div className="jarvis-panel">
          <div className="jarvis-panel-title">
            <Mic className="h-4 w-4" />
            첫 실행 점검
          </div>
          <div className="jarvis-check-list">
            <HealthPill
              label="마이크"
              status={permissions.find((item) => item.name === 'microphone')?.status ?? (isTauri() ? 'unknown' : 'browser')}
            />
            <HealthPill label="출력 볼륨" status={outputVolume && outputVolume > 0 ? `${outputVolume}%` : '0%'} />
            {launchAgents.map((agent) => (
              <HealthPill
                key={agent.label}
                label={agent.label.replace('ai.zeroclaw.', '')}
                status={agent.running ? 'ok' : `exit ${agent.last_exit_status ?? '-'}`}
              />
            ))}
          </div>
        </div>
      </section>

      <section className="jarvis-main-grid">
        <div className="jarvis-panel">
          <div className="jarvis-panel-title">
            <SlidersHorizontal className="h-4 w-4" />
            박수 감지 튜닝
          </div>
          <TuningSlider
            label="마이크 감도"
            value={tuning.energyThreshold}
            min={0.0005}
            max={0.01}
            step={0.0005}
            format={(value) => value.toFixed(4)}
            onChange={(value) => setTuning((current) => ({ ...current, energyThreshold: value }))}
          />
          <TuningSlider
            label="박수 임계값"
            value={tuning.clapEnergyThreshold}
            min={0.006}
            max={0.08}
            step={0.001}
            format={(value) => value.toFixed(3)}
            onChange={(value) => setTuning((current) => ({ ...current, clapEnergyThreshold: value }))}
          />
          <TuningSlider
            label="두 박수 허용 시간"
            value={tuning.clapWindowMs}
            min={500}
            max={2200}
            step={50}
            format={(value) => `${Math.round(value)}ms`}
            onChange={(value) => setTuning((current) => ({ ...current, clapWindowMs: value }))}
          />
          <TuningSlider
            label="박수 쿨다운"
            value={tuning.clapCooldownMs}
            min={40}
            max={300}
            step={10}
            format={(value) => `${Math.round(value)}ms`}
            onChange={(value) => setTuning((current) => ({ ...current, clapCooldownMs: value }))}
          />
          <div className="jarvis-actions">
            <button className="jarvis-primary-button" onClick={() => void saveTuning()} disabled={saving}>
              <Power className="h-4 w-4" />
              {saving ? '저장 중' : '저장 후 재시작'}
            </button>
            {saved && <span className="jarvis-save-ok">적용됨</span>}
          </div>
        </div>

        <div className="jarvis-panel">
          <div className="jarvis-panel-title">
            <Volume2 className="h-4 w-4" />
            최근 감지 로그
          </div>
          <div className="jarvis-latest">
            <span>최근 상태</span>
            <strong>{phaseLabel(latestPhase)}</strong>
          </div>
          <div className="jarvis-log-list">
            {logs.slice(0, 12).map((event) => {
              const phase = textValue(event.attributes?.phase);
              const energy = numberValue(event.attributes?.energy, 0);
              return (
                <div className="jarvis-log-row" key={`${event.id}:${phase}`}>
                  <span>{new Date(event['@timestamp']).toLocaleTimeString()}</span>
                  <strong>{phaseLabel(phase)}</strong>
                  <code>{energy ? energy.toFixed(4) : '-'}</code>
                </div>
              );
            })}
            {logs.length === 0 && <p className="jarvis-empty">아직 감지 로그가 없습니다.</p>}
          </div>
        </div>
      </section>
    </div>
  );
}

function TuningSlider({
  label,
  value,
  min,
  max,
  step,
  format,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  format: (value: number) => string;
  onChange: (value: number) => void;
}) {
  return (
    <label className="jarvis-slider">
      <span>
        <strong>{label}</strong>
        <code>{format(value)}</code>
      </span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => onChange(Number(event.currentTarget.value))}
      />
    </label>
  );
}
