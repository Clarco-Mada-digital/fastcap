import { useCallback, useEffect, useRef, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener';
import './VideoPlayer.css';

interface VideoPlayerProps {
  /** Chemin absolu du fichier vidéo */
  path: string;
  filename: string;
  onClose: () => void;
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
    : `${m}:${String(s).padStart(2, '0')}`;
}

/**
 * Lecteur vidéo intégré.
 *
 * Ouvrir l'enregistrement dans l'application évite de dépendre d'un lecteur
 * externe : sur une machine sans association de fichier pour le MP4, le bouton
 * « Lire » ne produisait aucun effet visible.
 */
export function VideoPlayer({ path, filename, onClose }: VideoPlayerProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);

  // Une image animée n'est pas un média au sens de la balise `<video>` : elle
  // s'affiche, elle ne se lit pas. Les commandes de lecture n'auraient aucune
  // prise dessus, on les retire donc plutôt que de les laisser inertes.
  const animation = /\.gif$/i.test(path);

  const [playing, setPlaying] = useState(false);
  const [current, setCurrent] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [error, setError] = useState<string | null>(null);

  const [source, setSource] = useState<string | null>(null);

  // Le moteur multimédia de WebKitGTK ne sait pas aller chercher un schéma
  // d'URL personnalisé comme `asset://` — il ne gère que http, file et blob.
  // On récupère donc le fichier par `fetch` (qui, lui, accepte ce schéma) et
  // on le présente à la balise `<video>` sous forme de blob.
  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;

    (async () => {
      try {
        const response = await fetch(convertFileSrc(path));
        if (!response.ok) {
          throw new Error(`réponse ${response.status}`);
        }
        const blob = await response.blob();
        if (cancelled) return;

        objectUrl = URL.createObjectURL(blob);
        setSource(objectUrl);
      } catch (cause) {
        if (!cancelled) {
          setError(`Le fichier n'a pas pu être chargé (${cause}).`);
        }
      }
    })();

    return () => {
      cancelled = true;
      // Le blob occupe la mémoire tant qu'il n'est pas révoqué
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [path]);

  const togglePlay = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) {
      void video.play().catch(() => setError('Lecture impossible.'));
    } else {
      video.pause();
    }
  }, []);

  const seek = useCallback((seconds: number) => {
    const video = videoRef.current;
    if (!video) return;
    video.currentTime = Math.min(Math.max(seconds, 0), video.duration || 0);
  }, []);

  // Raccourcis clavier usuels d'un lecteur
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      switch (event.key) {
        case 'Escape':
          onClose();
          break;
        case ' ':
          event.preventDefault();
          togglePlay();
          break;
        case 'ArrowRight':
          seek((videoRef.current?.currentTime ?? 0) + 5);
          break;
        case 'ArrowLeft':
          seek((videoRef.current?.currentTime ?? 0) - 5);
          break;
        default:
          break;
      }
    };

    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose, togglePlay, seek]);

  return (
    <div className="player-backdrop" onClick={onClose}>
      <div className="player-shell" onClick={(event) => event.stopPropagation()}>
        <header className="player-header">
          <span className="player-title" title={filename}>
            {filename}
          </span>
          <div className="player-header-actions">
            <button
              className="icon-btn"
              title="Afficher dans le dossier"
              onClick={() => revealItemInDir(path).catch(console.error)}
            >
              <svg viewBox="0 0 24 24" width="16" height="16">
                <path
                  d="M3 7V17C3 18 4 19 5 19H19C20 19 21 18 21 17V9C21 8 20 7 19 7H12L10 5H5C4 5 3 6 3 7Z"
                  stroke="currentColor"
                  strokeWidth="1.8"
                  fill="none"
                />
              </svg>
            </button>
            <button className="icon-btn" onClick={onClose} title="Fermer (Échap)">
              <svg viewBox="0 0 24 24" width="18" height="18">
                <path d="M18 6L6 18M6 6L18 18" stroke="currentColor" strokeWidth="2" />
              </svg>
            </button>
          </div>
        </header>

        <div className="player-stage">
          {error ? (
            <div className="player-error">
              <p>{error}</p>
              <p className="rec-muted small">
                Le moteur d'affichage a besoin d'un décodeur H.264 système
                (paquet <code>gstreamer1.0-libav</code> sous Linux).
              </p>
              <button
                className="btn-secondary btn-small"
                onClick={() => openPath(path).catch(console.error)}
              >
                Ouvrir avec le lecteur du système
              </button>
            </div>
          ) : !source ? (
            <div className="player-loading">
              <span className="spinner" />
              <span>Chargement…</span>
            </div>
          ) : animation ? (
            <img src={source} className="player-video" alt={filename} />
          ) : (
            <video
              ref={videoRef}
              src={source}
              className="player-video"
              onClick={togglePlay}
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onTimeUpdate={(event) => setCurrent(event.currentTarget.currentTime)}
              onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
              onError={(event) => {
                // Le code d'erreur du média dit précisément ce qui a échoué
                const media = event.currentTarget.error;
                const reason =
                  media?.code === 4
                    ? 'format non pris en charge'
                    : media?.code === 3
                      ? 'décodage impossible'
                      : media?.code === 2
                        ? 'lecture interrompue'
                        : 'cause inconnue';
                setError(`Lecture impossible — ${reason}.`);
              }}
              autoPlay
            />
          )}
        </div>

        <div className="player-controls">
          {animation ? (
            <span className="rec-muted small">Image animée — lecture en boucle</span>
          ) : (
            <>
              <button className="player-play" onClick={togglePlay} title="Lire / Pause (Espace)">
                {playing ? (
                  <svg viewBox="0 0 24 24" width="20" height="20">
                    <path d="M8 5V19M16 5V19" stroke="currentColor" strokeWidth="2.4" />
                  </svg>
                ) : (
                  <svg viewBox="0 0 24 24" width="20" height="20">
                    <path d="M8 5L19 12L8 19V5Z" fill="currentColor" />
                  </svg>
                )}
              </button>

              <span className="player-time">{formatTime(current)}</span>

              <input
                type="range"
                className="player-seek"
                min={0}
                max={duration || 0}
                step={0.05}
                value={current}
                onChange={(event) => seek(Number(event.target.value))}
              />

              <span className="player-time">{formatTime(duration)}</span>

              <svg viewBox="0 0 24 24" width="16" height="16" className="player-volume-icon">
                <path
                  d="M4 9V15H8L13 19V5L8 9H4Z"
                  stroke="currentColor"
                  strokeWidth="1.8"
                  fill="none"
                />
                {volume > 0 && (
                  <path d="M16 9C17 10 17 14 16 15" stroke="currentColor" strokeWidth="1.8" fill="none" />
                )}
              </svg>
              <input
                type="range"
                className="player-volume"
                min={0}
                max={1}
                step={0.05}
                value={volume}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setVolume(next);
                  if (videoRef.current) videoRef.current.volume = next;
                }}
              />
            </>
          )}
        </div>
      </div>
    </div>
  );
}
