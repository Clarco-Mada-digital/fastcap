import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Timeline, type FilmFrame, type Range, type RegionKind, type Selection } from './Timeline';
import { CropBox, fit, type Crop } from './CropBox';
import './VideoEditor.css';

interface MediaSummary {
  width: number;
  height: number;
  fps: number;
  duration: number;
  has_audio: boolean;
}

interface AudioAnalysis {
  peaks: number[];
  bucket: number;
  silences: Range[];
  floor_db: number | null;
  has_audio: boolean;
}

interface EditOutcome {
  path: string;
  filename: string;
  duration_ms: number;
  size_bytes: number;
  lossless: boolean;
}

interface VideoEditorProps {
  path: string;
  filename: string;
  onClose: () => void;
  onExported?: (outcome: EditOutcome) => void;
}

/** Ce qui se pose sur la ligne de temps, et que l'on peut annuler */
interface Cuts {
  trim: Range;
  removed: Range[];
  muted: Range[];
}

/** Réglages d'un carton, d'ouverture ou de fermeture */
interface Card {
  on: boolean;
  text: string;
  subtitle: string;
  backdrop: string;
  duration: number;
}

const EMPTY_CARD: Card = {
  on: false,
  text: '',
  subtitle: '',
  backdrop: 'aurora',
  duration: 3,
};

const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];

const RATIOS: { id: string; label: string; value: number | null }[] = [
  { id: 'free', label: 'Libre', value: null },
  { id: 'wide', label: '16:9', value: 16 / 9 },
  { id: 'square', label: '1:1', value: 1 },
  { id: 'tall', label: '9:16', value: 9 / 16 },
];

const BACKDROPS = [
  { id: 'aurora', label: 'Aurora' },
  { id: 'sunset', label: 'Sunset' },
  { id: 'mint', label: 'Mint' },
  { id: 'slate', label: 'Slate' },
  { id: 'cream', label: 'Cream' },
  { id: 'none', label: 'Sombre' },
];

/** Vignettes demandées pour la bande d'images */
const FILM_FRAMES = 48;

/** Marge laissée de part et d'autre d'un silence retiré : une coupe au ras du
 *  mot donne une élocution hachée. */
const SILENCE_PADDING = 0.12;

/** Un silence plus court que cela est une respiration, pas un blanc */
const SILENCE_MINIMUM = 0.8;

const HISTORY_LIMIT = 60;

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

function formatSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} Ko`;
  return `${(bytes / 1024 / 1024).toFixed(1)} Mo`;
}

/**
 * Passages effectivement conservés : ce qui reste entre le début et la fin
 * choisis, une fois retirés les passages supprimés.
 */
export function computeKeep(trim: Range, removed: Range[]): Range[] {
  const sorted = [...removed].sort((a, b) => a.start - b.start);
  const keep: Range[] = [];
  let cursor = trim.start;

  for (const cut of sorted) {
    const start = Math.max(cut.start, trim.start);
    const end = Math.min(cut.end, trim.end);
    if (end <= cursor) continue;
    if (start > cursor) keep.push({ start: cursor, end: Math.min(start, trim.end) });
    cursor = Math.max(cursor, end);
  }

  if (cursor < trim.end) keep.push({ start: cursor, end: trim.end });
  return keep.filter((range) => range.end - range.start > 0.05);
}

/**
 * Position dans le montage final correspondant à un instant de la source.
 *
 * Les fondus se règlent sur la vidéo produite, pas sur l'originale : sans cette
 * conversion, l'aperçu les afficherait au mauvais moment dès qu'un passage est
 * retiré.
 */
export function outputTime(keep: Range[], source: number): number {
  let elapsed = 0;
  for (const range of keep) {
    if (source < range.start) break;
    if (source <= range.end) return elapsed + (source - range.start);
    elapsed += range.end - range.start;
  }
  return elapsed;
}

/**
 * Silences à retirer : assez longs pour valoir une coupe, ramenés dans les
 * bornes du montage et amputés d'une marge de confort.
 */
export function silenceCuts(silences: Range[], trim: Range, minimum: number): Range[] {
  return silences
    .map((silence) => ({
      start: Math.max(silence.start + SILENCE_PADDING, trim.start),
      end: Math.min(silence.end - SILENCE_PADDING, trim.end),
    }))
    .filter((range) => range.end - range.start >= minimum);
}

/**
 * Réglages d'un carton. Ouverture et fermeture se règlent exactement pareil :
 * un seul formulaire sert aux deux.
 */
function CardFields({
  card,
  onChange,
  placeholder,
  children,
}: {
  card: Card;
  onChange: (card: Card) => void;
  placeholder: string;
  children?: React.ReactNode;
}) {
  return (
    <>
      <input
        className="setting-input"
        placeholder={placeholder}
        value={card.text}
        onChange={(event) => onChange({ ...card, text: event.target.value })}
      />
      <input
        className="setting-input"
        placeholder="Sous-titre (facultatif)"
        value={card.subtitle}
        onChange={(event) => onChange({ ...card, subtitle: event.target.value })}
      />
      <div className="rec-field">
        <label>Fond</label>
        <div className="backdrop-row">
          {BACKDROPS.map((option) => (
            <button
              key={option.id}
              className={`backdrop-choice backdrop-${option.id} ${
                card.backdrop === option.id ? 'active' : ''
              }`}
              onClick={() => onChange({ ...card, backdrop: option.id })}
            >
              <span className="backdrop-swatch" />
              <span>{option.label}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="rec-field">
        <label>Durée — {card.duration} s</label>
        <input
          type="range"
          min="1"
          max="8"
          value={card.duration}
          onChange={(event) => onChange({ ...card, duration: Number(event.target.value) })}
          className="setting-slider"
        />
      </div>
      {children}
    </>
  );
}

/**
 * Montage d'un enregistrement : découpe sur une ligne de temps illustrée,
 * traitement du son, vitesse, recadrage, cartons et export.
 */
export function VideoEditor({ path, filename, onClose, onExported }: VideoEditorProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const startedOutside = useRef(false);

  const [source, setSource] = useState<string | null>(null);
  const [info, setInfo] = useState<MediaSummary | null>(null);
  const [position, setPosition] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Analyse du fichier : forme d'onde, silences, bande d'images
  const [analysis, setAnalysis] = useState<AudioAnalysis | null>(null);
  const [frames, setFrames] = useState<FilmFrame[]>([]);
  const [analyzing, setAnalyzing] = useState(true);

  // Découpe, avec son historique
  const [history, setHistory] = useState<{ stack: Cuts[]; index: number }>({
    stack: [{ trim: { start: 0, end: 0 }, removed: [], muted: [] }],
    index: 0,
  });
  const [draft, setDraft] = useState<Cuts | null>(null);
  const [selected, setSelected] = useState<Selection | null>(null);
  const [tool, setTool] = useState<RegionKind>('cut');

  const committed = history.stack[history.index];
  const cuts = draft ?? committed;
  const { trim, removed, muted } = cuts;

  // Aperçu
  const [previewEdits, setPreviewEdits] = useState(true);
  const [titleCard, setTitleCard] = useState(false);
  const titleTimer = useRef<number | null>(null);

  // Réduction de bruit
  const [denoiseOn, setDenoiseOn] = useState(false);
  const [noiseRange, setNoiseRange] = useState<Range | null>(null);
  const [noiseFloor, setNoiseFloor] = useState<number | null>(null);
  const [noiseStrength, setNoiseStrength] = useState(25);
  const [measuring, setMeasuring] = useState(false);

  // Niveau sonore
  const [gain, setGain] = useState(0);
  const [normalize, setNormalize] = useState(false);

  // Cartons d'ouverture et de fermeture
  const [title, setTitle] = useState<Card>(EMPTY_CARD);
  const [outro, setOutro] = useState<Card>({ ...EMPTY_CARD, backdrop: 'slate', duration: 2 });

  // Fondus
  const [fadeIn, setFadeIn] = useState(0);
  const [fadeOut, setFadeOut] = useState(0);

  // Vitesse et recadrage
  const [speed, setSpeed] = useState(1);
  const [cropOn, setCropOn] = useState(false);
  const [crop, setCrop] = useState<Crop | null>(null);
  const [ratio, setRatio] = useState<string>('free');

  // Export
  const [asGif, setAsGif] = useState(false);
  const [gifFps, setGifFps] = useState(12);
  const [gifWidth, setGifWidth] = useState(640);

  // Découpe exacte
  const [precise, setPrecise] = useState(false);

  // Export
  const [exporting, setExporting] = useState(false);
  const [progress, setProgress] = useState(0);
  const [result, setResult] = useState<EditOutcome | null>(null);

  const duration = info?.duration ?? 0;
  const fps = info?.fps ?? 30;
  const keep = useMemo(() => computeKeep(trim, removed), [trim, removed]);
  const keptDuration = keep.reduce((total, range) => total + (range.end - range.start), 0);

  // --- Historique ---

  const commit = useCallback((next: Cuts) => {
    setDraft(null);
    setHistory((previous) => {
      const stack = [...previous.stack.slice(0, previous.index + 1), next];
      const overflow = Math.max(0, stack.length - HISTORY_LIMIT);
      return { stack: stack.slice(overflow), index: stack.length - 1 - overflow };
    });
  }, []);

  const undo = useCallback(() => {
    setDraft(null);
    setSelected(null);
    setHistory((previous) => ({ ...previous, index: Math.max(0, previous.index - 1) }));
  }, []);

  const redo = useCallback(() => {
    setDraft(null);
    setSelected(null);
    setHistory((previous) => ({
      ...previous,
      index: Math.min(previous.stack.length - 1, previous.index + 1),
    }));
  }, []);

  /** Met à jour une partie de la découpe, en brouillon ou pour de bon */
  const change = useCallback(
    (patch: Partial<Cuts>, persist: boolean) => {
      const next = { ...cuts, ...patch };
      if (persist) commit(next);
      else setDraft(next);
    },
    [cuts, commit],
  );

  const setTrim = useCallback(
    (range: Range, persist: boolean) => change({ trim: range }, persist),
    [change],
  );

  const setRemoved = useCallback(
    (ranges: Range[], persist: boolean) => change({ removed: ranges }, persist),
    [change],
  );

  const setMuted = useCallback(
    (ranges: Range[], persist: boolean) => change({ muted: ranges }, persist),
    [change],
  );

  // --- Chargement ---

  // Le moteur multimédia n'accepte pas `asset://` : on passe par un blob
  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;

    (async () => {
      try {
        const [summary, response] = await Promise.all([
          invoke<MediaSummary>('media_info', { path }),
          fetch(convertFileSrc(path)),
        ]);
        if (cancelled) return;

        setInfo(summary);
        setHistory({
          stack: [{ trim: { start: 0, end: summary.duration }, removed: [], muted: [] }],
          index: 0,
        });
        // Le cadre de recadrage part de l'image entière
        setCrop({ x: 0, y: 0, width: summary.width, height: summary.height });
        setGifWidth(Math.min(640, summary.width));

        const blob = await response.blob();
        if (cancelled) return;
        objectUrl = URL.createObjectURL(blob);
        setSource(objectUrl);
      } catch (cause) {
        if (!cancelled) setError(String(cause));
      }
    })();

    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [path]);

  // L'analyse est longue sur un fichier d'une heure : elle arrive après coup,
  // le ruban se remplit sans bloquer le reste de l'éditeur.
  useEffect(() => {
    let cancelled = false;
    setAnalyzing(true);
    setFrames([]);
    setAnalysis(null);

    (async () => {
      const both = await Promise.allSettled([
        invoke<AudioAnalysis>('analyze_audio', { path }),
        invoke<FilmFrame[]>('filmstrip', { path, count: FILM_FRAMES, height: 52 }),
      ]);
      if (cancelled) return;

      if (both[0].status === 'fulfilled') setAnalysis(both[0].value);
      if (both[1].status === 'fulfilled') setFrames(both[1].value);
      setAnalyzing(false);
    })();

    return () => {
      cancelled = true;
    };
  }, [path]);

  // Avancement du rendu
  useEffect(() => {
    const pending = listen<number>('edit-progress', (event) => setProgress(event.payload));
    return () => {
      pending.then((off) => off());
    };
  }, []);

  // --- Lecture ---

  const seek = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    video.currentTime = Math.min(Math.max(seconds, 0), video.duration || 0);
    setPosition(video.currentTime);
  }, []);

  const stopTitleCard = useCallback(() => {
    if (titleTimer.current !== null) {
      window.clearTimeout(titleTimer.current);
      titleTimer.current = null;
    }
    setTitleCard(false);
  }, []);

  useEffect(() => stopTitleCard, [stopTitleCard]);

  /** Montre le carton de titre, puis enchaîne sur la vidéo */
  const playTitleCard = useCallback(() => {
    setTitleCard(true);
    videoRef.current?.pause();
    titleTimer.current = window.setTimeout(() => {
      titleTimer.current = null;
      setTitleCard(false);
      void videoRef.current?.play().catch(() => undefined);
    }, title.duration * 1000);
  }, [title.duration]);

  // L'accélération se voit directement à la lecture
  useEffect(() => {
    if (videoRef.current) videoRef.current.playbackRate = speed;
  }, [speed, source]);

  const togglePlay = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;

    if (titleCard) {
      stopTitleCard();
      return;
    }

    if (!video.paused) {
      video.pause();
      return;
    }

    // Reprendre après la fin du montage relance depuis le début
    if (previewEdits && video.currentTime >= trim.end - 0.05) seek(trim.start);

    // Le montage s'ouvre sur son carton : l'aperçu le montre aussi
    if (previewEdits && title.on && video.currentTime <= trim.start + 0.05) {
      playTitleCard();
      return;
    }

    void video.play().catch(() => undefined);
  }, [titleCard, stopTitleCard, previewEdits, trim, title.on, seek, playTitleCard]);

  /**
   * Saute ce qui ne fait pas partie du montage. C'est ce qui permet de juger
   * le résultat avant un rendu de plusieurs minutes.
   */
  const onTimeUpdate = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;

    const time = video.currentTime;
    setPosition(time);

    if (!previewEdits) {
      video.muted = false;
      return;
    }

    // Les passages muets s'entendent — ou plutôt ne s'entendent pas — même à
    // l'arrêt sur image, pour vérifier où ils tombent.
    video.muted = muted.some((range) => time >= range.start && time < range.end);

    if (video.paused) return;

    if (time < trim.start - 0.05) {
      seek(trim.start);
      return;
    }

    if (time >= trim.end) {
      video.pause();
      seek(trim.end);
      return;
    }

    const inside = removed.find((cut) => time >= cut.start - 0.02 && time < cut.end);
    if (inside) seek(Math.min(inside.end, trim.end));
  }, [previewEdits, trim, removed, muted, seek]);

  // --- Raccourcis clavier ---

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      // Sur un bouton, la barre d'espace doit l'actionner, pas lancer la lecture
      if (event.key === ' ' && target?.tagName === 'BUTTON') return;

      const step = event.shiftKey ? 1 : 1 / fps;
      const key = event.key.toLowerCase();

      if ((event.ctrlKey || event.metaKey) && key === 'z') {
        event.preventDefault();
        if (event.shiftKey) redo();
        else undo();
        return;
      }
      if ((event.ctrlKey || event.metaKey) && key === 'y') {
        event.preventDefault();
        redo();
        return;
      }
      if (event.ctrlKey || event.metaKey || event.altKey) return;

      switch (key) {
        case ' ':
          event.preventDefault();
          togglePlay();
          break;
        case 'arrowleft':
          event.preventDefault();
          seek(position - step);
          break;
        case 'arrowright':
          event.preventDefault();
          seek(position + step);
          break;
        case 'home':
          event.preventDefault();
          seek(trim.start);
          break;
        case 'end':
          event.preventDefault();
          seek(trim.end);
          break;
        case 'i':
          event.preventDefault();
          setTrim({ start: Math.min(position, trim.end - 0.1), end: trim.end }, true);
          break;
        case 'o':
          event.preventDefault();
          setTrim({ start: trim.start, end: Math.max(position, trim.start + 0.1) }, true);
          break;
        case 'delete':
        case 'backspace':
          if (selected) {
            event.preventDefault();
            const list = selected.kind === 'cut' ? removed : muted;
            const next = list.filter((_, index) => index !== selected.index);
            if (selected.kind === 'cut') setRemoved(next, true);
            else setMuted(next, true);
            setSelected(null);
          }
          break;
        case 'm':
          // Bascule entre « retirer » et « rendre muet »
          event.preventDefault();
          setTool((previous) => (previous === 'cut' ? 'mute' : 'cut'));
          break;
        case 'escape':
          if (selected) setSelected(null);
          else onClose();
          break;
      }
    };

    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [
    fps,
    position,
    trim,
    removed,
    muted,
    selected,
    seek,
    togglePlay,
    setTrim,
    setRemoved,
    setMuted,
    undo,
    redo,
    onClose,
  ]);

  // --- Actions ---

  const suggestedCuts = useMemo(
    () => (analysis ? silenceCuts(analysis.silences, trim, SILENCE_MINIMUM) : []),
    [analysis, trim],
  );

  const removeSilences = useCallback(() => {
    // Les silences déjà couverts par une coupe existante ne sont pas redoublés
    const fresh = suggestedCuts.filter(
      (candidate) =>
        !removed.some((cut) => candidate.start >= cut.start && candidate.end <= cut.end),
    );
    if (fresh.length > 0) change({ removed: [...removed, ...fresh] }, true);
  }, [suggestedCuts, removed, change]);

  /** Mesure le bruit du passage choisi, comme un profil de bruit */
  const measureNoise = useCallback(async () => {
    if (!noiseRange) return;
    setMeasuring(true);
    try {
      const floor = await invoke<number>('measure_noise', {
        path,
        start: noiseRange.start,
        end: noiseRange.end,
      });
      setNoiseFloor(floor);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setMeasuring(false);
    }
  }, [noiseRange, path]);

  /** Le silence le plus long fait un profil de bruit tout trouvé */
  const pickNoiseSample = useCallback(() => {
    const longest = [...(analysis?.silences ?? [])].sort(
      (a, b) => b.end - b.start - (a.end - a.start),
    )[0];
    if (!longest) return;
    const sample = { start: longest.start, end: Math.min(longest.start + 1.5, longest.end) };
    setNoiseRange(sample);
    seek(sample.start);
  }, [analysis, seek]);

  const trimmed = Math.abs(keptDuration - duration) > 0.1;

  /** Le recadrage ne compte que s'il retire vraiment quelque chose */
  const cropping =
    cropOn &&
    crop !== null &&
    info !== null &&
    (crop.width < info.width || crop.height < info.height);

  const audioTouched = muted.length > 0 || normalize || Math.abs(gain) >= 0.1;

  const hasChanges =
    keep.length > 0 &&
    (trimmed ||
      denoiseOn ||
      audioTouched ||
      title.on ||
      outro.on ||
      fadeIn > 0 ||
      fadeOut > 0 ||
      speed !== 1 ||
      cropping ||
      asGif);

  const cardPlan = useCallback(
    (card: Card, fallback: string) =>
      card.on
        ? {
            text: card.text.trim() || fallback,
            subtitle: card.subtitle.trim(),
            backdrop: card.backdrop,
            duration: card.duration,
          }
        : null,
    [],
  );

  const exportVideo = useCallback(async () => {
    setExporting(true);
    setProgress(0);
    setError(null);
    setResult(null);

    try {
      const outcome = await invoke<EditOutcome>('apply_edit', {
        plan: {
          source: path,
          // Inutile de transmettre la découpe si l'on garde tout
          keep: trimmed ? keep : [],
          precise: precise && trimmed,
          denoise:
            denoiseOn && noiseRange
              ? { sample: noiseRange, strength: noiseStrength }
              : null,
          audio: audioTouched
            ? { mute: muted, gain_db: normalize ? 0 : gain, normalize }
            : null,
          title: cardPlan(title, filename),
          outro: cardPlan(outro, 'Merci'),
          fade_in: fadeIn,
          fade_out: fadeOut,
          speed,
          crop: cropping ? crop : null,
          gif: asGif ? { fps: gifFps, width: gifWidth } : null,
        },
      });
      setResult(outcome);
      onExported?.(outcome);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setExporting(false);
    }
  }, [
    path,
    keep,
    trimmed,
    precise,
    denoiseOn,
    noiseRange,
    noiseStrength,
    audioTouched,
    muted,
    gain,
    normalize,
    cardPlan,
    title,
    outro,
    fadeIn,
    fadeOut,
    speed,
    cropping,
    crop,
    asGif,
    gifFps,
    gifWidth,
    filename,
    onExported,
  ]);

  // Voile des fondus, calculé sur la durée du montage et non de la source
  const fadeOpacity = useMemo(() => {
    if (!previewEdits || (fadeIn <= 0 && fadeOut <= 0)) return 0;
    const time = outputTime(keep, position);
    const opening = fadeIn > 0 ? 1 - Math.min(time / fadeIn, 1) : 0;
    const closing =
      fadeOut > 0 ? 1 - Math.min((keptDuration - time) / fadeOut, 1) : 0;
    return Math.max(0, Math.min(1, Math.max(opening, closing)));
  }, [previewEdits, fadeIn, fadeOut, keep, position, keptDuration]);

  const renderCost =
    title.on ||
    outro.on ||
    fadeIn > 0 ||
    fadeOut > 0 ||
    speed !== 1 ||
    cropping ||
    asGif ||
    (precise && trimmed);

  /** Durée du montage produit, cartons compris et vitesse appliquée */
  const finalDuration =
    keptDuration / speed + (title.on ? title.duration : 0) + (outro.on ? outro.duration : 0);

  return (
    // Un glissement commencé sur le ruban et relâché à côté de la fenêtre
    // produit un clic dont la cible est le fond : sans mémoriser où le geste a
    // commencé, poser un repère un peu vivement fermait l'éditeur.
    <div
      className="editor-backdrop"
      onPointerDown={(event) => {
        startedOutside.current = event.target === event.currentTarget;
      }}
      onClick={(event) => {
        if (startedOutside.current && event.target === event.currentTarget) onClose();
      }}
    >
      <div className="veditor">
        <header className="veditor-header">
          <div>
            <h2>Montage</h2>
            <p title={filename}>{filename}</p>
          </div>

          <div className="veditor-history">
            <button
              className="icon-btn"
              onClick={undo}
              disabled={history.index === 0}
              title="Annuler (Ctrl+Z)"
            >
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none">
                <path
                  d="M9 14L4 9l5-5M4 9h9a7 7 0 010 14h-3"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
            <button
              className="icon-btn"
              onClick={redo}
              disabled={history.index >= history.stack.length - 1}
              title="Rétablir (Ctrl+Maj+Z)"
            >
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none">
                <path
                  d="M15 14l5-5-5-5m5 5h-9a7 7 0 000 14h3"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
          </div>

          <button className="icon-btn" onClick={onClose} title="Fermer">
            <svg viewBox="0 0 24 24" width="18" height="18">
              <path d="M18 6L6 18M6 6L18 18" stroke="currentColor" strokeWidth="2" />
            </svg>
          </button>
        </header>

        <div className="veditor-body">
          {/* Aperçu et ligne de temps */}
          <div className="veditor-preview">
            <div className="veditor-stage">
              {source ? (
                // Le cadre de recadrage doit se superposer exactement à l'image
                // et non au plateau : il vit donc dans un conteneur ajusté à la
                // vidéo elle-même.
                <div className="veditor-frame">
                  <video
                    ref={videoRef}
                    src={source}
                    onClick={togglePlay}
                    onPlay={() => setPlaying(true)}
                    onPause={() => setPlaying(false)}
                    onTimeUpdate={onTimeUpdate}
                    onSeeked={onTimeUpdate}
                  />
                  {fadeOpacity > 0 && (
                    <div className="veditor-fade" style={{ opacity: fadeOpacity }} />
                  )}
                  {titleCard && (
                    <div className={`veditor-titlecard backdrop-${title.backdrop}`}>
                      <strong>{title.text.trim() || filename}</strong>
                      {title.subtitle.trim() && <span>{title.subtitle}</span>}
                    </div>
                  )}
                  {cropOn && crop && info && (
                    <CropBox
                      frame={{ width: info.width, height: info.height }}
                      value={crop}
                      ratio={RATIOS.find((option) => option.id === ratio)?.value ?? null}
                      onChange={setCrop}
                    />
                  )}
                </div>
              ) : (
                <div className="veditor-loading">
                  <span className="spinner" />
                  <span>Chargement…</span>
                </div>
              )}
            </div>

            <div className="veditor-transport">
              <button className="player-play" onClick={togglePlay}>
                {playing || titleCard ? (
                  <svg viewBox="0 0 24 24" width="18" height="18">
                    <path d="M8 5V19M16 5V19" stroke="currentColor" strokeWidth="2.4" />
                  </svg>
                ) : (
                  <svg viewBox="0 0 24 24" width="18" height="18">
                    <path d="M8 5L19 12L8 19V5Z" fill="currentColor" />
                  </svg>
                )}
              </button>
              <span className="player-time">{formatTime(position)}</span>
              <span className="veditor-sep">/</span>
              <span className="player-time">{formatTime(duration)}</span>

              <label className="veditor-toggle" title="Sauter les passages retirés à la lecture">
                <input
                  type="checkbox"
                  checked={previewEdits}
                  onChange={(event) => setPreviewEdits(event.target.checked)}
                />
                <span>Aperçu du montage</span>
              </label>

              <span className="veditor-final">
                Durée finale <strong>{formatTime(finalDuration)}</strong>
                {keep.length > 1 && ` · ${keep.length} passages`}
              </span>
            </div>

            <Timeline
              duration={duration}
              fps={fps}
              position={position}
              trim={trim}
              removed={removed}
              muted={muted}
              tool={tool}
              selected={selected}
              peaks={analysis?.peaks ?? []}
              bucket={analysis?.bucket ?? 0.05}
              silences={analysis?.silences ?? []}
              frames={frames}
              analyzing={analyzing}
              onSeek={seek}
              onTrim={setTrim}
              onRemoved={setRemoved}
              onMuted={setMuted}
              onSelect={setSelected}
            />

            {/* Ce que `Maj` + glisser trace sur le ruban */}
            <div className="veditor-tools">
              <span className="rec-muted small">Outil</span>
              <div className="veditor-segmented">
                <button
                  className={tool === 'cut' ? 'active' : ''}
                  onClick={() => setTool('cut')}
                  title="Touche M pour basculer"
                >
                  Retirer
                </button>
                <button
                  className={tool === 'mute' ? 'active' : ''}
                  onClick={() => setTool('mute')}
                  disabled={!info?.has_audio}
                  title={
                    info?.has_audio
                      ? 'Garder l’image, couper le son (touche M)'
                      : 'Pas de piste sonore'
                  }
                >
                  Rendre muet
                </button>
              </div>
              <button
                className="btn-secondary btn-small"
                onClick={() =>
                  setMuted(
                    [...muted, { start: position, end: Math.min(position + 2, duration) }],
                    true,
                  )
                }
                disabled={!info?.has_audio}
              >
                Muet à partir d'ici
              </button>
            </div>

            <div className="veditor-marks">
              <button
                className="btn-secondary btn-small"
                onClick={() => setTrim({ start: Math.min(position, trim.end - 0.1), end: trim.end }, true)}
                title="Touche I"
              >
                Début ici
              </button>
              <button
                className="btn-secondary btn-small"
                onClick={() => setTrim({ start: trim.start, end: Math.max(position, trim.start + 0.1) }, true)}
                title="Touche O"
              >
                Fin ici
              </button>
              <button
                className="btn-secondary btn-small"
                onClick={removeSilences}
                disabled={suggestedCuts.length === 0}
                title={
                  suggestedCuts.length > 0
                    ? `${suggestedCuts.length} blanc(s) de plus de ${SILENCE_MINIMUM} s`
                    : 'Aucun blanc assez long'
                }
              >
                Retirer les blancs
                {suggestedCuts.length > 0 && ` (${suggestedCuts.length})`}
              </button>
              {removed.length > 0 && (
                <button className="btn-ghost btn-small" onClick={() => setRemoved([], true)}>
                  Tout rétablir
                </button>
              )}
            </div>
          </div>

          {/* Réglages */}
          <div className="veditor-options">
            <section className="rec-section">
              <h3>Découpe</h3>
              <div className="veditor-summary">
                <span>
                  Début <strong>{formatTime(trim.start)}</strong>
                </span>
                <span>
                  Fin <strong>{formatTime(trim.end)}</strong>
                </span>
                <span>
                  Retirés <strong>{removed.length}</strong>
                </span>
                {muted.length > 0 && (
                  <span>
                    Muets <strong>{muted.length}</strong>
                  </span>
                )}
              </div>

              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={precise}
                  onChange={(event) => setPrecise(event.target.checked)}
                  disabled={!trimmed}
                />
                <span>Couper exactement à l'image</span>
              </label>
              <p className="rec-muted small">
                {precise
                  ? "Les passages conservés sont réencodés : la coupe tombe pile où vous l'avez posée."
                  : "Par copie de flux, la coupe glisse jusqu'à l'image-clé la plus proche — jusqu'à une seconde d'écart."}
              </p>
            </section>

            <section className="rec-section">
              <h3>Son</h3>
              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={denoiseOn}
                  onChange={(e) => setDenoiseOn(e.target.checked)}
                  disabled={!info?.has_audio}
                />
                <span>
                  Réduire le bruit de fond
                  {!info?.has_audio && <em className="rec-muted"> — pas de piste sonore</em>}
                </span>
              </label>

              {denoiseOn && (
                <>
                  <p className="rec-muted small">
                    Désignez un passage <strong>silencieux</strong> (souffle seul), puis relevez-en
                    le profil : c'est ce bruit-là qui sera retiré de toute la piste.
                  </p>
                  <div className="rec-row">
                    <button
                      className="btn-secondary btn-small"
                      onClick={() =>
                        setNoiseRange({ start: position, end: Math.min(position + 1.5, duration) })
                      }
                    >
                      Marquer le silence ici
                    </button>
                    <button
                      className="btn-secondary btn-small"
                      onClick={pickNoiseSample}
                      disabled={(analysis?.silences.length ?? 0) === 0}
                      title="Utilise le plus long blanc repéré"
                    >
                      Le trouver pour moi
                    </button>
                  </div>

                  {noiseRange && (
                    <div className="rec-row">
                      <span className="rec-badge">
                        {formatTime(noiseRange.start)} → {formatTime(noiseRange.end)}
                      </span>
                      <button
                        className="btn-secondary btn-small"
                        onClick={measureNoise}
                        disabled={measuring}
                      >
                        {measuring ? 'Analyse…' : 'Relever le profil'}
                      </button>
                      {noiseFloor !== null && (
                        <span className="rec-muted small">
                          Bruit mesuré : {noiseFloor.toFixed(1)} dB
                        </span>
                      )}
                    </div>
                  )}

                  <div className="rec-field">
                    <label>Intensité — {noiseStrength}</label>
                    <input
                      type="range"
                      min="5"
                      max="60"
                      value={noiseStrength}
                      onChange={(e) => setNoiseStrength(Number(e.target.value))}
                      className="setting-slider"
                    />
                  </div>
                </>
              )}

              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={normalize}
                  onChange={(event) => setNormalize(event.target.checked)}
                  disabled={!info?.has_audio}
                />
                <span>Normaliser le niveau</span>
              </label>

              {normalize ? (
                <p className="rec-muted small">
                  La piste est ramenée au niveau attendu des plateformes de diffusion
                  (−16 LUFS). C'est la normalisation qui fixe alors le volume.
                </p>
              ) : (
                <div className="rec-field">
                  <label>
                    Volume — {gain > 0 ? '+' : ''}
                    {gain.toFixed(1)} dB
                  </label>
                  <input
                    type="range"
                    min="-12"
                    max="12"
                    step="0.5"
                    value={gain}
                    onChange={(event) => setGain(Number(event.target.value))}
                    className="setting-slider"
                    disabled={!info?.has_audio}
                  />
                </div>
              )}
            </section>

            <section className="rec-section">
              <h3>Image</h3>

              <div className="rec-field">
                <label>Vitesse</label>
                <div className="veditor-choices">
                  {SPEEDS.map((value) => (
                    <button
                      key={value}
                      className={`veditor-choice ${speed === value ? 'active' : ''}`}
                      onClick={() => setSpeed(value)}
                    >
                      ×{value}
                    </button>
                  ))}
                </div>
              </div>
              {speed !== 1 && (
                <p className="rec-muted small">
                  La voix garde sa hauteur : c'est le rythme qui change, pas le timbre.
                </p>
              )}

              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={cropOn}
                  onChange={(event) => setCropOn(event.target.checked)}
                />
                <span>Recadrer</span>
              </label>

              {cropOn && crop && info && (
                <>
                  <div className="rec-field">
                    <label>Proportions</label>
                    <div className="veditor-choices">
                      {RATIOS.map((option) => (
                        <button
                          key={option.id}
                          className={`veditor-choice ${ratio === option.id ? 'active' : ''}`}
                          onClick={() => {
                            setRatio(option.id);
                            setCrop((previous) =>
                              previous
                                ? fit(
                                    previous,
                                    { width: info.width, height: info.height },
                                    option.value,
                                  )
                                : previous,
                            );
                          }}
                        >
                          {option.label}
                        </button>
                      ))}
                    </div>
                  </div>
                  <div className="rec-row">
                    <span className="rec-muted small">
                      {crop.width} × {crop.height} px sur {info.width} × {info.height}
                    </span>
                    <button
                      className="btn-ghost btn-small"
                      onClick={() =>
                        setCrop({ x: 0, y: 0, width: info.width, height: info.height })
                      }
                    >
                      Toute l'image
                    </button>
                  </div>
                </>
              )}
            </section>

            <section className="rec-section">
              <h3>Titre</h3>
              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={title.on}
                  onChange={(event) => setTitle({ ...title, on: event.target.checked })}
                />
                <span>Carton d'ouverture</span>
              </label>

              {title.on && (
                <CardFields card={title} onChange={setTitle} placeholder="Titre">
                  <button className="btn-secondary btn-small" onClick={playTitleCard}>
                    Voir le carton
                  </button>
                </CardFields>
              )}

              <label className="switch-row">
                <input
                  type="checkbox"
                  checked={outro.on}
                  onChange={(event) => setOutro({ ...outro, on: event.target.checked })}
                />
                <span>Carton de fin</span>
              </label>

              {outro.on && (
                <CardFields card={outro} onChange={setOutro} placeholder="Merci" />
              )}
            </section>

            <section className="rec-section">
              <h3>Fondus</h3>
              <div className="rec-field">
                <label>Ouverture — {fadeIn.toFixed(1)} s</label>
                <input
                  type="range"
                  min="0"
                  max="3"
                  step="0.1"
                  value={fadeIn}
                  onChange={(e) => setFadeIn(Number(e.target.value))}
                  className="setting-slider"
                />
              </div>
              <div className="rec-field">
                <label>Fermeture — {fadeOut.toFixed(1)} s</label>
                <input
                  type="range"
                  min="0"
                  max="3"
                  step="0.1"
                  value={fadeOut}
                  onChange={(e) => setFadeOut(Number(e.target.value))}
                  className="setting-slider"
                />
              </div>
            </section>

            <section className="rec-section">
              <h3>Export</h3>
              <div className="veditor-segmented">
                <button className={asGif ? '' : 'active'} onClick={() => setAsGif(false)}>
                  Vidéo MP4
                </button>
                <button className={asGif ? 'active' : ''} onClick={() => setAsGif(true)}>
                  GIF animé
                </button>
              </div>

              {asGif && info && (
                <>
                  <p className="rec-muted small">
                    Un GIF n'a pas de son et pèse lourd : réservez-le aux passages courts.
                  </p>
                  <div className="rec-field">
                    <label>Cadence — {gifFps} images/s</label>
                    <input
                      type="range"
                      min="5"
                      max="24"
                      value={gifFps}
                      onChange={(event) => setGifFps(Number(event.target.value))}
                      className="setting-slider"
                    />
                  </div>
                  <div className="rec-field">
                    <label>Largeur — {gifWidth} px</label>
                    <input
                      type="range"
                      min="240"
                      max={Math.max(240, info.width)}
                      step="20"
                      value={gifWidth}
                      onChange={(event) => setGifWidth(Number(event.target.value))}
                      className="setting-slider"
                    />
                  </div>
                </>
              )}
            </section>
          </div>
        </div>

        <footer className="veditor-footer">
          {error && <span className="veditor-error">{error}</span>}

          {result ? (
            <span className="veditor-done">
              ✓ {result.filename} · {formatTime(result.duration_ms / 1000)} ·{' '}
              {formatSize(result.size_bytes)}
              {result.lossless && ' · sans réencodage'}
            </span>
          ) : (
            <span className="rec-muted small">
              {/* Le coût du rendu dépend entièrement de ce qui est demandé */}
              {asGif
                ? 'Le GIF se construit après le montage : comptez une passe de plus.'
                : renderCost
                  ? 'Réencodage de l’image nécessaire : comptez quelques minutes.'
                  : denoiseOn || audioTouched
                    ? "Seule la piste sonore sera réencodée : l'image reste intacte."
                    : 'Découpe sans réencodage : quasi instantané, sans perte de qualité.'}
            </span>
          )}

          {exporting && (
            <div className="veditor-progress">
              <div className="veditor-progress-bar" style={{ width: `${progress}%` }} />
              <span>{progress} %</span>
            </div>
          )}

          <button
            className="btn-record"
            onClick={exportVideo}
            disabled={exporting || !hasChanges}
            title={hasChanges ? undefined : 'Aucune modification à appliquer'}
          >
            {exporting ? 'Rendu…' : 'Exporter le montage'}
          </button>
        </footer>
      </div>
    </div>
  );
}
