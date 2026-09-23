import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './RegionSelector.css';

interface Selection {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Fenêtre plein écran affichant une image figée de l'écran et permettant
 * de dessiner au rectangle la région à capturer.
 */
export function RegionSelector() {
  const imageRef = useRef<HTMLImageElement>(null);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Selection | null>(null);
  const [dragging, setDragging] = useState(false);
  const [start, setStart] = useState<{ x: number; y: number } | null>(null);

  // Charger l'image figée fournie par le backend
  useEffect(() => {
    invoke<string>('get_region_selection_image')
      .then(setImageUrl)
      .catch((e) => setError(String(e)));
  }, []);

  const cancel = useCallback(() => {
    invoke('cancel_region_selection').catch(() => undefined);
  }, []);

  const confirm = useCallback(
    async (area: Selection) => {
      if (area.width < 2 || area.height < 2) {
        cancel();
        return;
      }
      try {
        await invoke('finish_region_selection', {
          x: Math.round(area.x),
          y: Math.round(area.y),
          width: Math.round(area.width),
          height: Math.round(area.height),
        });
      } catch (e) {
        setError(String(e));
      }
    },
    [cancel]
  );

  // Convertir une position souris en coordonnées de l'image capturée
  const toImageCoords = useCallback((event: React.MouseEvent): { x: number; y: number } | null => {
    const img = imageRef.current;
    if (!img) return null;

    const rect = img.getBoundingClientRect();
    const scaleX = img.naturalWidth / rect.width;
    const scaleY = img.naturalHeight / rect.height;

    return {
      x: (event.clientX - rect.left) * scaleX,
      y: (event.clientY - rect.top) * scaleY,
    };
  }, []);

  const handleMouseDown = useCallback(
    (event: React.MouseEvent) => {
      if (event.button !== 0) return;
      const pos = toImageCoords(event);
      if (!pos) return;

      setDragging(true);
      setStart(pos);
      setSelection({ x: pos.x, y: pos.y, width: 0, height: 0 });
    },
    [toImageCoords]
  );

  const handleMouseMove = useCallback(
    (event: React.MouseEvent) => {
      if (!dragging || !start) return;
      const pos = toImageCoords(event);
      if (!pos) return;

      setSelection({
        x: Math.min(start.x, pos.x),
        y: Math.min(start.y, pos.y),
        width: Math.abs(pos.x - start.x),
        height: Math.abs(pos.y - start.y),
      });
    },
    [dragging, start, toImageCoords]
  );

  const handleMouseUp = useCallback(() => {
    setDragging(false);
  }, []);

  // Entrée = valider, Échap = annuler
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        cancel();
      } else if (event.key === 'Enter' && selection) {
        confirm(selection);
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [cancel, confirm, selection]);

  // Repère affiché à l'écran (converti depuis les pixels de l'image)
  const overlay = (() => {
    const img = imageRef.current;
    if (!selection || !img) return null;

    const rect = img.getBoundingClientRect();
    const scaleX = rect.width / img.naturalWidth;
    const scaleY = rect.height / img.naturalHeight;

    return {
      left: rect.left + selection.x * scaleX,
      top: rect.top + selection.y * scaleY,
      width: selection.width * scaleX,
      height: selection.height * scaleY,
    };
  })();

  if (error) {
    return (
      <div className="region-selector" onClick={cancel}>
        <div className="region-error">
          <p>{error}</p>
          <button onClick={cancel}>Fermer</button>
        </div>
      </div>
    );
  }

  return (
    <div
      className="region-selector"
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      onContextMenu={(e) => {
        e.preventDefault();
        cancel();
      }}
    >
      {imageUrl ? (
        <img
          ref={imageRef}
          src={imageUrl}
          alt="Écran figé"
          className="region-freeze"
          draggable={false}
        />
      ) : (
        <div className="region-loading">Préparation de la sélection…</div>
      )}

      {/* Voile sombre hors de la sélection */}
      {overlay && (
        <>
          <div className="region-dim" style={{ left: 0, top: 0, right: 0, height: overlay.top }} />
          <div
            className="region-dim"
            style={{ left: 0, top: overlay.top + overlay.height, right: 0, bottom: 0 }}
          />
          <div
            className="region-dim"
            style={{ left: 0, top: overlay.top, width: overlay.left, height: overlay.height }}
          />
          <div
            className="region-dim"
            style={{
              left: overlay.left + overlay.width,
              top: overlay.top,
              right: 0,
              height: overlay.height,
            }}
          />
          <div
            className="region-frame"
            style={{
              left: overlay.left,
              top: overlay.top,
              width: overlay.width,
              height: overlay.height,
            }}
          />
          <div
            className="region-size"
            style={{
              left: overlay.left,
              top: Math.max(overlay.top - 26, 0),
            }}
          >
            {Math.round(selection!.width)} × {Math.round(selection!.height)}
          </div>
        </>
      )}

      {!dragging && (
        <div className="region-hint">
          Glissez pour sélectionner une zone · <kbd>Entrée</kbd> valider · <kbd>Échap</kbd> annuler
        </div>
      )}
    </div>
  );
}
