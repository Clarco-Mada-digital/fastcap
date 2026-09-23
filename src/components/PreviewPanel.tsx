import { useCallback, useEffect, useRef, useState } from 'react';
import type { MouseEvent, ReactNode } from 'react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { invoke } from '@tauri-apps/api/core';
import type {
  AnnotationTool,
  AnnotationToolId,
  AppSettings,
  CaptureResult,
} from '../hooks/useApp';
import { useOcr, useExport, useCaptureHistory } from '../hooks/useApp';
import './PreviewPanel.css';

const FONT_STACK =
  "'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Ubuntu, sans-serif";

interface EditorProps {
  capture: CaptureResult;
  settings: AppSettings | null;
  onClose: () => void;
  onCaptureNew: () => void;
  /** Appelé après une sauvegarde réussie, pour rafraîchir vignettes/historique */
  onSaved?: (id: string) => void;
}

interface Snapshot {
  url: string;
  width: number;
  height: number;
  annotations: AnnotationTool[];
}

interface Point {
  x: number;
  y: number;
}

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

type DragState =
  | { kind: 'shape'; start: Point; end: Point }
  | { kind: 'pen'; start: Point; end: Point; points: Point[] }
  | {
      kind: 'move';
      id: string;
      origX: number;
      origY: number;
      grabDX: number;
      grabDY: number;
    }
  | { kind: 'crop'; start: Point; end: Point }
  | null;

interface ToolDef {
  id: AnnotationToolId;
  label: string;
  icon: ReactNode;
}

const SHAPE_TOOLS: AnnotationToolId[] = ['rectangle', 'ellipse', 'line', 'arrow', 'blur', 'mosaic'];

function makeId(): string {
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

function timeStamp(): string {
  return new Date().toISOString().replace(/[:.]/g, '-');
}

const TOOLS: ToolDef[] = [
  {
    id: 'select',
    label: 'Sélectionner',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
        <path d="M5 3L18 10L12 11.5L14.5 18L11.5 19L9 12.5L5 15V3Z" strokeLinejoin="round" />
      </svg>
    ),
  },
  {
    id: 'pen',
    label: 'Stylo',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 20C8 18 15.5 13 19 9.5C20.5 8 20 6.5 18.5 5.5C17 4.5 15.5 4 14 5.5C10.5 9 5.5 16 4 20Z" />
      </svg>
    ),
  },
  {
    id: 'highlight',
    label: 'Surligner',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 17L14 7L18 11L8 21H4V17Z" />
        <path d="M12 9L16 13" />
      </svg>
    ),
  },
  {
    id: 'rectangle',
    label: 'Rectangle',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
        <rect x="3.5" y="5" width="17" height="14" rx="1.5" />
      </svg>
    ),
  },
  {
    id: 'ellipse',
    label: 'Ellipse',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
        <ellipse cx="12" cy="12" rx="9" ry="6.5" />
      </svg>
    ),
  },
  {
    id: 'line',
    label: 'Ligne',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
        <path d="M4 20L20 4" />
      </svg>
    ),
  },
  {
    id: 'arrow',
    label: 'Flèche',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M5 19L19 5M19 5H11M19 5V13" />
      </svg>
    ),
  },
  {
    id: 'text',
    label: 'Texte',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
        <path d="M5 6V5H19V6M12 5V19M10 19H14" />
      </svg>
    ),
  },
  {
    id: 'number',
    label: 'Numéro',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
        <circle cx="12" cy="12" r="8.5" />
        <path d="M10 8V16M14 8V16M9 11H15" strokeLinecap="round" />
      </svg>
    ),
  },
  {
    id: 'blur',
    label: 'Flouter',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
        <path d="M12 3.5V20.5M12 3.5C16.4 5.2 19 8.5 19 12.5C19 16.5 16.4 19.8 12 20.5M12 3.5C7.6 5.2 5 8.5 5 12.5C5 16.5 7.6 19.8 12 20.5" strokeLinecap="round" />
      </svg>
    ),
  },
  {
    id: 'mosaic',
    label: 'Mosaïque',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.4">
        <rect x="3" y="3" width="8" height="8" fill="currentColor" fillOpacity="0.35" />
        <rect x="13" y="3" width="8" height="8" />
        <rect x="3" y="13" width="8" height="8" />
        <rect x="13" y="13" width="8" height="8" fill="currentColor" fillOpacity="0.35" />
      </svg>
    ),
  },
  {
    id: 'crop',
    label: 'Recadrer',
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinejoin="round">
        <path d="M7 3V17H21" strokeLinecap="round" />
        <path d="M3 7H17V21" strokeLinecap="round" strokeDasharray="3 3" />
      </svg>
    ),
  },
];

/** Distance d'un point à un segment */
function distToSegment(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(px - ax, py - ay);
  let t = ((px - ax) * dx + (py - ay) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

function inBbox(x: number, y: number, r: Rect, pad: number): boolean {
  return x >= r.x - pad && x <= r.x + r.width + pad && y >= r.y - pad && y <= r.y + r.height + pad;
}

export function PreviewPanel({ capture, settings, onClose, onCaptureNew, onSaved }: EditorProps) {
  const { recognize: runOcr, isProcessing: isOcrProcessing, ocrResult, ocrError } = useOcr();
  const { exportImage, copyToClipboard } = useExport();
  const { deleteCapture } = useCaptureHistory();

  // ---- Image de base ----
  const [baseUrl, setBaseUrl] = useState(capture.data_url);
  const [imgWidth, setImgWidth] = useState(capture.width);
  const [imgHeight, setImgHeight] = useState(capture.height);
  const [baseTick, setBaseTick] = useState(0);
  const baseImageRef = useRef<HTMLImageElement | null>(null);

  useEffect(() => {
    const img = new Image();
    img.onload = () => {
      baseImageRef.current = img;
      setBaseTick((t) => t + 1);
    };
    img.src = baseUrl;
  }, [baseUrl]);

  // ---- Annotations + historique ----
  const [annotations, setAnnotations] = useState<AnnotationTool[]>([]);
  const [past, setPast] = useState<Snapshot[]>([]);
  const [future, setFuture] = useState<Snapshot[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [numberSeq, setNumberSeq] = useState(1);

  const snapshot = useCallback(
    (): Snapshot => ({ url: baseUrl, width: imgWidth, height: imgHeight, annotations }),
    [baseUrl, imgWidth, imgHeight, annotations]
  );

  const applySnapshot = useCallback((s: Snapshot) => {
    setBaseUrl(s.url);
    setImgWidth(s.width);
    setImgHeight(s.height);
    setAnnotations(s.annotations);
    setSelectedId(null);
    setCropRect(null);
    setTextDraft(null);
    setDrag(null);
  }, []);

  const pushHistory = useCallback(() => {
    setPast((p) => [...p, snapshot()].slice(-80));
    setFuture([]);
  }, [snapshot]);

  const undo = useCallback(() => {
    if (past.length === 0) return;
    const prev = past[past.length - 1];
    setFuture((f) => [...f, snapshot()]);
    setPast((p) => p.slice(0, -1));
    applySnapshot(prev);
  }, [past, snapshot, applySnapshot]);

  const redo = useCallback(() => {
    if (future.length === 0) return;
    const next = future[future.length - 1];
    setPast((p) => [...p, snapshot()]);
    setFuture((f) => f.slice(0, -1));
    applySnapshot(next);
  }, [future, snapshot, applySnapshot]);

  const commitAnnotation = useCallback(
    (ann: AnnotationTool) => {
      pushHistory();
      setAnnotations((a) => [...a, ann]);
      setSelectedId(ann.id);
    },
    [pushHistory]
  );

  // ---- Outils ----
  const [tool, setTool] = useState<AnnotationToolId>('select');
  const [color, setColor] = useState('#22d3ee');
  const [thickness, setThickness] = useState(4);
  const [fontSize, setFontSize] = useState(26);
  const [filled, setFilled] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [drag, setDrag] = useState<DragState>(null);
  const [textDraft, setTextDraft] = useState<{ pos: Point; value: string } | null>(null);

  // Le brouillon est aussi tenu dans une référence : cliquer ailleurs ouvre un
  // nouveau brouillon *avant* que le `blur` du précédent ne se déclenche, si
  // bien que la validation lisait un état déjà vidé — le texte saisi était
  // alors silencieusement perdu.
  const textDraftRef = useRef<{ pos: Point; value: string } | null>(null);
  useEffect(() => {
    textDraftRef.current = textDraft;
  }, [textDraft]);
  const [cropRect, setCropRect] = useState<Rect | null>(null);

  // ---- Panneaux latéraux ----
  const [panel, setPanel] = useState<'ocr' | 'export' | null>(null);
  const [isExporting, setIsExporting] = useState(false);
  const [exportedPath, setExportedPath] = useState<string | null>(null);
  const defaultFormat = settings?.default_export_format === 'jpeg' ? 'jpeg' : 'png';
  const defaultQuality = settings?.default_export_quality ?? 95;

  // ---- Références ----
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);

  const toImageCoords = useCallback(
    (clientX: number, clientY: number): Point => {
      const wrap = wrapRef.current;
      const rect = wrap?.getBoundingClientRect();
      if (!rect || rect.width === 0) {
        return { x: 0, y: 0 };
      }
      return {
        x: (clientX - rect.left) / zoom,
        y: (clientY - rect.top) / zoom,
      };
    },
    [zoom]
  );

  // ---- Dessin ----
  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    const base = baseImageRef.current;
    if (!canvas || !ctx || !base) return;

    canvas.width = imgWidth;
    canvas.height = imgHeight;
    ctx.clearRect(0, 0, imgWidth, imgHeight);
    ctx.drawImage(base, 0, 0, imgWidth, imgHeight);

    const drawShape = (ann: AnnotationTool) => {
      ctx.save();
      ctx.strokeStyle = ann.color;
      ctx.fillStyle = ann.color;
      ctx.lineWidth = Math.max(1, ann.thickness);
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';

      switch (ann.tool) {
        case 'pen':
        case 'highlight': {
          ctx.beginPath();
          const pts = ann.points ?? [];
          if (pts.length === 0) {
            ctx.arc(ann.x, ann.y, Math.max(ann.thickness / 2, 2), 0, Math.PI * 2);
            ctx.fill();
            return;
          }
          if (ann.tool === 'highlight') ctx.globalAlpha = 0.42;
          ctx.moveTo(pts[0].x, pts[0].y);
          for (let i = 1; i < pts.length; i++) {
            ctx.lineTo(pts[i].x, pts[i].y);
          }
          ctx.stroke();
          ctx.restore();
          return;
        }
        case 'rectangle': {
          if (ann.filled) {
            ctx.fillRect(ann.x, ann.y, ann.width, ann.height);
          } else {
            ctx.strokeRect(ann.x, ann.y, ann.width, ann.height);
          }
          break;
        }
        case 'ellipse': {
          ctx.beginPath();
          ctx.ellipse(
            ann.x + ann.width / 2,
            ann.y + ann.height / 2,
            Math.abs(ann.width) / 2,
            Math.abs(ann.height) / 2,
            0,
            0,
            Math.PI * 2
          );
          if (ann.filled) ctx.fill();
          else ctx.stroke();
          break;
        }
        case 'line':
        case 'arrow': {
          const ax = ann.x;
          const ay = ann.y;
          const bx = ann.x + ann.width;
          const by = ann.y + ann.height;
          ctx.beginPath();
          ctx.moveTo(ax, ay);
          ctx.lineTo(bx, by);
          ctx.stroke();
          if (ann.tool === 'arrow') {
            const ang = Math.atan2(by - ay, bx - ax);
            const head = Math.max(ann.thickness * 3, 14);
            ctx.beginPath();
            ctx.moveTo(bx, by);
            ctx.lineTo(bx - head * Math.cos(ang - 0.45), by - head * Math.sin(ang - 0.45));
            ctx.moveTo(bx, by);
            ctx.lineTo(bx - head * Math.cos(ang + 0.45), by - head * Math.sin(ang + 0.45));
            ctx.stroke();
          }
          break;
        }
        case 'blur': {
          const radius = Math.min(Math.max(Math.min(ann.width, ann.height) / 10, 4), 48);
          ctx.filter = `blur(${radius}px)`;
          ctx.drawImage(base, ann.x, ann.y, ann.width, ann.height, ann.x, ann.y, ann.width, ann.height);
          break;
        }
        case 'mosaic': {
          const cell = Math.max(Math.min(ann.thickness * 2, 28), 5);
          const tmp = document.createElement('canvas');
          tmp.width = cell;
          tmp.height = cell;
          const tctx = tmp.getContext('2d');
          if (tctx) {
            tctx.imageSmoothingEnabled = true;
            tctx.drawImage(base, ann.x, ann.y, ann.width, ann.height, 0, 0, cell, cell);
            ctx.save();
            ctx.imageSmoothingEnabled = false;
            ctx.drawImage(tmp, 0, 0, cell, cell, ann.x, ann.y, ann.width, ann.height);
            ctx.restore();
          }
          break;
        }
        case 'text': {
          ctx.font = `600 ${ann.fontSize ?? fontSize}px ${FONT_STACK}`;
          ctx.textBaseline = 'top';
          ctx.lineWidth = Math.max(3, (ann.fontSize ?? fontSize) / 8);
          ctx.strokeStyle = 'rgba(255,255,255,0.92)';
          ctx.strokeText(ann.text ?? '', ann.x, ann.y);
          ctx.fillStyle = ann.color;
          ctx.fillText(ann.text ?? '', ann.x, ann.y);
          break;
        }
        case 'number': {
          const radius = (ann.fontSize ?? fontSize) * 0.72;
          const cx = ann.x + radius;
          const cy = ann.y + radius;
          ctx.beginPath();
          ctx.arc(cx, cy, radius, 0, Math.PI * 2);
          ctx.fillStyle = ann.color;
          ctx.fill();
          ctx.lineWidth = 3;
          ctx.strokeStyle = 'rgba(255,255,255,0.85)';
          ctx.stroke();
          ctx.fillStyle = '#ffffff';
          ctx.font = `700 ${Math.max(radius * 1.05, 12)}px ${FONT_STACK}`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillText(ann.text ?? '1', cx, cy + 1);
          ctx.restore();
          return;
        }
      }
      ctx.restore();
    };

    for (const ann of annotations) drawShape(ann);

    // Aperçu de dessin en cours
    if (drag && drag.kind === 'pen' && drag.points.length > 0) {
      ctx.save();
      ctx.strokeStyle = color;
      ctx.globalAlpha = tool === 'highlight' ? 0.42 : 1;
      ctx.lineWidth = Math.max(1, thickness);
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';
      ctx.beginPath();
      ctx.moveTo(drag.points[0].x, drag.points[0].y);
      for (let i = 1; i < drag.points.length; i++) ctx.lineTo(drag.points[i].x, drag.points[i].y);
      ctx.stroke();
      ctx.restore();
    } else if (drag && drag.kind === 'shape') {
      const rect = {
        x: Math.min(drag.start.x, drag.end.x),
        y: Math.min(drag.start.y, drag.end.y),
        width: Math.abs(drag.end.x - drag.start.x),
        height: Math.abs(drag.end.y - drag.start.y),
      };
      const previewAnn: AnnotationTool = {
        id: 'preview',
        tool: tool as AnnotationTool['tool'],
        x: tool === 'line' || tool === 'arrow' ? drag.start.x : rect.x,
        y: tool === 'line' || tool === 'arrow' ? drag.start.y : rect.y,
        width: tool === 'line' || tool === 'arrow' ? drag.end.x - drag.start.x : rect.width,
        height: tool === 'line' || tool === 'arrow' ? drag.end.y - drag.start.y : rect.height,
        color,
        thickness,
        filled: filled && (tool === 'rectangle' || tool === 'ellipse'),
      };
      drawShape(previewAnn);
    }

    // Cadre de sélection
    const selected = annotations.find((a) => a.id === selectedId);
    if (selected) {
      const pad = 4;
      const box = {
        x: Math.min(selected.x, selected.x + selected.width) - pad,
        y: Math.min(selected.y, selected.y + selected.height) - pad,
        width: Math.max(Math.abs(selected.width), 2) + pad * 2,
        height: Math.max(Math.abs(selected.height), 2) + pad * 2,
      };
      ctx.save();
      ctx.strokeStyle = color;
      ctx.setLineDash([6, 4]);
      ctx.lineWidth = 1.5;
      ctx.strokeRect(box.x, box.y, box.width, box.height);
      ctx.setLineDash([]);
      ctx.fillStyle = 'rgba(255,255,255,0.12)';
      ctx.fillRect(box.x, box.y, box.width, box.height);
      ctx.restore();
    }
  }, [
    baseTick,
    imgWidth,
    imgHeight,
    annotations,
    selectedId,
    drag,
    tool,
    color,
    thickness,
    filled,
    fontSize,
  ]);

  // ---- Interaction souris ----
  const pointInAnnotation = useCallback(
    (ann: AnnotationTool, pos: Point): boolean => {
      const tol = Math.max(ann.thickness * 2, 8);
      switch (ann.tool) {
        case 'pen':
        case 'highlight': {
          const pts = ann.points ?? [];
          if (pts.length === 0) return Math.hypot(pos.x - ann.x, pos.y - ann.y) <= tol;
          for (let i = 1; i < pts.length; i++) {
            if (distToSegment(pos.x, pos.y, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= tol) {
              return true;
            }
          }
          return false;
        }
        case 'line':
        case 'arrow':
          return distToSegment(pos.x, pos.y, ann.x, ann.y, ann.x + ann.width, ann.y + ann.height) <= tol;
        case 'text': {
          const w = Math.max(ann.width, 20);
          return inBbox(pos.x, pos.y, { x: ann.x, y: ann.y, width: w, height: ann.fontSize ?? 24 }, 6);
        }
        case 'number': {
          const r = (ann.fontSize ?? fontSize) * 0.72;
          return Math.hypot(pos.x - (ann.x + r), pos.y - (ann.y + r)) <= r;
        }
        default:
          return inBbox(pos.x, pos.y, { x: ann.x, y: ann.y, width: Math.abs(ann.width), height: Math.abs(ann.height) }, 8);
      }
    },
    [fontSize]
  );

  const handleMouseDown = useCallback(
    (e: MouseEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      const pos = toImageCoords(e.clientX, e.clientY);
      if (pos.x < 0 || pos.y < 0 || pos.x > imgWidth || pos.y > imgHeight) return;

      if (tool === 'select') {
        const found = [...annotations].reverse().find((a) => pointInAnnotation(a, pos));
        if (found) {
          setSelectedId(found.id);
          setDrag({
            kind: 'move',
            id: found.id,
            origX: found.x,
            origY: found.y,
            grabDX: found.x - pos.x,
            grabDY: found.y - pos.y,
          });
        } else {
          setSelectedId(null);
        }
        return;
      }

      if (tool === 'crop') {
        setSelectedId(null);
        setDrag({ kind: 'crop', start: pos, end: pos });
        return;
      }

      if (tool === 'text') {
        // Valider la saisie en cours avant d'en ouvrir une nouvelle
        if (textDraftRef.current) commitText();
        textDraftRef.current = { pos, value: '' };
        setTextDraft({ pos, value: '' });
        return;
      }

      if (tool === 'number') {
        const radius = fontSize * 0.72;
        const ann: AnnotationTool = {
          id: makeId(),
          tool: 'number',
          x: pos.x - radius,
          y: pos.y - radius,
          width: radius * 2,
          height: radius * 2,
          color,
          thickness: 1,
          text: String(numberSeq),
          fontSize,
        };
        setNumberSeq((n) => n + 1);
        commitAnnotation(ann);
        return;
      }

      if (tool === 'pen' || tool === 'highlight') {
        setDrag({ kind: 'pen', start: pos, end: pos, points: [pos] });
        return;
      }

      if (SHAPE_TOOLS.includes(tool)) {
        setDrag({ kind: 'shape', start: pos, end: pos });
        return;
      }
    },
    [toImageCoords, tool, annotations, pointInAnnotation, fontSize, color, numberSeq, commitAnnotation, imgWidth, imgHeight]
  );

  const handleMouseMove = useCallback(
    (e: MouseEvent<HTMLDivElement>) => {
      if (!drag) return;
      const pos = toImageCoords(e.clientX, e.clientY);

      if (drag.kind === 'pen') {
        setDrag({ ...drag, end: pos, points: [...drag.points, pos] });
        return;
      }
      if (drag.kind === 'shape' || drag.kind === 'crop') {
        setDrag({ ...drag, end: pos });
        return;
      }
      if (drag.kind === 'move') {
        const nx = pos.x + drag.grabDX;
        const ny = pos.y + drag.grabDY;
        const dx = nx - drag.origX;
        const dy = ny - drag.origY;
        setAnnotations((arr) =>
          arr.map((a) => {
            if (a.id !== drag.id) return a;
            const points = a.points ? a.points.map((p) => ({ x: p.x + dx, y: p.y + dy })) : undefined;
            return { ...a, x: nx, y: ny, points };
          })
        );
        return;
      }
    },
    [drag, toImageCoords]
  );

  const handleMouseUp = useCallback(() => {
    if (!drag) return;

    if (drag.kind === 'move') {
      pushHistory();
      setDrag(null);
      return;
    }

    if (drag.kind === 'crop') {
      const rect = {
        x: Math.min(drag.start.x, drag.end.x),
        y: Math.min(drag.start.y, drag.end.y),
        width: Math.abs(drag.end.x - drag.start.x),
        height: Math.abs(drag.end.y - drag.start.y),
      };
      if (rect.width >= 4 && rect.height >= 4) {
        setCropRect(rect);
      }
      setDrag(null);
      return;
    }

    if (drag.kind === 'pen') {
      const ann: AnnotationTool = {
        id: makeId(),
        tool: tool as AnnotationTool['tool'],
        x: drag.points[0]?.x ?? drag.start.x,
        y: drag.points[0]?.y ?? drag.start.y,
        width: Math.abs(drag.end.x - drag.start.x),
        height: Math.abs(drag.end.y - drag.start.y),
        color,
        thickness: tool === 'highlight' ? Math.max(thickness, 14) : thickness,
        points: drag.points,
      };
      commitAnnotation(ann);
      setDrag(null);
      return;
    }

    if (drag.kind === 'shape') {
      const minX = Math.min(drag.start.x, drag.end.x);
      const minY = Math.min(drag.start.y, drag.end.y);
      const dir = tool === 'line' || tool === 'arrow';
      const ann: AnnotationTool = {
        id: makeId(),
        tool: tool as AnnotationTool['tool'],
        x: dir ? drag.start.x : minX,
        y: dir ? drag.start.y : minY,
        width: dir ? drag.end.x - drag.start.x : Math.abs(drag.end.x - drag.start.x),
        height: dir ? drag.end.y - drag.start.y : Math.abs(drag.end.y - drag.start.y),
        color,
        thickness,
        filled: filled && (tool === 'rectangle' || tool === 'ellipse'),
      };
      if (Math.abs(ann.width) < 1 && Math.abs(ann.height) < 1) {
        setDrag(null);
        return;
      }
      commitAnnotation(ann);
      setDrag(null);
    }
  }, [drag, tool, color, thickness, filled, commitAnnotation, pushHistory]);

  const clearDrag = useCallback(() => {
    if (drag && drag.kind === 'move') pushHistory();
    setDrag(null);
  }, [drag, pushHistory]);

  // ---- Texte ----
  const commitText = useCallback(() => {
    const draft = textDraftRef.current;
    const value = draft?.value.trim() ?? '';
    if (!draft || !value) {
      textDraftRef.current = null;
      setTextDraft(null);
      return;
    }

    // Largeur mesurée avec la police de rendu, pour que la zone cliquable de
    // l'annotation corresponde au texte affiché.
    const ctx = canvasRef.current?.getContext('2d');
    let width = value.length * fontSize * 0.6;
    if (ctx) {
      const previous = ctx.font;
      ctx.font = `600 ${fontSize}px ${FONT_STACK}`;
      width = ctx.measureText(value).width;
      ctx.font = previous;
    }

    textDraftRef.current = null;
    commitAnnotation({
      id: makeId(),
      tool: 'text',
      x: draft.pos.x,
      y: draft.pos.y,
      width,
      height: fontSize,
      color,
      thickness: 1,
      text: value,
      fontSize,
    });
    setTextDraft(null);
  }, [fontSize, color, commitAnnotation]);

  // ---- Recadrage ----
  const applyCrop = useCallback(() => {
    if (!cropRect || cropRect.width < 3 || cropRect.height < 3) return;
    const src = canvasRef.current;
    if (!src) return;

    const rx = Math.floor(cropRect.x);
    const ry = Math.floor(cropRect.y);
    const rw = Math.max(Math.floor(cropRect.width), 2);
    const rh = Math.max(Math.floor(cropRect.height), 2);

    const tmp = document.createElement('canvas');
    tmp.width = rw;
    tmp.height = rh;
    const tctx = tmp.getContext('2d');
    if (!tctx) return;

    tctx.fillStyle = '#000';
    tctx.fillRect(0, 0, tmp.width, tmp.height);
    tctx.drawImage(src, rx, ry, rw, rh, 0, 0, rw, rh);

    pushHistory();
    setBaseUrl(tmp.toDataURL('image/png'));
    setImgWidth(rw);
    setImgHeight(rh);
    setAnnotations([]);
    setCropRect(null);
    setSelectedId(null);
  }, [cropRect, pushHistory]);

  const cancelCrop = useCallback(() => {
    setCropRect(null);
    setDrag(null);
    setTool('select');
  }, []);

  // ---- Clavier ----
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable) return;

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z' && !e.shiftKey) {
        e.preventDefault();
        undo();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'y' || (e.key.toLowerCase() === 'z' && e.shiftKey))) {
        e.preventDefault();
        redo();
        return;
      }
      if (e.key === 'Delete' || e.key === 'Backspace') {
        if (selectedId) {
          const sel = annotations.find((a) => a.id === selectedId);
          if (sel) {
            pushHistory();
            setAnnotations((arr) => arr.filter((a) => a.id !== selectedId));
            setSelectedId(null);
          }
        }
        return;
      }
      if (e.key === 'Escape') {
        if (drag) {
          setDrag(null);
        } else if (textDraft) {
          setTextDraft(null);
        } else if (cropRect) {
          cancelCrop();
        } else {
          setSelectedId(null);
        }
      }
      if (e.key === 'Enter' && tool === 'crop' && cropRect) applyCrop();
      if (e.key === 'Enter' && tool === 'text' && textDraft) commitText();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [undo, redo, selectedId, annotations, pushHistory, drag, textDraft, cropRect, cancelCrop, tool, applyCrop, commitText]);

  // ---- Zoom ----
  const zoomIn = useCallback(() => setZoom((z) => Math.min(z * 1.25, 8)), []);
  const zoomOut = useCallback(() => setZoom((z) => Math.max(z / 1.25, 0.05)), []);

  useEffect(() => {
    const fit = () => {
      const stage = stageRef.current;
      if (!stage) return;
      const avail = Math.min(stage.clientWidth - 48, stage.clientHeight - 48);
      if (avail <= 0) return;
      const scale = Math.min(avail / imgWidth, avail / imgHeight);
      setZoom(Math.min(scale, 6));
    };
    fit();
    window.addEventListener('resize', fit);
    return () => window.removeEventListener('resize', fit);
  }, [imgWidth, imgHeight, baseTick]);

  // ---- Export / copie / OCR (WYSIWYG depuis le canvas) ----
  const composeDataUrl = useCallback((): string => {
    return canvasRef.current?.toDataURL('image/png') ?? baseUrl;
  }, [baseUrl]);

  const handleExport = useCallback(
    async (format: 'png' | 'jpeg') => {
      setIsExporting(true);
      try {
        const dataUrl = composeDataUrl();
        const path = await exportImage(dataUrl, {
          format,
          quality: defaultQuality,
          filename: `capture_${timeStamp()}.${format === 'jpeg' ? 'jpg' : 'png'}`,
        });
        setExportedPath(path);
        setPanel('export');
      } catch (err) {
        console.error("Erreur d'export:", err);
      } finally {
        setIsExporting(false);
      }
    },
    [composeDataUrl, exportImage, defaultQuality]
  );

  const handleCopy = useCallback(async () => {
    try {
      await copyToClipboard(composeDataUrl());
    } catch (err) {
      console.error('Erreur copie presse-papiers:', err);
    }
  }, [composeDataUrl, copyToClipboard]);

  // ---- Sauvegarde des modifications dans la capture (écrase le fichier) ----
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  const handleSave = useCallback(async () => {
    if (saving) return;
    setSaving(true);
    try {
      const record = await invoke<{ path: string; width: number; height: number }>(
        'save_capture_edit',
        { id: capture.id, dataUrl: composeDataUrl() }
      );
      setExportedPath(record.path);
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
      onSaved?.(capture.id);
    } catch (err) {
      console.error("Erreur d'enregistrement:", err);
    } finally {
      setSaving(false);
    }
  }, [saving, capture.id, composeDataUrl, onSaved]);

  const handleOcr = useCallback(async () => {
    setPanel('ocr');
    try {
      await runOcr(composeDataUrl());
    } catch (err) {
      console.error('Erreur OCR:', err);
    }
  }, [composeDataUrl, runOcr]);

  const handleDeleteCapture = useCallback(async () => {
    try {
      await deleteCapture(capture.id);
      onClose();
    } catch (err) {
      console.error('Erreur suppression:', err);
    }
  }, [capture.id, deleteCapture, onClose]);

  const copyOcrText = useCallback(async () => {
    if (ocrResult?.text) {
      try {
        await navigator.clipboard.writeText(ocrResult.text);
      } catch {
        /* presse-papiers non disponible */
      }
    }
  }, [ocrResult]);

  const showToolOptions =
    tool === 'pen' || tool === 'highlight' || SHAPE_TOOLS.includes(tool) || tool === 'text' || tool === 'number';

  // Zone de recadrage affichée (cours de dessin ou persistée)
  const displayCrop: Rect | null = drag?.kind === 'crop'
    ? {
        x: Math.min(drag.start.x, drag.end.x),
        y: Math.min(drag.start.y, drag.end.y),
        width: Math.abs(drag.end.x - drag.start.x),
        height: Math.abs(drag.end.y - drag.start.y),
      }
    : cropRect;

  const cursor = tool === 'select' ? 'default' : tool === 'text' ? 'text' : tool === 'crop' ? 'crosshair' : 'crosshair';
  const canUndo = past.length > 0;
  const canRedo = future.length > 0;

  return (
    <div className="editor-panel">
      {/* Barre supérieure */}
      <div className="editor-topbar">
        <div className="editor-fileinfo">
          <span className="editor-title">Éditeur d'image</span>
          <span className="editor-dims">
            {imgWidth} × {imgHeight}
          </span>
          <span className="editor-format">{capture.format.toUpperCase()}</span>
        </div>
        <div className="editor-topactions">
          <button className="editor-btn" onClick={handleCopy} title="Copier dans le presse-papiers">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
              <rect x="9" y="9" width="11" height="11" rx="2" />
              <path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1" />
            </svg>
            Copier
          </button>
          <button className="editor-btn" onClick={handleOcr} title="Reconnaissance de texte">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
              <path d="M4 6V4a2 2 0 012-2h12a2 2 0 012 2v2M4 6h16M4 6v12a2 2 0 002 2h3" />
              <circle cx="18" cy="15" r="3.5" />
              <path d="M20.5 20.5L18 18" />
            </svg>
            OCR
          </button>
          <button
            className={`editor-btn editor-btn-primary ${saved ? 'editor-btn-saved' : ''}`}
            onClick={handleSave}
            disabled={saving}
            title="Enregistrer les modifications"
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
              <path d="M19 21H5a2 2 0 01-2-2V5a2 2 0 012-2h11l5 5v11a2 2 0 01-2 2z" strokeLinejoin="round" />
              <path d="M17 21v-8H7v8M7 3v5h8" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
            {saving ? 'Enregistrement…' : saved ? 'Enregistré ✓' : 'Enregistrer'}
          </button>
          <button className="editor-btn" onClick={onClose} title="Accepter et fermer">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
              <path d="M5 12L10 17L19 8" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
            Terminé
          </button>
          <button className="editor-btn editor-btn-danger" onClick={handleDeleteCapture} title="Supprimer la capture">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
              <path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2M10 11v6M14 11v6M6 6v14a2 2 0 002 2h8a2 2 0 002-2V6" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      </div>

      {/* Barre d'outils */}
      <div className="editor-toolbar">
        <div className="editor-tools">
          {TOOLS.map((t) => (
            <button
              key={t.id}
              className={`editor-tool-btn ${tool === t.id ? 'active' : ''}`}
              onClick={() => {
                setTool(t.id);
                if (t.id !== 'text') setTextDraft(null);
                setCropRect(null);
                setDrag(null);
              }}
              title={t.label}
            >
              {t.icon}
            </button>
          ))}
        </div>

        {showToolOptions && (
          <div className="editor-tooloptions">
            <label className="editor-opt">
              <span>Couleur</span>
              <input
                type="color"
                value={color}
                onChange={(e) => setColor(e.target.value)}
                className="editor-color"
              />
            </label>

            {(tool === 'pen' ||
              tool === 'highlight' ||
              tool === 'rectangle' ||
              tool === 'ellipse' ||
              tool === 'line' ||
              tool === 'arrow' ||
              tool === 'mosaic') && (
              <label className="editor-opt">
                <span>Epaisseur</span>
                <input
                  type="range"
                  min="2"
                  max="40"
                  value={thickness}
                  onChange={(e) => setThickness(Number(e.target.value))}
                />
                <b>{thickness}px</b>
              </label>
            )}

            {(tool === 'text' || tool === 'number') && (
              <label className="editor-opt">
                <span>Taille</span>
                <input
                  type="range"
                  min="12"
                  max="120"
                  value={fontSize}
                  onChange={(e) => setFontSize(Number(e.target.value))}
                />
                <b>{fontSize}px</b>
              </label>
            )}

            {(tool === 'rectangle' || tool === 'ellipse') && (
              <label className="editor-opt editor-check">
                <input type="checkbox" checked={filled} onChange={(e) => setFilled(e.target.checked)} />
                <span>Rempli</span>
              </label>
            )}
          </div>
        )}

        <div className="editor-toolops">
          <button className="editor-ico-btn" onClick={undo} disabled={!canUndo} title="Annuler (Ctrl+Z)">
            <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <path d="M3 7v6h6M3 13a9 9 0 103-7" />
            </svg>
          </button>
          <button className="editor-ico-btn" onClick={redo} disabled={!canRedo} title="Rétablir (Ctrl+Y)">
            <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21 7v6h-6M21 13a9 9 0 00-14.9-5.8M21 13l-5-5" />
            </svg>
          </button>
          <button
            className="editor-ico-btn"
            onClick={() => {
              pushHistory();
              setAnnotations([]);
              setSelectedId(null);
            }}
            disabled={annotations.length === 0}
            title="Tout effacer"
          >
            <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 3v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1" />
            </svg>
          </button>
        </div>
      </div>

      {/* Scène */}
      <div className="editor-stage" ref={stageRef}>
        <div
          className="editor-wrap"
          ref={wrapRef}
          style={{ width: imgWidth * zoom, height: imgHeight * zoom, cursor }}
          onMouseDown={handleMouseDown}
          onMouseMove={handleMouseMove}
          onMouseUp={handleMouseUp}
          onMouseLeave={clearDrag}
          onContextMenu={(e) => {
            e.preventDefault();
            setDrag(null);
            if (drag?.kind === 'move') pushHistory();
          }}
        >
          <canvas ref={canvasRef} style={{ width: '100%', height: '100%' }} />

          {/* Masque de recadrage */}
          {displayCrop && (
            <>
              <div className="editor-crop-dim" style={{ left: 0, top: 0, right: 0, height: displayCrop.y * zoom }} />
              <div className="editor-crop-dim" style={{ left: 0, top: (displayCrop.y + displayCrop.height) * zoom, right: 0, bottom: 0 }} />
              <div className="editor-crop-dim" style={{ left: 0, top: displayCrop.y * zoom, width: displayCrop.x * zoom, height: displayCrop.height * zoom }} />
              <div className="editor-crop-dim" style={{ left: (displayCrop.x + displayCrop.width) * zoom, top: displayCrop.y * zoom, right: 0, height: displayCrop.height * zoom }} />
            </>
          )}
          {displayCrop && (
            <div
              className={`editor-crop-frame ${tool === 'crop' ? 'dragging' : ''}`}
              style={{
                left: displayCrop.x * zoom,
                top: displayCrop.y * zoom,
                width: displayCrop.width * zoom,
                height: displayCrop.height * zoom,
              }}
            >
              <span className="editor-crop-size">
                {Math.round(displayCrop.width)} × {Math.round(displayCrop.height)}
              </span>
              {tool === 'crop' && (
                <div className="editor-crop-actions">
                  <button onClick={applyCrop} disabled={cropRect === null}>
                    Recadrer
                  </button>
                  <button onClick={cancelCrop}>Annuler</button>
                </div>
              )}
            </div>
          )}

          {/* Saisie de texte */}
          {textDraft && (
            <div
              className="editor-textdraft"
              style={{ left: textDraft.pos.x * zoom, top: textDraft.pos.y * zoom, fontSize }}
              onMouseDown={(e) => e.stopPropagation()}
            >
              <input
                autoFocus
                value={textDraft.value}
                placeholder="Tapez votre texte…"
                onChange={(e) => {
                  const value = e.target.value;
                  textDraftRef.current = textDraftRef.current
                    ? { ...textDraftRef.current, value }
                    : { pos: textDraft.pos, value };
                  setTextDraft((d) => (d ? { ...d, value } : d));
                }}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === 'Enter') commitText();
                  if (e.key === 'Escape') {
                    textDraftRef.current = null;
                    setTextDraft(null);
                  }
                }}
                onBlur={commitText}
                style={{ fontSize: Math.min(fontSize * zoom, 64) }}
              />
            </div>
          )}
        </div>
      </div>

      {/* Barre d'état */}
      <div className="editor-statusbar">
        <span className="editor-status-item">
          {imgWidth} × {imgHeight}px
        </span>
        <span className="editor-status-item">
          {annotations.length} {annotations.length === 1 ? 'élément' : 'éléments'}
        </span>
        <span className="editor-status-item">
          {tool === 'crop' && cropRect ? 'Entrée pour recadrer · Échap pour annuler' : tool === 'select' ? 'Cliquez et glissez pour déplacer' : 'Glissez pour dessiner'}
        </span>
        <div className="editor-zoom">
          <button onClick={zoomOut} title="Zoom arrière">−</button>
          <span>{Math.round(zoom * 100)}%</span>
          <button onClick={zoomIn} title="Zoom avant">+</button>
          <button
            onClick={() => {
              const stage = stageRef.current;
              if (stage) {
                const avail = Math.min(stage.clientWidth - 48, stage.clientHeight - 48);
                const s = Math.min(avail / imgWidth, avail / imgHeight, 6);
                setZoom(s);
              }
            }}
            title="Ajuster à la fenêtre"
          >
            Adapter
          </button>
        </div>
      </div>

      {/* Panneau OCR */}
      {panel === 'ocr' && (
        <aside className="editor-sidepanel">
          <div className="sidepanel-header">
            <h3>
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
                <path d="M4 6V4a2 2 0 012-2h12a2 2 0 012 2v2M4 6h16M4 6v12a2 2 0 002 2h3" />
                <circle cx="18" cy="15" r="3.5" />
                <path d="M20.5 20.5L18 18" />
              </svg>
              OCR
            </h3>
            <button className="sidepanel-close" onClick={() => setPanel(null)}>×</button>
          </div>
          <div className="sidepanel-body">
            <button className="editor-btn editor-btn-block" onClick={handleOcr} disabled={isOcrProcessing}>
              {isOcrProcessing ? 'Analyse…' : 'Analyser l' + 'image'}
            </button>
            {ocrError && <div className="editor-err">{ocrError}</div>}
            {ocrResult && (
              <div className="ocr-box">
                <div className="ocr-stats">
                  <span>Confiance <b>{ocrResult.confidence.toFixed(0)}%</b></span>
                  <span>Mots <b>{ocrResult.words.length}</b></span>
                </div>
                <pre>{ocrResult.text}</pre>
              </div>
            )}
            {ocrResult?.text && (
              <button className="editor-btn editor-btn-block" onClick={copyOcrText}>
                Copier le texte
              </button>
            )}
          </div>
        </aside>
      )}

      {/* Panneau export */}
      {panel === 'export' && (
        <aside className="editor-sidepanel">
          <div className="sidepanel-header">
            <h3>
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
                <path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
              Exporter
            </h3>
            <button className="sidepanel-close" onClick={() => setPanel(null)}>×</button>
          </div>
          <div className="sidepanel-body">
            <div className="export-formats">
              <button className={defaultFormat === 'png' ? 'active' : ''} onClick={() => handleExport('png')} disabled={isExporting}>
                PNG
                <small>Sans perte</small>
              </button>
              <button className={defaultFormat === 'jpeg' ? 'active' : ''} onClick={() => handleExport('jpeg')} disabled={isExporting}>
                JPEG
                <small>{defaultQuality}% qualité</small>
              </button>
            </div>
            {exportedPath && (
              <div className="export-done">
                <span className="export-ok">Fichier enregistré</span>
                <code title={exportedPath}>{exportedPath}</code>
                <button className="editor-btn editor-btn-block" onClick={() => revealItemInDir(exportedPath).catch(() => undefined)}>
                  Afficher dans le dossier
                </button>
              </div>
            )}
            <button className="editor-btn editor-btn-block editor-btn-mute" onClick={onCaptureNew}>
              Nouvelle capture
            </button>
          </div>
        </aside>
      )}
    </div>
  );
}