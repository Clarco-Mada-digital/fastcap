import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import './Timeline.css';

export interface Range {
  start: number;
  end: number;
}

export interface FilmFrame {
  time: number;
  data: string;
}

/** Les deux sortes de zones que l'on trace sur le ruban */
export type RegionKind = 'cut' | 'mute';

export interface Selection {
  kind: RegionKind;
  index: number;
}

interface TimelineProps {
  duration: number;
  fps: number;
  position: number;
  trim: Range;
  removed: Range[];
  /** Passages dont on ne garde que l'image */
  muted: Range[];
  /** Ce que `Maj` + glisser trace sur le ruban */
  tool: RegionKind;
  /** Zone sélectionnée, mise en évidence et supprimable au clavier */
  selected: Selection | null;
  peaks: number[];
  /** Durée couverte par une valeur de `peaks`, en secondes */
  bucket: number;
  silences: Range[];
  frames: FilmFrame[];
  analyzing: boolean;
  onSeek: (time: number) => void;
  /** `commit` distingue le glissement en cours de son résultat : seul ce
   *  dernier entre dans l'historique d'annulation. */
  onTrim: (range: Range, commit: boolean) => void;
  onRemoved: (ranges: Range[], commit: boolean) => void;
  onMuted: (ranges: Range[], commit: boolean) => void;
  onSelect: (selection: Selection | null) => void;
}

type DragKind =
  | { kind: 'scrub' }
  | { kind: 'trim-start' }
  | { kind: 'trim-end' }
  | { kind: 'region-start'; region: RegionKind; index: number }
  | { kind: 'region-end'; region: RegionKind; index: number }
  | { kind: 'region-new'; region: RegionKind; anchor: number };

/** Distance d'aimantation, en pixels */
const MAGNET = 7;

const ZOOM_MAX = 60;

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(value, low), high);
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

/**
 * Graduations lisibles : on choisit le pas le plus fin dont l'espacement reste
 * confortable à l'écran, sinon les repères se chevauchent dès qu'on dézoome.
 */
function tickStep(span: number, width: number): number {
  const candidates = [0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800];
  const minimumPixels = 64;
  return (
    candidates.find((step) => (step / span) * width >= minimumPixels) ??
    candidates[candidates.length - 1]
  );
}

/**
 * Ruban de montage : bande d'images, forme d'onde, bornes de découpe et
 * passages retirés, le tout sur une même échelle de temps que l'on peut zoomer.
 *
 * Les repères se posent à la souris **et** au clavier : un glissement pour
 * viser vite, les flèches pour ajuster à l'image près.
 */
export function Timeline({
  duration,
  fps,
  position,
  trim,
  removed,
  muted,
  tool,
  selected,
  peaks,
  bucket,
  silences,
  frames,
  analyzing,
  onSeek,
  onTrim,
  onRemoved,
  onMuted,
  onSelect,
}: TimelineProps) {
  const laneRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const [zoom, setZoom] = useState(1);
  const [scroll, setScroll] = useState(0);
  const [width, setWidth] = useState(0);
  const [drag, setDrag] = useState<DragKind | null>(null);
  const [hover, setHover] = useState<number | null>(null);

  const span = duration > 0 ? duration / zoom : 1;
  const start = clamp(scroll, 0, Math.max(0, duration - span));

  // Largeur réelle du ruban : elle détermine l'aimantation et le tracé
  useEffect(() => {
    const lane = laneRef.current;
    if (!lane) return;

    const observer = new ResizeObserver((entries) => {
      setWidth(entries[0].contentRect.width);
    });
    observer.observe(lane);
    setWidth(lane.getBoundingClientRect().width);

    return () => observer.disconnect();
  }, []);

  const timeAt = useCallback(
    (clientX: number): number => {
      const lane = laneRef.current;
      if (!lane) return 0;
      const bounds = lane.getBoundingClientRect();
      const ratio = (clientX - bounds.left) / Math.max(bounds.width, 1);
      return clamp(start + ratio * span, 0, duration);
    },
    [start, span, duration],
  );

  const xOf = useCallback(
    (time: number): number => ((time - start) / span) * 100,
    [start, span],
  );

  /**
   * Points d'intérêt sur lesquels un repère se cale : bornes du fichier,
   * limites des silences et des passages déjà retirés. `Alt` désactive
   * l'aimantation quand on veut poser un repère exactement où l'on clique.
   */
  const magnets = useMemo(() => {
    const points = [0, duration];
    for (const silence of silences) points.push(silence.start, silence.end);
    for (const region of [...removed, ...muted]) points.push(region.start, region.end);
    return points;
  }, [duration, silences, removed, muted]);

  /** Les deux listes de zones se manipulent exactement de la même façon */
  const regions = useCallback(
    (kind: RegionKind) => (kind === 'cut' ? removed : muted),
    [removed, muted],
  );

  const emitRegions = useCallback(
    (kind: RegionKind, ranges: Range[], commit: boolean) => {
      if (kind === 'cut') onRemoved(ranges, commit);
      else onMuted(ranges, commit);
    },
    [onRemoved, onMuted],
  );

  const snap = useCallback(
    (time: number, free: boolean): number => {
      if (free || width <= 0) return time;
      const tolerance = (MAGNET / width) * span;
      let best = time;
      let distance = tolerance;
      for (const point of magnets) {
        const gap = Math.abs(point - time);
        if (gap < distance) {
          distance = gap;
          best = point;
        }
      }
      return best;
    },
    [magnets, width, span],
  );

  // --- Tracé de la forme d'onde ---

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width <= 0) return;

    const ratio = window.devicePixelRatio || 1;
    const height = canvas.clientHeight;
    canvas.width = Math.floor(width * ratio);
    canvas.height = Math.floor(height * ratio);

    const context = canvas.getContext('2d');
    if (!context) return;

    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);

    if (peaks.length === 0) return;

    const middle = height / 2;
    const gradient = context.createLinearGradient(0, 0, 0, height);
    gradient.addColorStop(0, 'rgba(56, 189, 248, 0.95)');
    gradient.addColorStop(0.5, 'rgba(34, 211, 238, 0.75)');
    gradient.addColorStop(1, 'rgba(56, 189, 248, 0.95)');
    context.fillStyle = gradient;

    // Une colonne par pixel : on retient la crête des intervalles couverts,
    // faute de quoi un dézoom effacerait les pics les plus courts.
    for (let x = 0; x < width; x += 1) {
      const from = start + (x / width) * span;
      const to = start + ((x + 1) / width) * span;
      const first = Math.floor(from / bucket);
      const last = Math.max(Math.ceil(to / bucket), first + 1);

      let peak = 0;
      for (let index = first; index < last && index < peaks.length; index += 1) {
        if (index >= 0) peak = Math.max(peak, peaks[index]);
      }

      if (peak <= 0) continue;
      // Racine carrée : les niveaux de parole ordinaires restent visibles
      // au lieu d'être écrasés contre l'axe.
      const amplitude = Math.sqrt(peak) * (middle - 1);
      context.fillRect(x, middle - amplitude, 1, amplitude * 2);
    }
  }, [peaks, bucket, start, span, width]);

  // --- Glissements ---

  const applyDrag = useCallback(
    (current: DragKind, time: number, commit: boolean) => {
      switch (current.kind) {
        case 'scrub':
          onSeek(time);
          break;
        case 'trim-start':
          onTrim({ start: Math.min(time, trim.end - 0.1), end: trim.end }, commit);
          break;
        case 'trim-end':
          onTrim({ start: trim.start, end: Math.max(time, trim.start + 0.1) }, commit);
          break;
        case 'region-start': {
          const list = regions(current.region);
          const region = list[current.index];
          if (!region) break;
          const next = [...list];
          next[current.index] = { start: Math.min(time, region.end - 0.05), end: region.end };
          emitRegions(current.region, next, commit);
          break;
        }
        case 'region-end': {
          const list = regions(current.region);
          const region = list[current.index];
          if (!region) break;
          const next = [...list];
          next[current.index] = { start: region.start, end: Math.max(time, region.start + 0.05) };
          emitRegions(current.region, next, commit);
          break;
        }
        case 'region-new': {
          const from = Math.min(current.anchor, time);
          const to = Math.max(current.anchor, time);
          // La zone naissante est toujours la dernière de la liste
          const next = [...regions(current.region)];
          next[next.length - 1] = { start: from, end: to };
          emitRegions(current.region, next, commit);
          break;
        }
      }
    },
    [onSeek, onTrim, trim, regions, emitRegions],
  );

  const beginDrag = useCallback(
    (event: React.PointerEvent, kind: DragKind) => {
      event.preventDefault();
      event.stopPropagation();
      (event.target as Element).setPointerCapture?.(event.pointerId);
      setDrag(kind);
      if (kind.kind !== 'region-new') {
        applyDrag(kind, snap(timeAt(event.clientX), event.altKey), false);
      }
    },
    [applyDrag, snap, timeAt],
  );

  useEffect(() => {
    if (!drag) return;

    const move = (event: PointerEvent) => {
      applyDrag(drag, snap(timeAt(event.clientX), event.altKey), false);
    };

    const end = (event: PointerEvent) => {
      const time = snap(timeAt(event.clientX), event.altKey);

      // Une zone tracée par mégarde (simple clic) ne doit pas rester, et son
      // retrait ne doit pas non plus encombrer l'historique d'annulation.
      if (drag.kind === 'region-new' && Math.abs(time - drag.anchor) < 0.05) {
        emitRegions(drag.region, regions(drag.region).slice(0, -1), true);
      } else {
        applyDrag(drag, time, true);
      }
      setDrag(null);
    };

    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);

    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
    };
  }, [drag, applyDrag, snap, timeAt, regions, emitRegions]);

  /** Clic simple : on déplace la lecture ; `Maj` + glissement : on trace */
  const onLanePointerDown = useCallback(
    (event: React.PointerEvent) => {
      if (event.button !== 0) return;
      const time = snap(timeAt(event.clientX), event.altKey);

      if (event.shiftKey) {
        emitRegions(tool, [...regions(tool), { start: time, end: time }], false);
        beginDrag(event, { kind: 'region-new', region: tool, anchor: time });
        return;
      }

      onSelect(null);
      beginDrag(event, { kind: 'scrub' });
    },
    [snap, timeAt, tool, regions, emitRegions, beginDrag, onSelect],
  );

  // --- Zoom ---

  const zoomAround = useCallback(
    (factor: number, focus: number) => {
      setZoom((previous) => {
        const next = clamp(previous * factor, 1, ZOOM_MAX);
        const nextSpan = duration / next;
        // Le point visé reste sous le curseur pendant le zoom
        setScroll(clamp(focus - (focus - start) * (nextSpan / span), 0, duration - nextSpan));
        return next;
      });
    },
    [duration, start, span],
  );

  // La molette est écoutée à la main, en mode non passif : sans cela `Ctrl` +
  // molette zoomerait la fenêtre entière au lieu du ruban.
  useEffect(() => {
    const lane = laneRef.current;
    if (!lane || duration <= 0) return;

    const onWheel = (event: WheelEvent) => {
      if (event.ctrlKey || event.metaKey) {
        event.preventDefault();
        zoomAround(event.deltaY < 0 ? 1.25 : 0.8, timeAt(event.clientX));
        return;
      }

      if (zoom > 1) {
        event.preventDefault();
        const step = (event.deltaY !== 0 ? event.deltaY : event.deltaX) * (span / 600);
        setScroll((previous) => clamp(previous + step, 0, duration - span));
      }
    };

    lane.addEventListener('wheel', onWheel, { passive: false });
    return () => lane.removeEventListener('wheel', onWheel);
  }, [duration, zoom, span, zoomAround, timeAt]);

  // La tête de lecture reste en vue quand on est zoomé
  useEffect(() => {
    if (zoom <= 1 || drag) return;
    if (position < start || position > start + span) {
      setScroll(clamp(position - span / 2, 0, duration - span));
    }
  }, [position, zoom, start, span, duration, drag]);

  const ticks = useMemo(() => {
    if (duration <= 0 || width <= 0) return [];
    const step = tickStep(span, width);
    const first = Math.ceil(start / step) * step;
    const list: number[] = [];
    for (let time = first; time <= start + span; time += step) list.push(time);
    return list;
  }, [duration, width, span, start]);

  const frameSpacing = frames.length > 1 ? duration / frames.length : duration;
  const visible = (range: Range) => range.end >= start && range.start <= start + span;

  return (
    <div className="tl">
      <div className="tl-head">
        <span className="tl-range">
          {formatTime(start)} → {formatTime(start + span)}
        </span>
        <div className="tl-zoom">
          <button
            className="tl-zoom-btn"
            onClick={() => zoomAround(0.66, position)}
            disabled={zoom <= 1}
            title="Dézoomer"
          >
            −
          </button>
          <span className="tl-zoom-level">×{zoom.toFixed(1)}</span>
          <button
            className="tl-zoom-btn"
            onClick={() => zoomAround(1.5, position)}
            disabled={zoom >= ZOOM_MAX}
            title="Zoomer (Ctrl + molette)"
          >
            +
          </button>
          <button
            className="tl-zoom-btn tl-zoom-reset"
            onClick={() => {
              setZoom(1);
              setScroll(0);
            }}
            disabled={zoom <= 1}
            title="Voir toute la durée"
          >
            Tout
          </button>
        </div>
      </div>

      <div
        className={`tl-lane ${drag ? 'is-dragging' : ''}`}
        ref={laneRef}
        onPointerDown={onLanePointerDown}
        onPointerMove={(event) => setHover(timeAt(event.clientX))}
        onPointerLeave={() => setHover(null)}
      >
        {/* Bande d'images */}
        <div className="tl-film">
          {frames.map((frame) => (
            <img
              key={frame.time}
              src={frame.data}
              alt=""
              draggable={false}
              style={{
                left: `${xOf(frame.time - frameSpacing / 2)}%`,
                width: `${(frameSpacing / span) * 100}%`,
              }}
            />
          ))}
          {frames.length === 0 && (
            <div className="tl-placeholder">{analyzing ? 'Analyse de la vidéo…' : ''}</div>
          )}
        </div>

        {/* Forme d'onde */}
        <div className="tl-wave">
          <canvas ref={canvasRef} />
          {peaks.length === 0 && !analyzing && <span className="tl-nosound">Pas de piste sonore</span>}
        </div>

        {/* Silences repérés, sous la forme d'onde */}
        {silences.filter(visible).map((silence) => (
          <div
            key={`sil-${silence.start}`}
            className="tl-silence"
            style={{
              left: `${xOf(silence.start)}%`,
              width: `${((silence.end - silence.start) / span) * 100}%`,
            }}
            title={`Silence · ${(silence.end - silence.start).toFixed(1)} s`}
          />
        ))}

        {/* Ce qui est écarté par les bornes de début et de fin */}
        <div className="tl-outside" style={{ left: 0, width: `${Math.max(xOf(trim.start), 0)}%` }} />
        <div
          className="tl-outside"
          style={{ left: `${Math.min(xOf(trim.end), 100)}%`, right: 0 }}
        />

        {/* Passages retirés, puis passages muets */}
        {(['cut', 'mute'] as RegionKind[]).flatMap((kind) =>
          regions(kind).map((region, index) =>
            visible(region) ? (
              <div
                key={`${kind}-${index}`}
                className={`tl-region tl-${kind} ${
                  selected?.kind === kind && selected.index === index ? 'is-selected' : ''
                }`}
                style={{
                  left: `${xOf(region.start)}%`,
                  width: `${((region.end - region.start) / span) * 100}%`,
                }}
                onPointerDown={(event) => {
                  event.stopPropagation();
                  onSelect({ kind, index });
                }}
                title={
                  kind === 'cut'
                    ? `Passage retiré · ${(region.end - region.start).toFixed(1)} s`
                    : `Passage muet · ${(region.end - region.start).toFixed(1)} s`
                }
              >
                <span
                  className="tl-region-grip tl-grip-left"
                  onPointerDown={(event) =>
                    beginDrag(event, { kind: 'region-start', region: kind, index })
                  }
                />
                <span className="tl-region-label">
                  {kind === 'cut' ? '−' : '🔇 '}
                  {(region.end - region.start).toFixed(1)} s
                </span>
                <button
                  className="tl-region-undo"
                  onPointerDown={(event) => event.stopPropagation()}
                  onClick={(event) => {
                    event.stopPropagation();
                    emitRegions(
                      kind,
                      regions(kind).filter((_, other) => other !== index),
                      true,
                    );
                    onSelect(null);
                  }}
                  title={kind === 'cut' ? 'Rétablir ce passage' : 'Rendre le son à ce passage'}
                >
                  ×
                </button>
                <span
                  className="tl-region-grip tl-grip-right"
                  onPointerDown={(event) =>
                    beginDrag(event, { kind: 'region-end', region: kind, index })
                  }
                />
              </div>
            ) : null,
          ),
        )}

        {/* Bornes de découpe */}
        <div
          className="tl-handle tl-handle-start"
          style={{ left: `${xOf(trim.start)}%` }}
          onPointerDown={(event) => beginDrag(event, { kind: 'trim-start' })}
          title="Début du montage"
        >
          <span className="tl-handle-grip" />
        </div>
        <div
          className="tl-handle tl-handle-end"
          style={{ left: `${xOf(trim.end)}%` }}
          onPointerDown={(event) => beginDrag(event, { kind: 'trim-end' })}
          title="Fin du montage"
        >
          <span className="tl-handle-grip" />
        </div>

        {/* Tête de lecture */}
        <div className="tl-playhead" style={{ left: `${xOf(position)}%` }}>
          <span className="tl-playhead-cap">{formatTime(position)}</span>
        </div>

        {hover !== null && !drag && (
          <div className="tl-hover" style={{ left: `${xOf(hover)}%` }} />
        )}
      </div>

      {/* Graduations */}
      <div className="tl-ruler">
        {ticks.map((time) => (
          <span key={time} style={{ left: `${xOf(time)}%` }}>
            {span < 4 ? `${time.toFixed(1)} s` : formatTime(time)}
          </span>
        ))}
      </div>

      <p className="tl-hint">
        Glisser pour déplacer la lecture · <kbd>Maj</kbd> + glisser pour{' '}
        {tool === 'cut' ? 'retirer un passage' : 'rendre un passage muet'} · <kbd>Ctrl</kbd> +
        molette pour zoomer · <kbd>Alt</kbd> pour ignorer l'aimantation ·{' '}
        {fps > 0 && `1 image = ${(1000 / fps).toFixed(0)} ms`}
      </p>
    </div>
  );
}
