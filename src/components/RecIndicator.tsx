import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import "./RecIndicator.css";

/** Retour envoyé par le backend après chaque ajustement de l'incrustation */
interface LiveFeedback {
  action: string;
  corner: string;
  shape: string;
  size_percent: number;
  camera_visible: boolean;
  swapped: boolean;
}

const toTime = (ms: number): string => {
  const secs = Math.floor(ms / 1000);
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
};

/** Résumé permanent de la présentation en cours */
function summarise(live: LiveFeedback): string {
  if (!live.camera_visible) return "Caméra masquée";
  if (live.swapped) return `Caméra en grand · écran ${live.corner.toLowerCase()}`;
  return `${live.corner} · ${live.shape} · ${live.size_percent} %`;
}

/**
 * Indicateur flottant affiché pendant l'enregistrement.
 *
 * La fenêtre principale étant masquée, c'est le seul retour visible : il
 * affiche donc aussi l'état de l'incrustation, et met en avant le dernier
 * ajustement effectué au clavier.
 */
export function RecIndicator() {
  const [elapsed, setElapsed] = useState(0);
  const [recording, setRecording] = useState(true);
  const [live, setLive] = useState<LiveFeedback | null>(null);
  const [flash, setFlash] = useState<string | null>(null);

  const unlisteners = useRef<(() => void)[]>([]);
  const flashTimer = useRef<number | null>(null);

  useEffect(() => {
    let disposed = false;

    (async () => {
      const offTick = await listen<number>("rec-tick", (event) => {
        if (!disposed) {
          setElapsed(event.payload);
          setRecording(true);
        }
      });

      const offStop = await listen<void>("rec-stop", () => {
        if (!disposed) setRecording(false);
      });

      const offLive = await listen<LiveFeedback>("live-feedback", (event) => {
        if (disposed) return;
        setLive(event.payload);

        // Le libellé du changement reste en avant quelques secondes, puis
        // laisse place au résumé permanent.
        setFlash(event.payload.action);
        if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
        flashTimer.current = window.setTimeout(() => setFlash(null), 2200);
      });

      if (disposed) {
        offTick();
        offStop();
        offLive();
        return;
      }
      unlisteners.current.push(offTick, offStop, offLive);
    })();

    return () => {
      disposed = true;
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
      unlisteners.current.forEach((off) => off());
      unlisteners.current = [];
    };
  }, []);

  return (
    <div className={`rec-indicator ${recording ? "rec-indicator--on" : "rec-indicator--off"}`}>
      <div className="rec-indicator__row">
        <span className="rec-indicator__dot" />
        <span className="rec-indicator__label">REC</span>
        <span className="rec-indicator__time">{toTime(elapsed)}</span>
      </div>

      {live && (
        <div
          key={flash ?? "resume"}
          className={`rec-indicator__live ${flash ? "rec-indicator__live--flash" : ""}`}
        >
          {flash ?? summarise(live)}
        </div>
      )}
    </div>
  );
}
