import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { sendNotification, isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';

// Types
export interface CaptureResult {
  id: string;
  timestamp: string;
  width: number;
  height: number;
  format: string;
  data_url: string;
  path?: string;
}

export type AnnotationToolId =
  | 'select'
  | 'pen'
  | 'highlight'
  | 'rectangle'
  | 'ellipse'
  | 'line'
  | 'arrow'
  | 'text'
  | 'number'
  | 'blur'
  | 'mosaic'
  | 'crop';

export interface AnnotationTool {
  id: string;
  tool: Exclude<AnnotationToolId, 'select' | 'crop'>;
  x: number;
  y: number;
  width: number;
  height: number;
  color: string;
  thickness: number;
  filled?: boolean;
  text?: string;
  points?: { x: number; y: number }[];
  fontSize?: number;
}

export interface ExportOptions {
  format: 'png' | 'jpeg';
  quality?: number;
  path?: string;
  filename?: string;
}

export interface AppSettings {
  default_export_format: string;
  default_export_quality: number;
  default_save_path: string;
  auto_save: boolean;
  auto_copy_to_clipboard: boolean;
  screenshot_shortcut: string;
  capture_on_startup: boolean;
  theme: 'light' | 'dark' | 'system';
  accent_color: string;
}

export interface AppInfo {
  name: string;
  version: string;
  description: string;
}

export interface OcrResult {
  text: string;
  language: string;
  confidence: number;
  words: Array<{
    text: string;
    confidence: number;
    bbox?: [number, number, number, number];
  }>;
}

export interface WindowInfo {
  id: number;
  title: string;
  app_name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_minimized: boolean;
  is_maximized: boolean;
}

export interface MonitorInfo {
  index: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
  scale_factor: number;
}

// Hook principal de l'application
export function useApp() {
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Charger les infos de l'application
  useEffect(() => {
    invoke<AppInfo>('get_app_info')
      .then(setAppInfo)
      .catch((e) => setError(`Erreur: ${e}`))
      .finally(() => setIsLoading(false));
  }, []);

  // Charger les paramètres
  useEffect(() => {
    invoke<{ settings: AppSettings }>('get_settings')
      .then((res) => setSettings(res.settings))
      .catch((e) => console.error('Erreur chargement settings:', e));
  }, []);

  // Sauvegarder les paramètres
  const saveSettings = useCallback(async (newSettings: AppSettings) => {
    try {
      await invoke('save_settings', { settings: newSettings });
      setSettings(newSettings);
    } catch (e) {
      console.error('Erreur sauvegarde settings:', e);
      throw e;
    }
  }, []);

  return {
    appInfo,
    settings,
    isLoading,
    error,
    saveSettings,
  };
}

// Hook pour la capture d'écran
export function useCapture() {
  const [isCapturing, setIsCapturing] = useState(false);
  const [lastCapture, setLastCapture] = useState<CaptureResult | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  const captureFullscreen = useCallback(async () => {
    setIsCapturing(true);
    setCaptureError(null);
    try {
      const result = await invoke<CaptureResult>('capture_fullscreen');
      setLastCapture(result);
      return result;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setCaptureError(msg);
      throw e;
    } finally {
      setIsCapturing(false);
    }
  }, []);

  const captureRegion = useCallback(async (x: number, y: number, width: number, height: number) => {
    setIsCapturing(true);
    setCaptureError(null);
    try {
      const result = await invoke<CaptureResult>('capture_region', { x, y, width, height });
      setLastCapture(result);
      return result;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setCaptureError(msg);
      throw e;
    } finally {
      setIsCapturing(false);
    }
  }, []);

  const captureWindow = useCallback(async (windowId: number) => {
    setIsCapturing(true);
    setCaptureError(null);
    try {
      const result = await invoke<CaptureResult>('capture_window', { windowId });
      setLastCapture(result);
      return result;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setCaptureError(msg);
      throw e;
    } finally {
      setIsCapturing(false);
    }
  }, []);

  const listWindows = useCallback(async () => {
    try {
      return await invoke<WindowInfo[]>('list_windows');
    } catch (e) {
      console.error('Erreur liste fenêtres:', e);
      return [];
    }
  }, []);

  const listMonitors = useCallback(async () => {
    try {
      return await invoke<MonitorInfo[]>('list_monitors');
    } catch (e) {
      console.error('Erreur liste écrans:', e);
      return [];
    }
  }, []);

  return {
    isCapturing,
    lastCapture,
    captureError,
    captureFullscreen,
    captureRegion,
    captureWindow,
    listWindows,
    listMonitors,
  };
}

// Hook pour l'annotation
export function useAnnotation() {
  const annotate = useCallback(async (dataUrl: string, annotations: AnnotationTool[]) => {
    try {
      const result = await invoke<string>('annotate_image', { dataUrl, annotations });
      return result;
    } catch (e) {
      console.error('Erreur annotation:', e);
      throw e;
    }
  }, []);

  return { annotate };
}

// Hook pour l'OCR
export function useOcr() {
  const [isProcessing, setIsProcessing] = useState(false);
  const [ocrResult, setOcrResult] = useState<OcrResult | null>(null);
  const [ocrError, setOcrError] = useState<string | null>(null);

  // Vérifie que le moteur OCR (binaire tesseract) est disponible
  useEffect(() => {
    invoke<boolean>('ocr_is_available').then((available) => {
      if (!available) {
        setOcrError(
          "OCR indisponible : installez tesseract et ajoutez-le au PATH."
        );
      }
    }).catch(() => undefined);
  }, []);

  const recognize = useCallback(async (dataUrl: string, language?: string) => {
    setIsProcessing(true);
    setOcrError(null);
    try {
      const result = await invoke<OcrResult>('ocr_image', { dataUrl, language });
      setOcrResult(result);
      return result;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setOcrError(msg);
      throw e;
    } finally {
      setIsProcessing(false);
    }
  }, []);

  return { isProcessing, ocrResult, ocrError, recognize };
}

// Hook pour l'export
export function useExport() {
  const exportImage = useCallback(async (dataUrl: string, options: ExportOptions) => {
    try {
      const path = await invoke<string>('export_image', { dataUrl, options });
      return path;
    } catch (e) {
      console.error('Erreur export:', e);
      throw e;
    }
  }, []);

  const copyToClipboard = useCallback(async (dataUrl: string) => {
    try {
      // Copie de l'image dans le presse-papiers système (côté Rust)
      await invoke('copy_image_to_clipboard', { dataUrl });
      return true;
    } catch (e) {
      console.error('Erreur copie presse-papiers:', e);
      throw e;
    }
  }, []);

  return { exportImage, copyToClipboard };
}

// Hook pour l'historique
export function useCaptureHistory() {
  const [history, setHistory] = useState<Array<{
    id: string;
    timestamp: string;
    filename: string;
    path: string;
    width: number;
    height: number;
    format: string;
    size_bytes: number;
  }>>([]);
  const [isLoading, setIsLoading] = useState(false);

  const loadHistory = useCallback(async () => {
    setIsLoading(true);
    try {
      const result = await invoke<{ captures: any[] }>('get_capture_history');
      setHistory(result.captures);
    } catch (e) {
      console.error('Erreur chargement historique:', e);
    } finally {
      setIsLoading(false);
    }
  }, []);

  /** Recharge une capture du disque sous forme de data URL (vignettes) */
  const readCaptureImage = useCallback(async (path: string) => {
    try {
      return await invoke<string>('read_capture_image', { path });
    } catch (e) {
      console.error('Erreur lecture capture:', e);
      return null;
    }
  }, []);

  /** Recharge une capture du disque comme vignette réduite (léger, pour les galeries) */
  const readCaptureThumbnail = useCallback(
    async (path: string, maxWidth = 360) => {
      try {
        return await invoke<string>('read_capture_thumbnail', { path, maxWidth });
      } catch (e) {
        console.error('Erreur lecture vignette:', e);
        return null;
      }
    },
    []
  );

  const deleteCapture = useCallback(async (id: string) => {
    try {
      await invoke('delete_capture', { id });
      setHistory(prev => prev.filter(c => c.id !== id));
    } catch (e) {
      console.error('Erreur suppression capture:', e);
      throw e;
    }
  }, []);

  const clearHistory = useCallback(async () => {
    try {
      await invoke('clear_capture_history');
      setHistory([]);
    } catch (e) {
      console.error('Erreur nettoyage historique:', e);
      throw e;
    }
  }, []);

  useEffect(() => {
    loadHistory();
  }, [loadHistory]);

  return { history, isLoading, loadHistory, readCaptureImage, readCaptureThumbnail, deleteCapture, clearHistory };
}

// --- Enregistrement vidéo ---

export interface CaptureDevice {
  id: string;
  label: string;
  /** "video" | "mic" | "system" */
  kind: string;
}

export interface RecorderCapabilities {
  available: boolean;
  version: string | null;
  cameras: CaptureDevice[];
  audio_devices: CaptureDevice[];
  /** Encodeur qui sera utilisé (matériel si disponible) */
  encoder: string;
  hardware_encoding: boolean;
}

export type WebcamLayout =
  | 'pip-tl'
  | 'pip-tr'
  | 'pip-bl'
  | 'pip-br'
  | 'side-left'
  | 'side-right'
  | 'full';

export type WebcamShape = 'square' | 'rounded' | 'circle';

export type WebcamPresentation = 'minimal' | 'classic' | 'studio' | 'bubble';

export interface WebcamOptions {
  device: string;
  layout: WebcamLayout;
  shape: WebcamShape;
  size_percent: number;
  margin: number;
  presentation: WebcamPresentation;
}

export interface RegionRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Plafond de résolution de la vidéo produite */
export type RecordingResolution = 'native' | '1080' | '720' | '480';

/** Arrière-plan décoratif appliqué à la capture */
export type RecordingBackdrop = 'none' | 'aurora' | 'sunset' | 'mint' | 'slate' | 'cream';

export interface RecordingOptions {
  source: 'fullscreen' | 'region' | 'window';
  window_id?: number | null;
  /** Écran à filmer, dans l'ordre de `list_monitors` */
  monitor?: number | null;
  region?: RegionRect | null;
  fps: number;
  quality: 'high' | 'balanced' | 'small';
  resolution: RecordingResolution;
  /** Inclure le pointeur de souris dans la vidéo */
  capture_cursor?: boolean;
  backdrop?: RecordingBackdrop | null;
  audio: boolean;
  /** Microphone à enregistrer */
  audio_device?: string | null;
  /** Sortie haut-parleurs à enregistrer (« ce que joue la machine ») */
  system_audio_device?: string | null;
  webcam?: WebcamOptions | null;
  hide_app: boolean;
  timer_in_video?: boolean;
  output_dir?: string | null;
}

export interface RecordingStart {
  id: string;
  path: string;
  started_ms: number;
  width: number;
  height: number;
  fps: number;
  encoder: string;
  hardware: boolean;
}

export interface RecordingInfo {
  id: string;
  path: string;
  filename: string;
  started_at: string;
  duration_ms: number;
  width: number;
  height: number;
  fps: number;
  size_bytes: number;
  has_webcam: boolean;
  /** Cadence réellement capturée, hors images répétées */
  captured_fps: number;
  encoder: string;
}

/** Compteurs de la session en cours, alimentés par ffmpeg */
export interface RecordingStats {
  frames: number;
  /** Images perdues par le capteur d'écran */
  dropped: number;
  captured_fps: number;
  target_fps: number;
  /** Part de la cadence demandée réellement atteinte, en pourcentage */
  health_percent: number;
}

export interface RecordingStatus {
  active: boolean;
  elapsed_ms: number;
  path: string | null;
  width: number;
  height: number;
  fps: number;
  has_webcam: boolean;
  encoder: string | null;
  stats: RecordingStats | null;
}

/** Hook d'enregistrement vidéo (capture écran + incrustation webcam) */
/** Ajustement applicable pendant l'enregistrement */
export type LiveAction =
  | 'next-corner'
  | 'next-shape'
  | 'toggle-camera'
  | 'toggle-swap'
  | { resize: number }
  | { corner: string };

/** État pilotable de l'incrustation */
export interface LiveState {
  layout: string;
  shape: string;
  size_percent: number;
  margin: number;
  camera_visible: boolean;
  swapped: boolean;
}

export function useRecorder() {
  const [capabilities, setCapabilities] = useState<RecorderCapabilities | null>(null);
  const [checkingCapabilities, setCheckingCapabilities] = useState(true);
  const [isRecording, setIsRecording] = useState(false);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [recordings, setRecordings] = useState<RecordingInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [stats, setStats] = useState<RecordingStats | null>(null);
  const [hasWebcam, setHasWebcam] = useState(false);
  // Le démarrage prend une à deux secondes : l'interface doit le montrer,
  // et surtout empêcher un second clic.
  const [isStarting, setIsStarting] = useState(false);
  const [encoder, setEncoder] = useState<string | null>(null);

  const refreshCapabilities = useCallback(async () => {
    // La sonde interroge ffmpeg, les caméras et les entrées audio : plusieurs
    // secondes. Tant qu'elle n'a pas répondu, l'interface doit annoncer une
    // détection en cours et non une absence de ffmpeg.
    setCheckingCapabilities(true);
    try {
      setCapabilities(await invoke<RecorderCapabilities>('recorder_capabilities'));
    } catch (e) {
      console.error('Erreur capacités enregistreur:', e);
    } finally {
      setCheckingCapabilities(false);
    }
  }, []);

  const loadRecordings = useCallback(async () => {
    try {
      setRecordings(await invoke<RecordingInfo[]>('get_recordings'));
    } catch (e) {
      console.error('Erreur chargement enregistrements:', e);
    }
  }, []);

  const start = useCallback(async (options: RecordingOptions) => {
    setError(null);
    setIsStarting(true);
    try {
      const result = await invoke<RecordingStart>('start_recording', { options });
      setIsRecording(true);
      setElapsedMs(0);
      setLive(null);
      setStats(null);
      setEncoder(result.encoder);
      return result;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      throw e;
    } finally {
      setIsStarting(false);
    }
  }, []);

  const stop = useCallback(async () => {
    try {
      const info = await invoke<RecordingInfo>('stop_recording');
      setIsRecording(false);
      setElapsedMs(0);
      setStats(null);
      setRecordings((prev) => [info, ...prev]);
      return info;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setIsRecording(false);
      setStats(null);
      throw e;
    }
  }, []);

  const remove = useCallback(async (id: string) => {
    await invoke('delete_recording', { id });
    setRecordings((prev) => prev.filter((r) => r.id !== id));
  }, []);

  const [live, setLive] = useState<LiveState | null>(null);

  /** Ajuste l'incrustation pendant l'enregistrement (sans rien interrompre) */
  const applyLive = useCallback(async (action: LiveAction): Promise<LiveState | null> => {
    try {
      const state = await invoke<LiveState>('recorder_live', { action });
      setLive(state);
      return state;
    } catch (e) {
      console.error('Pilotage incrustation:', e);
      return null;
    }
  }, []);

  /** Prend une photo de la webcam (JPEG) pour l'aperçu de l'interface */
  const previewFrame = useCallback(async (device: string): Promise<string | null> => {
    try {
      return await invoke<string>('webcam_preview_frame', { device });
    } catch (e) {
      console.error('Aperçu webcam:', e);
      return null;
    }
  }, []);

  /** Aperçu composite : la webcam incrustée telle qu'elle apparaîtra dans la vidéo */
  const compositePreviewFrame = useCallback(
    async (params: {
      device: string;
      layout: WebcamLayout;
      shape: WebcamShape;
      size_percent: number;
      margin: number;
      presentation: WebcamPresentation;
      source: string;
      resolution: RecordingResolution;
      window_id: number | null;
      region: RegionRect | null;
    }): Promise<string | null> => {
      try {
        return await invoke<string>('render_webcam_composite_preview', params);
      } catch (e) {
        console.error('Aperçu incrustation webcam:', e);
        return null;
      }
    },
    []
  );

  // Suivi du temps écoulé et de l'état de la webcam pendant l'enregistrement
  useEffect(() => {
    if (!isRecording) return;

    const timer = window.setInterval(() => {
      invoke<RecordingStatus>('recording_status')
        .then((status) => {
          if (status.active) {
            setElapsedMs(status.elapsed_ms);
            setStats(status.stats);
            setEncoder(status.encoder);
            setHasWebcam(status.has_webcam);
          } else {
            // Arrêt déclenché ailleurs (raccourci, zone de notification) :
            // la liste doit refléter le nouvel enregistrement.
            setIsRecording(false);
            setStats(null);
            void loadRecordings();
          }
        })
        .catch(() => undefined);
    }, 500);

    return () => window.clearInterval(timer);
  }, [isRecording, loadRecordings]);

  useEffect(() => {
    refreshCapabilities();
    loadRecordings();
  }, [refreshCapabilities, loadRecordings]);

  return {
    capabilities,
    checkingCapabilities,
    isRecording,
    isStarting,
    elapsedMs,
    recordings,
    error,
    stats,
    live,
    applyLive,
    hasWebcam,
    encoder,
    refreshCapabilities,
    loadRecordings,
    start,
    stop,
    remove,
    previewFrame,
    compositePreviewFrame,
  };
}

// Hook pour les notifications
export function useNotification() {
  const requestPermissionAsync = async () => {
    let permissionGranted = await isPermissionGranted();
    if (!permissionGranted) {
      const permission = await requestPermission();
      permissionGranted = permission === 'granted';
    }
    return permissionGranted;
  };

  const showNotification = async (title: string, body: string) => {
    const granted = await requestPermissionAsync();
    if (granted) {
      await sendNotification({ title, body });
    }
  };

  return { showNotification };
}