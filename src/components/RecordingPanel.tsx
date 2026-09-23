import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { VideoPlayer } from './VideoPlayer';
import { Dependencies } from './Dependencies';
import { VideoEditor } from './VideoEditor';
import type { MonitorInfo, RecordingInfo } from '../hooks/useApp';
import {
  useCapture,
  useRecorder,
  type AppSettings,
  type RecordingBackdrop,
  type RecordingResolution,
  type RegionRect,
  type WebcamLayout,
  type WebcamPresentation,
  type WebcamShape,
  type WindowInfo,
} from '../hooks/useApp';
import './RecordingPanel.css';

interface RecordingPanelProps {
  settings: AppSettings | null;
  onClose: () => void;
}

const LAYOUTS: { id: WebcamLayout; label: string }[] = [
  { id: 'pip-tl', label: 'Coin haut gauche' },
  { id: 'pip-tr', label: 'Coin haut droit' },
  { id: 'pip-bl', label: 'Coin bas gauche' },
  { id: 'pip-br', label: 'Coin bas droit' },
  { id: 'side-right', label: 'Côte à côte (droite)' },
  { id: 'side-left', label: 'Côte à côte (gauche)' },
  { id: 'full', label: 'Caméra plein cadre' },
];

const SHAPES: { id: WebcamShape; label: string }[] = [
  { id: 'square', label: 'Carré' },
  { id: 'rounded', label: 'Arrondi' },
  { id: 'circle', label: 'Cercle' },
];

const PRESENTATIONS: { id: WebcamPresentation; label: string; hint: string }[] = [
  { id: 'minimal', label: 'Minimal', hint: 'aucune décoration' },
  { id: 'classic', label: 'Classique', hint: 'cadre + ombre' },
  { id: 'studio', label: 'Studio', hint: 'halo lumineux' },
  { id: 'bubble', label: 'Bulle', hint: 'cercle + halo (style Tella)' },
];

const FPS_CHOICES = [15, 24, 30, 60];

const QUALITIES = [
  { id: 'high', label: 'Haute', hint: 'fichier plus gros' },
  { id: 'balanced', label: 'Équilibrée', hint: 'recommandé' },
  { id: 'small', label: 'Compacte', hint: 'fichier plus léger' },
] as const;

const RESOLUTIONS: { id: RecordingResolution; label: string; hint: string }[] = [
  { id: '480', label: '480p', hint: 'très léger' },
  { id: '720', label: '720p', hint: 'recommandé' },
  { id: '1080', label: '1080p', hint: 'demande une machine confortable' },
  { id: 'native', label: 'Native', hint: "résolution réelle de l'écran" },
];

const BACKDROPS: { id: RecordingBackdrop; label: string }[] = [
  { id: 'none', label: 'Aucun' },
  { id: 'aurora', label: 'Aurora' },
  { id: 'sunset', label: 'Sunset' },
  { id: 'mint', label: 'Mint' },
  { id: 'slate', label: 'Slate' },
  { id: 'cream', label: 'Cream' },
];

function formatDuration(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return [h, m, s].map((v) => String(v).padStart(2, '0')).join(':');
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} o`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} Ko`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} Mo`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} Go`;
}

/** Mini-diagramme d'une disposition de webcam */
function LayoutPreview({ layout }: { layout: WebcamLayout }) {
  if (layout === 'full') {
    return (
      <svg viewBox="0 0 48 30" className="layout-svg">
        <rect x="1" y="1" width="46" height="28" rx="3" className="layout-cam" />
      </svg>
    );
  }

  if (layout === 'side-left' || layout === 'side-right') {
    const camX = layout === 'side-right' ? 32 : 2;
    const screenX = layout === 'side-right' ? 2 : 14;
    return (
      <svg viewBox="0 0 48 30" className="layout-svg">
        <rect x={screenX} y="3" width="14" height="24" rx="2" className="layout-screen" />
        <rect x={camX} y="3" width="14" height="24" rx="2" className="layout-cam" />
      </svg>
    );
  }

  const camPos: Record<string, { x: number; y: number }> = {
    'pip-tl': { x: 3, y: 3 },
    'pip-tr': { x: 33, y: 3 },
    'pip-bl': { x: 3, y: 19 },
    'pip-br': { x: 33, y: 19 },
  };
  const pos = camPos[layout] ?? camPos['pip-br'];

  return (
    <svg viewBox="0 0 48 30" className="layout-svg">
      <rect x="1" y="1" width="46" height="28" rx="3" className="layout-screen" />
      <rect x={pos.x} y={pos.y} width="12" height="8" rx="2" className="layout-cam" />
    </svg>
  );
}

export function RecordingPanel({ settings, onClose }: RecordingPanelProps) {
  const { listWindows, listMonitors } = useCapture();

  /** Aperçu composite : la webcam incrustée telle qu'elle apparaîtra dans la vidéo */
  const {
    capabilities,
    checkingCapabilities,
    isRecording,
    isStarting,
    elapsedMs,
    recordings,
    error,
    hasWebcam,
    stats,
    live,
    applyLive,
    encoder,
    refreshCapabilities,
    loadRecordings,
    start,
    stop,
    remove,
    previewFrame,
    compositePreviewFrame,
  } = useRecorder();

  // Source
  const [source, setSource] = useState<'fullscreen' | 'region' | 'window'>('fullscreen');
  const [windows, setWindows] = useState<WindowInfo[]>([]);
  const [windowId, setWindowId] = useState<number | null>(null);
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);
  const [monitorIndex, setMonitorIndex] = useState<number | null>(null);
  const [region, setRegion] = useState<RegionRect | null>(null);

  // Réglages vidéo. Les valeurs par défaut (720p, 24 i/s) sont tenables même
  // sur une machine modeste : mieux vaut une vidéo fluide qu'une vidéo saccadée.
  const [fps, setFps] = useState(24);
  const [quality, setQuality] = useState<'high' | 'balanced' | 'small'>('balanced');
  const [resolution, setResolution] = useState<RecordingResolution>('720');
  const [backdrop, setBackdrop] = useState<RecordingBackdrop>('none');
  const [audio, setAudio] = useState(false);
  const [audioDevice, setAudioDevice] = useState('');
  const [systemAudio, setSystemAudio] = useState(false);
  const [systemAudioDevice, setSystemAudioDevice] = useState('');
  const [hideApp, setHideApp] = useState(true);
  const [timerInVideo, setTimerInVideo] = useState(false);
  const [captureCursor, setCaptureCursor] = useState(true);
  const [countdownSeconds, setCountdownSeconds] = useState(3);

  // Décompte affiché avant le début réel de la capture.
  // `'starting'` couvre le laps entre la fin du décompte et le premier
  // instant filmé : l'utilisateur ne doit jamais retomber sur l'application.
  const [countdown, setCountdown] = useState<number | 'starting' | null>(null);

  // Webcam
  const [webcamEnabled, setWebcamEnabled] = useState(false);
  const [cameraDevice, setCameraDevice] = useState('');
  const [layout, setLayout] = useState<WebcamLayout>('pip-br');
  const [shape, setShape] = useState<WebcamShape>('rounded');
  const [sizePercent, setSizePercent] = useState(28);
  const [margin, setMargin] = useState(24);
  const [presentation, setPresentation] = useState<WebcamPresentation>('minimal');
  const handleCompositePreview = useCallback(async () => {
    if (!webcamEnabled || !cameraDevice) return;
    const url = await compositePreviewFrame({
      device: cameraDevice,
      layout,
      shape,
      size_percent: sizePercent,
      margin,
      presentation,
      source,
      resolution,
      window_id: source === 'window' ? windowId : null,
      region: source === 'region' ? region : null,
    });
    if (url) {
      setPreviewFrameUrl(url);
      setPreviewError(null);
    } else {
      setPreviewError('Aperçu composite indisponible (vérifiez caméra et source).');
    }
  }, [
    webcamEnabled,
    cameraDevice,
    compositePreviewFrame,
    layout,
    shape,
    sizePercent,
    margin,
    presentation,
    source,
    resolution,
    windowId,
    region,
  ]);


  const [previewFrameUrl, setPreviewFrameUrl] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  // Aperçu de la source
  const [sourcePreview, setSourcePreview] = useState<string | null>(null);

  // Enregistrement ouvert dans le lecteur intégré
  const [playing, setPlaying] = useState<{ path: string; filename: string } | null>(null);

  // Vignettes des enregistrements, extraites à la demande
  const [posters, setPosters] = useState<Record<string, string>>({});

  // Dernier enregistrement terminé, présenté à l'utilisateur
  const [lastRecording, setLastRecording] = useState<RecordingInfo | null>(null);

  // Enregistrement ouvert dans l'éditeur
  const [editing, setEditing] = useState<{ path: string; filename: string } | null>(null);

  // Panneau d'installation des binaires externes
  const [showDependencies, setShowDependencies] = useState(false);

  // Rafraîchir la liste des fenêtres
  const refreshWindows = useCallback(() => {
    listWindows().then(setWindows);
  }, [listWindows]);

  // Aperçu périodique de ce qui sera enregistré
  useEffect(() => {
    if (isRecording) return;
    if (source === 'window' && windowId === null) return;
    if (source === 'region' && region === null) return;

    let cancelled = false;

    const refresh = () => {
      invoke<string>('preview_recording_source', {
        source,
        windowId: source === 'window' ? windowId : null,
        region: source === 'region' ? region : null,
      })
        .then((url) => {
          if (!cancelled) setSourcePreview(url);
        })
        .catch(() => {
          if (!cancelled) setSourcePreview(null);
        });
    };

    refresh();
    const timer = window.setInterval(refresh, 3000);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [source, windowId, region, isRecording]);

  useEffect(() => {
    refreshWindows();
  }, [refreshWindows]);

  // Écrans disponibles : le choix n'a de sens qu'à partir de deux
  useEffect(() => {
    listMonitors().then((found) => {
      setMonitors(found);
      setMonitorIndex((current) => {
        if (current !== null) return current;
        const primary = found.find((monitor) => monitor.is_primary);
        return primary ? primary.index : found.length ? found[0].index : null;
      });
    });
  }, [listMonitors]);

  // Extraction des vignettes, une par une pour ne pas saturer le processeur
  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      for (const item of recordings) {
        if (cancelled) return;
        if (posters[item.id]) continue;

        try {
          const url = await invoke<string>('recording_thumbnail', { path: item.path });
          if (cancelled) return;
          setPosters((prev) => (prev[item.id] ? prev : { ...prev, [item.id]: url }));
        } catch {
          // Fichier déplacé ou illisible : l'icône générique reste affichée
        }
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
    // `posters` est volontairement absent : il est lu via la forme fonctionnelle
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recordings]);

  // Sélection d'une zone d'enregistrement
  const handlePickRegion = useCallback(async () => {
    try {
      await invoke('start_region_selection', { forRecording: true });
    } catch (e) {
      console.error('Erreur sélection de zone:', e);
    }
  }, []);

  useEffect(() => {
    const pending = listen<RegionRect>('region-selected', (event) => {
      setRegion(event.payload);
      setSource('region');
    });

    return () => {
      pending.then((off) => off());
    };
  }, []);

  // Aperçu de la caméra. La source reste ouverte tant que l'aperçu est
  // visible : on ne fait que relire la dernière image produite, ce qui permet
  // un rafraîchissement fluide sans rouvrir le périphérique.
  // (getUserMedia n'est pas fiable sur WebKitGTK, d'où le passage par ffmpeg.)
  useEffect(() => {
    if (!webcamEnabled || !cameraDevice) {
      setPreviewFrameUrl(null);
      setPreviewError(null);
      return;
    }

    // Pendant l'enregistrement, la source est partagée avec l'encodeur : on
    // garde la dernière image affichée plutôt que de solliciter la caméra.
    if (isRecording) {
      setPreviewError(null);
      return;
    }

    let cancelled = false;
    let timer: number | null = null;

    const snap = async () => {
      const url = await previewFrame(cameraDevice);
      if (cancelled) return;
      if (url) {
        setPreviewFrameUrl(url);
        setPreviewError(null);
      } else {
        setPreviewError('Aucune image obtenue. Vérifiez que la caméra est libre.');
      }
    };

    invoke('webcam_preview_start', { device: cameraDevice })
      .then(() => {
        if (cancelled) return;
        void snap();
        // La source caméra tourne à 10 i/s : inutile de l'interroger plus vite
        timer = window.setInterval(() => {
          void snap();
        }, 300);
      })
      .catch((e) => {
        if (!cancelled) setPreviewError(String(e));
      });

    return () => {
      cancelled = true;
      if (timer !== null) window.clearInterval(timer);
      invoke('webcam_preview_stop').catch(() => undefined);
    };
  }, [webcamEnabled, isRecording, cameraDevice, previewFrame]);

  // Entrées par défaut : le premier micro, et le premier moniteur système
  useEffect(() => {
    const devices = capabilities?.audio_devices ?? [];
    if (!audioDevice) {
      const mic = devices.find((d) => d.kind === 'mic');
      if (mic) setAudioDevice(mic.id);
    }
    if (!systemAudioDevice) {
      const system = devices.find((d) => d.kind === 'system');
      if (system) setSystemAudioDevice(system.id);
    }
  }, [capabilities, audioDevice, systemAudioDevice]);

  // Caméra par défaut
  useEffect(() => {
    if (!cameraDevice && capabilities?.cameras.length) {
      setCameraDevice(capabilities.cameras[0].id);
    }
  }, [capabilities, cameraDevice]);

  /** Lance la capture, précédée du décompte si l'utilisateur en veut un */
  const handleStart = useCallback(async () => {
    try {
      if (countdownSeconds > 0) {
        for (let remaining = countdownSeconds; remaining > 0; remaining -= 1) {
          setCountdown(remaining);
          await new Promise((resolve) => setTimeout(resolve, 1000));
        }
      }

      // Le voile reste en place jusqu'à ce que la capture soit réellement
      // lancée : sans lui, on revoyait l'application et son indicateur de
      // chargement juste après le décompte.
      setCountdown('starting');

      await start({
        source,
        window_id: source === 'window' ? windowId : null,
        monitor: source === 'fullscreen' ? monitorIndex : null,
        region: source === 'region' ? region : null,
        fps,
        quality,
        resolution,
        backdrop,
        audio: audio || systemAudio,
        audio_device: audio ? audioDevice : null,
        system_audio_device: systemAudio ? systemAudioDevice : null,
        hide_app: hideApp,
        timer_in_video: timerInVideo,
        capture_cursor: captureCursor,
        output_dir: settings?.default_save_path ?? null,
        webcam: webcamEnabled
          ? {
              device: cameraDevice,
              layout,
              shape,
              size_percent: sizePercent,
              margin,
              presentation,
            }
          : null,
      });
    } finally {
      setCountdown(null);
    }
  }, [
    start,
    countdownSeconds,
    source,
    windowId,
    monitorIndex,
    region,
    fps,
    quality,
    resolution,
    backdrop,
    audio,
    audioDevice,
    systemAudio,
    systemAudioDevice,
    hideApp,
    timerInVideo,
    captureCursor,
    settings,
    webcamEnabled,
    cameraDevice,
    layout,
    shape,
    sizePercent,
    margin,
    presentation,
  ]);

  const canStart =
    capabilities?.available === true &&
    !isRecording &&
    !isStarting &&
    (source !== 'window' || windowId !== null) &&
    (source !== 'region' || region !== null) &&
    (!webcamEnabled || cameraDevice !== '');

  return (
    <div className="recording-panel">
      {/* En-tête */}
      <div className="recording-header">
        <div>
          <h2>Enregistrement d'écran</h2>
          <p>
            {capabilities?.available ? (
              <>
                Encodage H.264
                {capabilities.version
                  ? ` · ${capabilities.version.replace('ffmpeg version ', 'v').split(' ')[0]}`
                  : ''}
                {' · '}
                <span className={capabilities.hardware_encoding ? 'enc-hw' : 'enc-sw'}>
                  {encoder ?? capabilities.encoder}
                </span>
              </>
            ) : (
              'ffmpeg est requis pour enregistrer une vidéo'
            )}
          </p>
        </div>
        <button className="icon-btn" onClick={onClose} title="Fermer">
          <svg viewBox="0 0 24 24" width="18" height="18">
            <path d="M18 6L6 18M6 6L18 18" stroke="currentColor" strokeWidth="2" />
          </svg>
        </button>
      </div>

      {/* Détection en cours : surtout ne pas annoncer ffmpeg comme absent */}
      {checkingCapabilities && !capabilities && (
        <div className="recording-checking">
          <span className="spinner" />
          <span>Détection de ffmpeg, des caméras et des entrées audio…</span>
        </div>
      )}

      {!checkingCapabilities && capabilities && !capabilities.available && (
        <div className="recording-unavailable">
          <svg viewBox="0 0 24 24" width="20" height="20">
            <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="2" fill="none" />
            <path d="M12 7V13" stroke="currentColor" strokeWidth="2" />
            <circle cx="12" cy="17" r="1" fill="currentColor" />
          </svg>
          <div>
            <strong>ffmpeg est requis pour enregistrer</strong>
            <p>
              FastCap peut l'installer pour vous : le système demandera votre mot de
              passe, puis la détection sera relancée.
            </p>
          </div>
          <button className="btn-secondary" onClick={() => setShowDependencies(true)}>
            Installer
          </button>
        </div>
      )}

      {error && <div className="recording-error">{error}</div>}

      {/* Barre d'état / contrôles */}
      <div className={`recording-bar ${isRecording ? 'is-recording' : ''}`}>
        {isRecording ? (
          <>
            <span className="rec-dot" />
            <span className="rec-time">{formatDuration(elapsedMs)}</span>
            <span className="rec-hint">
              {stats && stats.frames > 0
                ? `${stats.captured_fps.toFixed(0)} i/s · ${stats.frames} images`
                : hideApp
                  ? 'Arrêtez depuis la zone de notification ou Ctrl + Shift + E'
                  : 'Enregistrement en cours'}
            </span>
            <button
              className="btn-stop"
              onClick={async () => {
                try {
                  // Sans retour visuel, l'utilisateur ne savait pas ce qui
                  // avait été produit : on ouvre le résultat directement.
                  const info = await stop();
                  setLastRecording(info);
                  setPlaying({ path: info.path, filename: info.filename });
                } catch {
                  // L'erreur est déjà affichée par le bandeau du panneau
                }
              }}
            >
              Arrêter
            </button>
          </>
        ) : (
          <>
            <span className="rec-idle">
              {isStarting ? 'Ouverture des périphériques…' : 'Prêt à enregistrer'}
            </span>
            <button className="btn-record" onClick={handleStart} disabled={!canStart}>
              {isStarting ? <span className="spinner" /> : <span className="rec-dot" />}
              {isStarting ? 'Démarrage…' : "Démarrer l'enregistrement"}
            </button>
          </>
        )}
      </div>

      {/* Réglages trop lourds : la vidéo reste à la bonne vitesse, mais saccade */}
      {isRecording && stats && stats.frames > 30 && stats.health_percent < 70 && (
        <div className="recording-warning">
          La machine ne suit pas : {stats.captured_fps.toFixed(0)} i/s sur{' '}
          {stats.target_fps} demandées. La durée restera juste, mais la vidéo sera
          saccadée — baissez la résolution, les images par seconde, ou désactivez
          l'arrière-plan pour le prochain enregistrement.
        </div>
      )}

      <div className="recording-body">
        {/* Colonne réglages */}
        <div className="recording-column">
          <section className="rec-section">
            <h3>Source</h3>
            <div className="segmented">
              {(
                [
                  { id: 'fullscreen', label: 'Plein écran' },
                  { id: 'region', label: 'Zone' },
                  { id: 'window', label: 'Fenêtre' },
                ] as const
              ).map((option) => (
                <button
                  key={option.id}
                  className={`segment ${source === option.id ? 'active' : ''}`}
                  onClick={() => setSource(option.id)}
                  disabled={isRecording}
                >
                  {option.label}
                </button>
              ))}
            </div>

            {source === 'region' && (
              <div className="rec-row">
                {region ? (
                  <span className="rec-badge">
                    Zone {region.width} × {region.height} à ({region.x}, {region.y})
                  </span>
                ) : (
                  <span className="rec-muted">Aucune zone définie</span>
                )}
                <button
                  className="btn-secondary btn-small"
                  onClick={handlePickRegion}
                  disabled={isRecording}
                >
                  {region ? 'Redéfinir' : 'Définir la zone'}
                </button>
              </div>
            )}

            {/* Choix de l'écran : inutile d'encombrer quand il n'y en a qu'un */}
            {source === 'fullscreen' && monitors.length > 1 && (
              <div className="rec-field">
                <label>Écran à filmer</label>
                <div className="monitor-row">
                  {monitors.map((monitor) => (
                    <button
                      key={monitor.index}
                      className={`monitor-choice ${
                        monitorIndex === monitor.index ? 'active' : ''
                      }`}
                      onClick={() => setMonitorIndex(monitor.index)}
                      disabled={isRecording}
                    >
                      <span className="monitor-screen">{monitor.index + 1}</span>
                      <span className="monitor-label">
                        {monitor.name}
                        {monitor.is_primary && <em> — principal</em>}
                      </span>
                      <span className="monitor-size">
                        {monitor.width} × {monitor.height}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            )}

            {sourcePreview && !isRecording && (
              <div className="source-preview">
                <img src={sourcePreview} alt="Aperçu de la source" />
                <span>Ce qui sera enregistré</span>
              </div>
            )}

            {source === 'window' && (
              <div className="rec-row">
                <select
                  className="setting-select"
                  value={windowId ?? ''}
                  onChange={(e) => setWindowId(e.target.value ? Number(e.target.value) : null)}
                  disabled={isRecording}
                >
                  <option value="">Choisir une fenêtre…</option>
                  {windows.map((w) => (
                    <option key={w.id} value={w.id}>
                      {w.app_name ? `${w.app_name} — ` : ''}
                      {w.title}
                    </option>
                  ))}
                </select>
                <button className="btn-secondary btn-small" onClick={refreshWindows}>
                  Rafraîchir
                </button>
              </div>
            )}
          </section>

          <section className="rec-section">
            <h3>Vidéo</h3>

            <div className="rec-field">
              <label>Résolution</label>
              <div className="chip-row">
                {RESOLUTIONS.map((option) => (
                  <button
                    key={option.id}
                    className={`chip ${resolution === option.id ? 'active' : ''}`}
                    onClick={() => setResolution(option.id)}
                    disabled={isRecording}
                    title={option.hint}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="rec-field">
              <label>Images par seconde</label>
              <div className="chip-row">
                {FPS_CHOICES.map((value) => (
                  <button
                    key={value}
                    className={`chip ${fps === value ? 'active' : ''}`}
                    onClick={() => setFps(value)}
                    disabled={isRecording}
                  >
                    {value} i/s
                  </button>
                ))}
              </div>
            </div>

            <div className="rec-field">
              <label>Qualité</label>
              <div className="chip-row">
                {QUALITIES.map((value) => (
                  <button
                    key={value.id}
                    className={`chip ${quality === value.id ? 'active' : ''}`}
                    onClick={() => setQuality(value.id)}
                    disabled={isRecording}
                    title={value.hint}
                  >
                    {value.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="rec-field">
              <label>
                Arrière-plan
                <em className="rec-muted"> — capture arrondie posée sur un dégradé</em>
              </label>
              <div className="backdrop-row">
                {BACKDROPS.map((option) => (
                  <button
                    key={option.id}
                    className={`backdrop-choice backdrop-${option.id} ${
                      backdrop === option.id ? 'active' : ''
                    }`}
                    onClick={() => setBackdrop(option.id)}
                    disabled={isRecording}
                    title={option.label}
                  >
                    <span className="backdrop-swatch" />
                    <span>{option.label}</span>
                  </button>
                ))}
              </div>
            </div>

            <label className="switch-row">
              <input
                type="checkbox"
                checked={captureCursor}
                onChange={(e) => setCaptureCursor(e.target.checked)}
                disabled={isRecording}
              />
              <span>Afficher le pointeur de souris</span>
            </label>

            <label className="switch-row">
              <input
                type="checkbox"
                checked={hideApp}
                onChange={(e) => setHideApp(e.target.checked)}
                disabled={isRecording}
              />
              <span>Masquer FastCap pendant l'enregistrement</span>
            </label>

            <label className="switch-row">
              <input
                type="checkbox"
                checked={timerInVideo}
                onChange={(e) => setTimerInVideo(e.target.checked)}
                disabled={isRecording}
              />
              <span>Inscrire le minuteur REC dans la vidéo</span>
            </label>

            <label className="switch-row">
              <input
                type="checkbox"
                checked={audio}
                onChange={(e) => setAudio(e.target.checked)}
                disabled={isRecording || !capabilities?.audio_devices.length}
              />
              <span>
                Enregistrer le son
                {capabilities && capabilities.audio_devices.length === 0 && (
                  <em className="rec-muted"> — aucune entrée détectée</em>
                )}
              </span>
            </label>

            {audio && !!capabilities?.audio_devices.length && (
              <div className="rec-field">
                <label>Microphone</label>
                <select
                  className="setting-select"
                  value={audioDevice}
                  onChange={(e) => setAudioDevice(e.target.value)}
                  disabled={isRecording}
                >
                  {capabilities.audio_devices
                    .filter((device) => device.kind !== 'system')
                    .map((device) => (
                      <option key={device.id} value={device.id}>
                        {device.label}
                      </option>
                    ))}
                </select>
              </div>
            )}

            <label className="switch-row">
              <input
                type="checkbox"
                checked={systemAudio}
                onChange={(e) => setSystemAudio(e.target.checked)}
                disabled={
                  isRecording ||
                  !capabilities?.audio_devices.some((d) => d.kind === 'system')
                }
              />
              <span>
                Enregistrer le son de l'ordinateur
                <em className="rec-muted">
                  {' '}
                  — musique, vidéos, et la voix des autres participants
                </em>
              </span>
            </label>

            {systemAudio && (
              <div className="rec-field">
                <label>Sortie à capter</label>
                <select
                  className="setting-select"
                  value={systemAudioDevice}
                  onChange={(e) => setSystemAudioDevice(e.target.value)}
                  disabled={isRecording}
                >
                  {capabilities?.audio_devices
                    .filter((device) => device.kind === 'system')
                    .map((device) => (
                      <option key={device.id} value={device.id}>
                        {device.label}
                      </option>
                    ))}
                </select>
              </div>
            )}

            {audio && systemAudio && (
              <p className="rec-muted small">
                Les deux sources sont fondues en une seule piste : votre voix et
                celle de vos interlocuteurs figurent dans la même vidéo.
              </p>
            )}

            <div className="rec-field">
              <label>Décompte avant de démarrer</label>
              <div className="chip-row">
                {[0, 3, 5, 10].map((value) => (
                  <button
                    key={value}
                    className={`chip ${countdownSeconds === value ? 'active' : ''}`}
                    onClick={() => setCountdownSeconds(value)}
                    disabled={isRecording}
                  >
                    {value === 0 ? 'Aucun' : `${value} s`}
                  </button>
                ))}
              </div>
            </div>
          </section>
        </div>

        {/* Colonne webcam */}
        <div className="recording-column">
          <section className="rec-section">
            <h3>
              Ma caméra
              <label className="switch-inline">
                <input
                  type="checkbox"
                  checked={webcamEnabled}
                  onChange={(e) => setWebcamEnabled(e.target.checked)}
                  disabled={isRecording}
                />
                <span>Incruster</span>
              </label>
            </h3>

            <div className={`webcam-preview ${webcamEnabled ? 'active' : ''}`}>
              {isRecording ? (
                <div className="webcam-live">
                  <span className="rec-dot" />
                  <span>
                    {hasWebcam
                      ? 'Caméra incrustée dans la vidéo'
                      : 'Enregistrement sans caméra'}
                  </span>
                  <em className="rec-muted">
                    {LAYOUTS.find((l) => l.id === layout)?.label ?? layout}
                    {' · '}
                    {PRESENTATIONS.find((p) => p.id === presentation)?.label ?? presentation}
                    {' · '}
                    {sizePercent}%
                  </em>
                </div>
              ) : previewFrameUrl ? (
                <img src={previewFrameUrl} alt="Aperçu caméra" className="webcam-img" />
              ) : (
                <div className="webcam-placeholder">
                  {webcamEnabled ? 'Aperçu en cours…' : 'Aperçu désactivé'}
                </div>
              )}
            </div>

            {previewError && webcamEnabled && <p className="rec-muted small">{previewError}</p>}

            <div className="rec-field">
              <label>Périphérique</label>
              <div className="rec-row">
                <select
                  className="setting-select"
                  value={cameraDevice}
                  onChange={(e) => setCameraDevice(e.target.value)}
                  disabled={!webcamEnabled || isRecording}
                >
                  {capabilities?.cameras.length ? (
                    capabilities.cameras.map((cam) => (
                      <option key={cam.id} value={cam.id}>
                        {cam.label}
                      </option>
                    ))
                  ) : (
                    <option value="">Aucune caméra détectée</option>
                  )}
                </select>
                <button className="btn-secondary btn-small" onClick={refreshCapabilities}>
                  Scanner
                </button>
                  
                  <button
                    className="btn-secondary btn-small"
                    onClick={handleCompositePreview}
                    disabled={!webcamEnabled || !cameraDevice || isRecording}
                    title="Aperçu de la webcam incrustée telle qu'elle apparaîtra dans la vidéo"
                  >
                    Aperçu incrustation
                  </button>
              </div>
            </div>

            <div className="rec-field">
              <label>Disposition</label>
              <div className="layout-grid">
                {LAYOUTS.map((option) => (
                  <button
                    key={option.id}
                    className={`layout-choice ${layout === option.id ? 'active' : ''}`}
                    onClick={() => setLayout(option.id)}
                    disabled={!webcamEnabled || isRecording}
                    title={option.label}
                  >
                    <LayoutPreview layout={option.id} />
                    <span>{option.label}</span>
                  </button>
                ))}
              </div>
            </div>

            {layout.startsWith('pip') && (
              <div className="rec-field">
                <label>Présentation — style Tella</label>
                <div className="preset-grid">
                  {PRESENTATIONS.map((option) => (
                    <button
                      key={option.id}
                      className={`preset-choice ${presentation === option.id ? 'active' : ''}`}
                      onClick={() => setPresentation(option.id)}
                      disabled={!webcamEnabled || isRecording}
                    >
                      <span className={`preset-thumb preset-${option.id}`} />
                      <span>{option.label}</span>
                      <em>{option.hint}</em>
                    </button>
                  ))}
                </div>
              </div>
            )}

            {(layout.startsWith('pip') || layout === 'full') && (
              <div className="rec-field">
                <label>Forme</label>
                <div className="chip-row">
                  {SHAPES.map((option) => (
                    <button
                      key={option.id}
                      className={`chip ${shape === option.id ? 'active' : ''}`}
                      onClick={() => setShape(option.id)}
                      disabled={!webcamEnabled || isRecording || layout === 'full'}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {layout.startsWith('pip') && (
              <>
                <div className="rec-field">
                  <label>Taille — {sizePercent}%</label>
                  <input
                    type="range"
                    min="10"
                    max="60"
                    value={sizePercent}
                    onChange={(e) => setSizePercent(Number(e.target.value))}
                    disabled={!webcamEnabled || isRecording}
                    className="setting-slider"
                  />
                </div>
                <div className="rec-field">
                  <label>Marge — {margin} px</label>
                  <input
                    type="range"
                    min="0"
                    max="120"
                    value={margin}
                    onChange={(e) => setMargin(Number(e.target.value))}
                    disabled={!webcamEnabled || isRecording}
                    className="setting-slider"
                  />
                </div>
              </>
            )}

            {/* Pilotage en direct : n'a de sens qu'une fois la capture lancée */}
            {isRecording && live && (
              <div className="live-controls">
                <h4>Piloter pendant l'enregistrement</h4>
                <div className="live-grid">
                  <button className="live-btn" onClick={() => applyLive('toggle-camera')}>
                    {live.camera_visible ? 'Masquer la caméra' : 'Afficher la caméra'}
                    <kbd>Ctrl+Maj+H</kbd>
                  </button>
                  <button className="live-btn" onClick={() => applyLive('toggle-swap')}>
                    {live.swapped ? 'Remettre l’écran en grand' : 'Caméra en grand'}
                    <kbd>Ctrl+Maj+X</kbd>
                  </button>
                  <button className="live-btn" onClick={() => applyLive('next-corner')}>
                    Coin suivant
                    <kbd>Ctrl+Maj+L</kbd>
                  </button>
                  <button className="live-btn" onClick={() => applyLive('next-shape')}>
                    Forme suivante
                    <kbd>Ctrl+Maj+F</kbd>
                  </button>
                  <button className="live-btn" onClick={() => applyLive({ resize: 5 })}>
                    Agrandir
                    <kbd>Ctrl+Maj++</kbd>
                  </button>
                  <button className="live-btn" onClick={() => applyLive({ resize: -5 })}>
                    Réduire
                    <kbd>Ctrl+Maj+-</kbd>
                  </button>
                </div>
                <p className="rec-muted small">
                  {LAYOUTS.find((l) => l.id === live.layout)?.label ?? live.layout}
                  {' · '}
                  {SHAPES.find((sh) => sh.id === live.shape)?.label ?? live.shape}
                  {' · '}
                  {live.size_percent}%
                  {live.swapped ? ' · rôles échangés' : ''}
                  {live.camera_visible ? '' : ' · caméra masquée'}
                </p>
              </div>
            )}

            {webcamEnabled && !isRecording && (
              <div className="rec-shortcuts">
                <h4>Déplacer l'incrustation en direct</h4>
                <p>Pendant l'enregistrement, utilisez ces raccourcis globaux :</p>
                <div className="shortcut-grid">
                  <span>
                    <kbd>Ctrl+Maj+H</kbd> Masquer / afficher la caméra
                  </span>
                  <span>
                    <kbd>Ctrl+Maj+X</kbd> Échanger écran et caméra
                  </span>
                  <span>
                    <kbd>Ctrl+Maj+L</kbd> Coin suivant
                  </span>
                  <span>
                    <kbd>Ctrl+Maj+F</kbd> Forme suivante
                  </span>
                  <span>
                    <kbd>Ctrl+Maj++</kbd> / <kbd>Ctrl+Maj+-</kbd> Taille
                  </span>
                  <span>
                    <kbd>Ctrl+Maj+E</kbd> Arrêter l'enregistrement
                  </span>
                </div>
              </div>
            )}
          </section>
        </div>
      </div>

      {/* Résultat du dernier enregistrement */}
      {lastRecording && !isRecording && (
        <div className="recording-result">
          <div className="result-icon">
            <svg viewBox="0 0 24 24" width="18" height="18">
              <path d="M5 13L9 17L19 7" stroke="currentColor" strokeWidth="2.4" fill="none" />
            </svg>
          </div>
          <div className="result-info">
            <strong>Enregistrement terminé</strong>
            <span>
              {lastRecording.filename} · {formatDuration(lastRecording.duration_ms)} ·{' '}
              {lastRecording.width}×{lastRecording.height} ·{' '}
              {formatSize(lastRecording.size_bytes)} ·{' '}
              {lastRecording.captured_fps.toFixed(0)} i/s réelles
            </span>
          </div>
          <div className="result-actions">
            <button
              className="btn-secondary btn-small"
              onClick={() =>
                setPlaying({
                  path: lastRecording.path,
                  filename: lastRecording.filename,
                })
              }
            >
              Revoir
            </button>
            <button
              className="btn-secondary btn-small"
              onClick={() =>
                setEditing({
                  path: lastRecording.path,
                  filename: lastRecording.filename,
                })
              }
            >
              Monter
            </button>
            <button
              className="btn-ghost btn-small"
              onClick={() => revealItemInDir(lastRecording.path).catch(console.error)}
            >
              Dossier
            </button>
            <button className="icon-btn" onClick={() => setLastRecording(null)} title="Masquer">
              <svg viewBox="0 0 24 24" width="16" height="16">
                <path d="M18 6L6 18M6 6L18 18" stroke="currentColor" strokeWidth="2" />
              </svg>
            </button>
          </div>
        </div>
      )}

      {/* Enregistrements */}
      <section className="rec-section recordings-section">
        <h3>
          Mes enregistrements
          <button className="btn-ghost btn-small" onClick={loadRecordings}>
            Actualiser
          </button>
        </h3>

        {recordings.length === 0 ? (
          <p className="rec-muted">Aucun enregistrement pour l'instant.</p>
        ) : (
          <div className="recordings-grid">
            {recordings.map((item) => (
              <div className="recording-card" key={item.id}>
                <button
                  className="recording-card-thumb"
                  title="Lire la vidéo"
                  onClick={() => setPlaying({ path: item.path, filename: item.filename })}
                >
                  {posters[item.id] ? (
                    <img src={posters[item.id]} alt="" className="recording-poster" />
                  ) : (
                    <svg viewBox="0 0 24 24" width="22" height="22">
                      <rect x="2" y="5" width="14" height="14" rx="2" stroke="currentColor" strokeWidth="1.6" fill="none" />
                      <path d="M16 10L22 7V17L16 14" stroke="currentColor" strokeWidth="1.6" fill="none" />
                    </svg>
                  )}
                  <span className="recording-play-overlay">
                    <svg viewBox="0 0 24 24" width="18" height="18">
                      <path d="M8 5L19 12L8 19V5Z" fill="currentColor" />
                    </svg>
                  </span>
                  {item.has_webcam && <span className="recording-badge">cam</span>}
                </button>
                <div className="recording-card-info">
                  <span className="recording-name">{item.filename}</span>
                  <span className="recording-meta">
                    {formatDuration(item.duration_ms)} · {item.width}×{item.height} · {item.fps} i/s ·{' '}
                    {formatSize(item.size_bytes)}
                  </span>
                </div>
                <div className="recording-card-actions">
                  <button
                    className="icon-btn"
                    title="Lire la vidéo"
                    onClick={() => setPlaying({ path: item.path, filename: item.filename })}
                  >
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path d="M8 5L19 12L8 19V5Z" stroke="currentColor" strokeWidth="1.8" fill="none" />
                    </svg>
                  </button>
                  <button
                    className="icon-btn"
                    title="Monter la vidéo"
                    onClick={() => setEditing({ path: item.path, filename: item.filename })}
                  >
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path
                        d="M4 7h16M4 12h10M4 17h6M17 12l4 5h-8Z"
                        stroke="currentColor"
                        strokeWidth="1.8"
                        fill="none"
                        strokeLinejoin="round"
                      />
                    </svg>
                  </button>
                  <button
                    className="icon-btn"
                    title="Afficher dans le dossier"
                    onClick={() => revealItemInDir(item.path).catch(console.error)}
                  >
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path d="M3 7V17C3 18 4 19 5 19H19C20 19 21 18 21 17V9C21 8 20 7 19 7H12L10 5H5C4 5 3 6 3 7Z" stroke="currentColor" strokeWidth="1.8" fill="none" />
                    </svg>
                  </button>
                  <button
                    className="icon-btn icon-btn-danger"
                    title="Supprimer"
                    onClick={() => remove(item.id).catch(console.error)}
                  >
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path d="M4 7H20M9 7V5H15V7M7 7L8 20H16L17 7" stroke="currentColor" strokeWidth="1.8" fill="none" />
                    </svg>
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {countdown !== null && (
        <div className="countdown-overlay">
          {countdown === 'starting' ? (
            <>
              <span className="spinner spinner-large" />
              <p>Ouverture des périphériques…</p>
            </>
          ) : (
            <>
              <div className="countdown-ring">
                <span key={countdown} className="countdown-number">
                  {countdown}
                </span>
              </div>
              <p>Préparez-vous…</p>
            </>
          )}
        </div>
      )}

      {showDependencies && (
        <Dependencies
          onInstalled={refreshCapabilities}
          onClose={() => setShowDependencies(false)}
        />
      )}

      {editing && (
        <VideoEditor
          path={editing.path}
          filename={editing.filename}
          onClose={() => setEditing(null)}
          onExported={() => loadRecordings()}
        />
      )}

      {playing && (
        <VideoPlayer
          path={playing.path}
          filename={playing.filename}
          onClose={() => setPlaying(null)}
        />
      )}
    </div>
  );
}
