import { useEffect, useRef, useState } from 'react';
import * as PIXI from 'pixi.js';
import type { Live2DModel } from 'pixi-live2d-display/cubism4';

declare global {
  interface Window {
    PIXI?: typeof PIXI;
    Live2DCubismCore?: unknown;
  }
}

const ASSET_BASE = import.meta.env.BASE_URL;
const LIVE2D_RUNTIME_SRC = `${ASSET_BASE}vendor/live2dcubismcore.min.js`;
const MAO_MODEL_SRC = `${ASSET_BASE}live2d/mao/mao_pro.model3.json`;

type Live2DModelInstance = Live2DModel<any>;
type Live2DCoreModel = {
  getCanvasWidth?: () => number;
  getCanvasHeight?: () => number;
  getModel?: () => {
    canvasinfo?: {
      CanvasWidth?: number;
      CanvasHeight?: number;
    };
  };
  setParamFloat?: (id: string | number, value: number, weight?: number) => unknown;
  setParameterValueById?: (id: string, value: number, weight?: number) => unknown;
};
type Live2DInternalModel = Live2DModelInstance['internalModel'] & {
  originalWidth?: number;
  originalHeight?: number;
  coreModel: Live2DCoreModel;
};

interface Live2DJarvisAvatarProps {
  level: number;
  phase: string;
  speaking: boolean;
}

let runtimePromise: Promise<void> | null = null;

function loadLive2DRuntime(): Promise<void> {
  if (window.Live2DCubismCore) return Promise.resolve();
  if (runtimePromise) return runtimePromise;

  runtimePromise = new Promise((resolve, reject) => {
    const existing = document.querySelector<HTMLScriptElement>(
      `script[data-live2d-runtime="cubism4"]`,
    );
    if (existing) {
      existing.addEventListener('load', () => resolve(), { once: true });
      existing.addEventListener('error', () => reject(new Error('Live2D runtime failed')), {
        once: true,
      });
      return;
    }

    const script = document.createElement('script');
    script.src = LIVE2D_RUNTIME_SRC;
    script.async = true;
    script.dataset.live2dRuntime = 'cubism4';
    script.addEventListener('load', () => resolve(), { once: true });
    script.addEventListener('error', () => reject(new Error('Live2D runtime failed')), {
      once: true,
    });
    document.head.appendChild(script);
  });

  return runtimePromise;
}

function positiveNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

function setModelParameter(
  coreModel: Live2DCoreModel,
  ids: string | string[],
  value: number,
  weight = 1,
) {
  const parameterIds = Array.isArray(ids) ? ids : [ids];

  try {
    for (const id of parameterIds) {
      if (coreModel.setParameterValueById) {
        coreModel.setParameterValueById(id, value, weight);
      } else {
        coreModel.setParamFloat?.(id, value, weight);
      }
    }
  } catch {
    // Some models omit optional parameters. Animation should degrade silently.
  }
}

function getModelSourceSize(model: Live2DModelInstance) {
  const internalModel = model.internalModel as Live2DInternalModel;
  const canvasInfo = internalModel.coreModel.getModel?.().canvasinfo;
  const sourceWidth =
    [internalModel.originalWidth, canvasInfo?.CanvasWidth, internalModel.coreModel.getCanvasWidth?.()]
      .find(positiveNumber) ?? 1800;
  const sourceHeight =
    [internalModel.originalHeight, canvasInfo?.CanvasHeight, internalModel.coreModel.getCanvasHeight?.()]
      .find(positiveNumber) ?? 2600;

  return { sourceWidth, sourceHeight };
}

function fitModelToContainer(
  model: Live2DModelInstance,
  container: HTMLDivElement,
) {
  const bounds = container.getBoundingClientRect();
  if (!bounds.width || !bounds.height) return;

  const { sourceWidth, sourceHeight } = getModelSourceSize(model);
  const scale = Math.min(bounds.width / sourceWidth, bounds.height / sourceHeight) * 1.02;

  model.scale.set(scale);
  model.anchor.set(0.5, 0.57);
  model.x = bounds.width * 0.5;
  model.y = bounds.height * 0.54;
}

function fallbackLabel(phase: string) {
  if (phase === 'wake_confirmed') return 'ONLINE';
  if (phase === 'utterance_dispatched' || phase === 'utterance_transcribing') return 'SYNC';
  if (phase === 'double_clap_detected' || phase === 'wake_name_audio_started') return 'WAKE';
  return 'IDLE';
}

export default function Live2DJarvisAvatar({ level, phase, speaking }: Live2DJarvisAvatarProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const modelRef = useRef<Live2DModelInstance | null>(null);
  const levelRef = useRef(level);
  const speakingRef = useRef(speaking);
  const phaseRef = useRef(phase);
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    levelRef.current = level;
    speakingRef.current = speaking;
    phaseRef.current = phase;
  }, [level, speaking, phase]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    let cancelled = false;
    let frame = 0;
    let app: PIXI.Application | null = null;
    let resizeObserver: ResizeObserver | null = null;

    const start = async () => {
      try {
        window.PIXI = PIXI;
        await loadLive2DRuntime();
        if (cancelled) return;

        const { Live2DModel, MotionPriority } = await import('pixi-live2d-display/cubism4');
        Live2DModel.registerTicker(PIXI.Ticker);

        app = new PIXI.Application({
          autoDensity: true,
          antialias: true,
          backgroundAlpha: 0,
          resolution: Math.min(window.devicePixelRatio || 1, 2),
          resizeTo: host,
        });
        app.view.className = 'live2d-avatar-canvas';
        host.appendChild(app.view);

        const model = (await Live2DModel.from(MAO_MODEL_SRC, {
          autoInteract: false,
        })) as Live2DModelInstance;
        if (cancelled || !app) {
          model.destroy();
          return;
        }

        modelRef.current = model;
        model.interactive = false;
        model.motion('Idle', undefined, MotionPriority.IDLE).catch(() => undefined);
        model.expression('exp_04').catch(() => undefined);
        app.stage.addChild(model);

        fitModelToContainer(model, host);
        resizeObserver = new ResizeObserver(() => fitModelToContainer(model, host));
        resizeObserver.observe(host);
        setReady(true);

        const animate = () => {
          const activeLevel = Math.max(0, Math.min(1, levelRef.current));
          const now = performance.now();
          const isSpeaking =
            speakingRef.current ||
            phaseRef.current === 'wake_confirmed' ||
            phaseRef.current === 'utterance_transcribing';
          const pulse = isSpeaking
            ? 0.32 + Math.sin(now / 92) * 0.22 + Math.sin(now / 41) * 0.1
            : 0;
          const mouth = Math.max(activeLevel * 1.15, pulse);
          const drift = Math.sin(now / 1300);
          const quick = Math.sin(now / 420);

          const coreModel = (model.internalModel as Live2DInternalModel).coreModel;
          const vowelShift = Math.sin(now / 118);
          setModelParameter(coreModel, ['ParamA', 'PARAM_MOUTH_OPEN_Y'], Math.min(1, mouth), 0.9);
          setModelParameter(coreModel, 'ParamI', isSpeaking ? Math.max(0, vowelShift) * 0.26 : 0, 0.45);
          setModelParameter(coreModel, 'ParamU', isSpeaking ? Math.max(0, -vowelShift) * 0.18 : 0, 0.42);
          setModelParameter(coreModel, 'ParamE', isSpeaking ? Math.max(0, quick) * 0.16 : 0, 0.38);
          setModelParameter(coreModel, 'ParamO', isSpeaking ? activeLevel * 0.24 : 0, 0.38);
          setModelParameter(coreModel, ['ParamMouthUp', 'PARAM_MOUTH_FORM'], isSpeaking ? 0.42 : 0.08, 0.42);
          setModelParameter(coreModel, ['ParamAngleX', 'PARAM_ANGLE_X'], drift * 8 + activeLevel * 8, 0.28);
          setModelParameter(coreModel, ['ParamAngleY', 'PARAM_ANGLE_Y'], quick * 4 - activeLevel * 5, 0.22);
          setModelParameter(coreModel, ['ParamAngleZ', 'PARAM_ANGLE_Z'], drift * 3, 0.18);
          setModelParameter(coreModel, ['ParamBodyAngleX', 'PARAM_BODY_ANGLE_X'], quick * 4 + activeLevel * 4, 0.2);
          setModelParameter(coreModel, ['ParamEyeBallX', 'PARAM_EYE_BALL_X'], drift * 0.38, 0.22);
          setModelParameter(coreModel, ['ParamEyeBallY', 'PARAM_EYE_BALL_Y'], quick * 0.16, 0.2);
          setModelParameter(coreModel, ['ParamBreath', 'PARAM_BREATH'], 0.5 + activeLevel * 0.45, 0.25);
          setModelParameter(coreModel, 'ParamEyeEffect', isSpeaking ? 1 : 0.28, 0.16);
          setModelParameter(coreModel, 'ParamWandInk', isSpeaking ? 1 : activeLevel * 0.4, 0.18);

          frame = requestAnimationFrame(animate);
        };

        frame = requestAnimationFrame(animate);
      } catch {
        if (!cancelled) setFailed(true);
      }
    };

    void start();

    return () => {
      cancelled = true;
      if (frame) cancelAnimationFrame(frame);
      resizeObserver?.disconnect();
      modelRef.current?.destroy();
      app?.destroy(true, { children: true, texture: true, baseTexture: true });
      modelRef.current = null;
      setReady(false);
    };
  }, []);

  useEffect(() => {
    if (phase !== 'wake_confirmed') return;

    const model = modelRef.current;
    if (!model) return;

    void import('pixi-live2d-display/cubism4').then(({ MotionPriority }) => {
      void model.motion('', 3, MotionPriority.FORCE).catch(() => undefined);
      void model.expression('exp_04').catch(() => undefined);
    });
  }, [phase]);

  return (
    <div
      ref={hostRef}
      className={`live2d-avatar-host${ready ? ' is-ready' : ''}${failed ? ' is-fallback' : ''}`}
      aria-label="Jarvis Live2D avatar"
    >
      <div className="live2d-avatar-grid" aria-hidden="true" />
      {(!ready || failed) && (
        <div className="live2d-avatar-fallback" aria-hidden="true">
          <div className="live2d-fallback-head">
            <span className="live2d-fallback-eye live2d-fallback-eye-left" />
            <span className="live2d-fallback-eye live2d-fallback-eye-right" />
            <span className="live2d-fallback-mouth" />
          </div>
          <div className="live2d-fallback-status">{failed ? 'MODEL ERR' : fallbackLabel(phase)}</div>
        </div>
      )}
    </div>
  );
}
