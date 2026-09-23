import { useCallback, useEffect, useRef, useState } from 'react';
import './CropBox.css';

export interface Crop {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface CropBoxProps {
  /** Dimensions de la vidéo d'origine, en pixels */
  frame: { width: number; height: number };
  value: Crop;
  onChange: (crop: Crop) => void;
  /** Proportions imposées (largeur / hauteur), ou `null` pour un cadre libre */
  ratio: number | null;
}

/** Poignée saisie, ou déplacement de tout le cadre */
type Grip = 'move' | 'nw' | 'ne' | 'sw' | 'se' | 'n' | 's' | 'w' | 'e';

const CORNERS: Grip[] = ['nw', 'ne', 'sw', 'se'];
const EDGES: Grip[] = ['n', 's', 'w', 'e'];

/** Un cadre plus petit ne serait plus manipulable, ni encodable */
const MINIMUM = 32;

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(value, low), high);
}

/**
 * Ramène le cadre dans l'image, en respectant les proportions demandées.
 *
 * Le calcul se fait toujours sur le cadre entier plutôt que sur le seul bord
 * déplacé : c'est ce qui empêche de sortir de l'image en poussant une poignée.
 */
export function fit(crop: Crop, frame: { width: number; height: number }, ratio: number | null): Crop {
  let width = clamp(crop.width, MINIMUM, frame.width);
  let height = clamp(crop.height, MINIMUM, frame.height);

  if (ratio) {
    // On part de la largeur, puis on rattrape si la hauteur ne tient pas
    height = width / ratio;
    if (height > frame.height) {
      height = frame.height;
      width = height * ratio;
    }
  }

  return {
    width: Math.round(width),
    height: Math.round(height),
    x: Math.round(clamp(crop.x, 0, frame.width - width)),
    y: Math.round(clamp(crop.y, 0, frame.height - height)),
  };
}

/**
 * Cadre de recadrage posé sur l'aperçu : on le déplace et on le redimensionne
 * directement sur l'image, plutôt que de saisir quatre nombres.
 */
export function CropBox({ frame, value, onChange, ratio }: CropBoxProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [grip, setGrip] = useState<Grip | null>(null);
  const origin = useRef<{ pointer: { x: number; y: number }; crop: Crop } | null>(null);

  const begin = useCallback(
    (event: React.PointerEvent, handle: Grip) => {
      event.preventDefault();
      event.stopPropagation();
      origin.current = { pointer: { x: event.clientX, y: event.clientY }, crop: value };
      setGrip(handle);
    },
    [value],
  );

  useEffect(() => {
    if (!grip) return;

    const move = (event: PointerEvent) => {
      const host = hostRef.current;
      const start = origin.current;
      if (!host || !start) return;

      // Le déplacement se convertit des pixels affichés vers ceux de la vidéo
      const bounds = host.getBoundingClientRect();
      const scale = frame.width / Math.max(bounds.width, 1);
      const dx = (event.clientX - start.pointer.x) * scale;
      const dy = (event.clientY - start.pointer.y) * scale;

      const from = start.crop;

      if (grip === 'move') {
        onChange({
          ...from,
          x: Math.round(clamp(from.x + dx, 0, frame.width - from.width)),
          y: Math.round(clamp(from.y + dy, 0, frame.height - from.height)),
        });
        return;
      }

      // Bords libres : chacun se déplace sans emporter le côté opposé
      let { x, y, width, height } = from;

      if (grip.includes('w')) {
        const right = from.x + from.width;
        x = clamp(from.x + dx, 0, right - MINIMUM);
        width = right - x;
      }
      if (grip.includes('e')) {
        width = clamp(from.width + dx, MINIMUM, frame.width - from.x);
      }
      if (grip.includes('n')) {
        const bottom = from.y + from.height;
        y = clamp(from.y + dy, 0, bottom - MINIMUM);
        height = bottom - y;
      }
      if (grip.includes('s')) {
        height = clamp(from.height + dy, MINIMUM, frame.height - from.y);
      }

      onChange(fit({ x, y, width, height }, frame, ratio));
    };

    const end = () => {
      origin.current = null;
      setGrip(null);
    };

    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', end);
    window.addEventListener('pointercancel', end);

    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', end);
      window.removeEventListener('pointercancel', end);
    };
  }, [grip, frame, ratio, onChange]);

  const percent = (part: number, whole: number) => `${(part / whole) * 100}%`;

  return (
    <div className={`cropbox ${grip ? 'is-dragging' : ''}`} ref={hostRef}>
      {/* Ce qui est écarté s'assombrit : l'ombre portée du cadre déborde
          largement, et le `overflow: hidden` du plateau la découpe. */}
      <div
        className="cropbox-frame"
        style={{
          left: percent(value.x, frame.width),
          top: percent(value.y, frame.height),
          width: percent(value.width, frame.width),
          height: percent(value.height, frame.height),
        }}
        onPointerDown={(event) => begin(event, 'move')}
      >
        <span className="cropbox-size">
          {value.width} × {value.height}
        </span>

        {/* Les proportions imposées rendent les bords inutiles : seuls les
            coins conservent alors un sens. */}
        {(ratio ? CORNERS : [...CORNERS, ...EDGES]).map((handle) => (
          <span
            key={handle}
            className={`cropbox-grip cropbox-${handle}`}
            onPointerDown={(event) => begin(event, handle)}
          />
        ))}
      </div>
    </div>
  );
}
