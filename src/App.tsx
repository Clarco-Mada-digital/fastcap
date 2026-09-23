import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  useApp,
  useCapture,
  useCaptureHistory,
  useExport,
} from './hooks/useApp';
import { PreviewPanel } from './components/PreviewPanel';
import { RecordingPanel } from './components/RecordingPanel';
import { SettingsPanel } from './components/Settings';
import { applyTheme, applyThemeAndAccent, type ThemeMode } from './theme';
import './App.css';

import type { CaptureResult, WindowInfo } from './hooks/useApp';

type Mode = 'home' | 'library' | 'edit' | 'record' | 'settings';

interface HistoryItem {
  id: string;
  timestamp: string;
  filename: string;
  path: string;
  width: number;
  height: number;
  format: string;
  size_bytes: number;
}

function App() {
  const { appInfo, settings, isLoading: isAppLoading, saveSettings } = useApp();
  const { captureFullscreen, captureWindow, listWindows } = useCapture();
  const { history, loadHistory, readCaptureImage, readCaptureThumbnail, deleteCapture, clearHistory } = useCaptureHistory();
  const { copyToClipboard } = useExport();

  const [mode, setMode] = useState<Mode>('home');
  const [capturePreview, setCapturePreview] = useState<CaptureResult | null>(null);
  const [windows, setWindows] = useState<WindowInfo[]>([]);
  const [showWindowSelector, setShowWindowSelector] = useState(false);
  const [selectedWindow, setSelectedWindow] = useState<WindowInfo | null>(null);
  const [thumbnails, setThumbnails] = useState<Record<string, string>>({});
  const loadedThumbsRef = useRef<Set<string>>(new Set());
  const [libraryCount, setLibraryCount] = useState(18);

  // ---- Thème ----
  useEffect(() => {
    if (settings) {
      applyThemeAndAccent(settings.theme, settings.accent_color);
    }
  }, [settings?.theme, settings?.accent_color]);

  useEffect(() => {
    if (settings?.theme !== 'system') return;
    const mq = window.matchMedia('(prefers-color-scheme: light)');
    const onChange = () => applyTheme('system');
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, [settings?.theme]);

  const toggleTheme = useCallback(() => {
    if (!settings) return;
    const current = document.documentElement.getAttribute('data-theme');
    const next: ThemeMode = current === 'light' ? 'dark' : 'light';
    applyTheme(next);
    saveSettings({ ...settings, theme: next }).catch((e) => console.error(e));
  }, [settings, saveSettings]);

  // Charger la liste des fenêtres
  useEffect(() => {
    listWindows().then(setWindows);
  }, [listWindows]);

  // ---- Vignettes (légères, mises en cache, chargées en parallèle limité) ----
  useEffect(() => {
    let cancelled = false;
    const items = history.slice(0, mode === 'library' ? libraryCount : 6);
    const pending = items.filter((item) => !loadedThumbsRef.current.has(item.id));
    if (pending.length === 0) return;

    let cursor = 0;
    const CONCURRENCY = 3;

    const worker = async () => {
      while (true) {
        const index = cursor++;
        if (index >= pending.length) return;
        const item = pending[index];
        const url = await readCaptureThumbnail(item.path, 360);
        if (cancelled) return;
        loadedThumbsRef.current.add(item.id);
        if (!url) return;
        setThumbnails((prev) => (prev[item.id] ? prev : { ...prev, [item.id]: url }));
      }
    };

    void Promise.all(Array.from({ length: CONCURRENCY }, worker));

    return () => {
      cancelled = true;
    };
  }, [history, libraryCount, mode, readCaptureThumbnail]);

  // ---- Capture ----
  const handleCapture = useCallback(
    async (captureMode: 'fullscreen' | 'region' | 'window') => {
      try {
        let result: CaptureResult | null = null;

        switch (captureMode) {
          case 'fullscreen':
            result = await captureFullscreen();
            break;
          case 'window':
            if (selectedWindow) {
              result = await captureWindow(selectedWindow.id);
            } else {
              result = await captureFullscreen();
            }
            break;
          case 'region':
            await invoke('start_region_selection');
            return;
        }

        if (result) {
          if (settings?.auto_copy_to_clipboard) {
            copyToClipboard(result.data_url).catch((e) => console.error('Copie automatique:', e));
          }
          setCapturePreview(result);
          setMode('edit');
        }
      } catch (e) {
        console.error('Erreur capture:', e);
      }
    },
    [captureFullscreen, captureWindow, selectedWindow, settings, copyToClipboard]
  );

  const handleOpenWindowSelector = useCallback(() => {
    listWindows().then(setWindows);
    setShowWindowSelector(true);
  }, [listWindows]);

  // ---- Événements Tauri (raccourcis globaux, région, zone de notification) ----
  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    const prepare = async () => {
      const offRegion = await listen<CaptureResult>('region-captured', (event) => {
        setCapturePreview(event.payload);
        setMode('edit');
      });

      const offShortcut = await listen<string>('global-shortcut', (event) => {
        switch (event.payload) {
          case 'fullscreen':
            handleCapture('fullscreen');
            break;
          case 'region':
            invoke('start_region_selection').catch((e) => console.error(e));
            break;
          case 'window':
            handleOpenWindowSelector();
            break;
          // Pilotage de l'incrustation, sans interrompre l'enregistrement
          case 'cam-corner':
            invoke('recorder_live', { action: 'next-corner' }).catch(() => undefined);
            break;
          case 'cam-toggle':
            invoke('recorder_live', { action: 'toggle-camera' }).catch(() => undefined);
            break;
          case 'cam-shape':
            invoke('recorder_live', { action: 'next-shape' }).catch(() => undefined);
            break;
          case 'cam-swap':
            invoke('recorder_live', { action: 'toggle-swap' }).catch(() => undefined);
            break;
          case 'cam-bigger':
            invoke('recorder_live', { action: { resize: 5 } }).catch(() => undefined);
            break;
          case 'cam-smaller':
            invoke('recorder_live', { action: { resize: -5 } }).catch(() => undefined);
            break;
          default:
            setMode('home');
        }
      });

      const offTray = await listen('trigger-capture', () => handleOpenWindowSelector());
      const offRecord = await listen('trigger-recording', () => setMode('record'));
      const offStopRecord = await listen('stop-recording', async () => {
        try {
          await invoke('stop_recording');
        } catch {
          // aucun enregistrement en cours
        }
        try {
          const window = getCurrentWindow();
          await window.show();
          await window.setFocus();
        } catch {
          // déjà visible
        }
        setMode('record');
      });

      if (disposed) {
        offRegion();
        offShortcut();
        offTray();
        offRecord();
        offStopRecord();
        return;
      }
      unlisteners.push(offRegion, offShortcut, offTray, offRecord, offStopRecord);
    };

    prepare();

    return () => {
      disposed = true;
      unlisteners.forEach((off) => off());
    };
  }, [handleCapture, handleOpenWindowSelector]);

  const handleOpenHistory = useCallback(
    async (record: HistoryItem) => {
      const dataUrl = await readCaptureImage(record.path);
      if (!dataUrl) return;
      setCapturePreview({
        id: record.id,
        timestamp: record.timestamp,
        width: record.width,
        height: record.height,
        format: record.format,
        data_url: dataUrl,
        path: record.path,
      });
      setMode('edit');
    },
    [readCaptureImage]
  );

  const handleCopyFromHistory = useCallback(
    (path: string) => {
      readCaptureImage(path).then((dataUrl) => {
        if (dataUrl) copyToClipboard(dataUrl).catch(() => undefined);
      });
    },
    [readCaptureImage, copyToClipboard]
  );

  const handleNewCapture = useCallback(() => {
    setCapturePreview(null);
    setMode('home');
  }, []);

  const handleSelectWindow = useCallback(
    (window: WindowInfo) => {
      setSelectedWindow(window);
      setShowWindowSelector(false);
      handleCapture('window');
    },
    [handleCapture]
  );

  const handleClearSelection = useCallback(() => {
    setSelectedWindow(null);
    setShowWindowSelector(false);
  }, []);

  const handleCloseEdit = useCallback(() => {
    setCapturePreview(null);
    setMode('home');
  }, []);

  /** Après enregistrement d'une édition : rafraîchit vignette + dimensions */
  const handleSavedEdit = useCallback(
    (id: string) => {
      loadedThumbsRef.current.delete(id);
      setThumbnails((prev) => {
        if (!(id in prev)) return prev;
        const next = { ...prev };
        delete next[id];
        return next;
      });
      loadHistory();
    },
    [loadHistory]
  );

  const navItems: { id: Mode; label: string; icon: React.ReactNode }[] = [
    {
      id: 'home',
      label: 'Accueil',
      icon: (
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
          <path d="M3 11l9-8 9 8M5 10v10h5v-6h4v6h5V10" strokeLinejoin="round" />
        </svg>
      ),
    },
    {
      id: 'library',
      label: 'Mes captures',
      icon: (
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
          <path d="M4 5a2 2 0 012-2h12a2 2 0 012 2v14a2 2 0 01-2 2H6a2 2 0 01-2-2V5z" strokeLinejoin="round" />
          <path d="M8 9h8M8 13h5" strokeLinecap="round" />
        </svg>
      ),
    },
    {
      id: 'record',
      label: 'Enregistrer',
      icon: (
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
          <rect x="2" y="5" width="14" height="14" rx="2" />
          <path d="M16 10l6-3v10l-6-3" strokeLinejoin="round" />
        </svg>
      ),
    },
    {
      id: 'settings',
      label: 'Paramètres',
      icon: (
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8">
          <circle cx="12" cy="12" r="3" />
          <path d="M12 2v3M12 19v3M4.2 4.2l2.1 2.1M17.7 17.7l2.1 2.1M2 12h3M19 12h3M4.2 19.8l2.1-2.1M17.7 6.3l2.1-2.1" strokeLinecap="round" />
        </svg>
      ),
    },
  ];

  if (isAppLoading) {
    return (
      <div className="app-loading">
        <div className="loading-badge">
          <svg viewBox="0 0 24 24" width="44" height="44">
            <path d="M12 2L2 7l10 5 10-5-10-5z" stroke="var(--primary)" strokeWidth="1.6" fill="none" strokeLinejoin="round" opacity="0.9" />
            <path d="M2 12l10 5 10-5" stroke="var(--primary)" strokeWidth="1.6" fill="none" strokeLinejoin="round" opacity="0.65" />
            <path d="M2 17l10 5 10-5" stroke="var(--primary)" strokeWidth="1.6" fill="none" strokeLinejoin="round" opacity="0.35" />
          </svg>
        </div>
        <h1 className="loading-title">
          fast<span>cap</span>
        </h1>
        <p className="loading-text">Préparation de votre espace de capture…</p>
      </div>
    );
  }

  return (
    <div className="app">
      {/* En-tête */}
      <header className="app-header">
        <button className="brand" onClick={() => setMode('home')}>
          <span className="brand-logo">
            <svg viewBox="0 0 24 24" width="22" height="22">
              <path d="M12 2L2 7l10 5 10-5-10-5z" stroke="currentColor" strokeWidth="1.7" fill="none" strokeLinejoin="round" />
              <path d="M2 12l10 5 10-5" stroke="currentColor" strokeWidth="1.7" fill="none" strokeLinejoin="round" opacity="0.7" />
              <path d="M2 17l10 5 10-5" stroke="currentColor" strokeWidth="1.7" fill="none" strokeLinejoin="round" opacity="0.4" />
            </svg>
          </span>
          <span className="brand-name">
            fast<span>cap</span>
            <em>{appInfo?.version || '1.0'}</em>
          </span>
        </button>

        <nav className="app-nav">
          {navItems.map((item) => (
            <button
              key={item.id}
              className={`nav-item ${mode === item.id ? 'active' : ''}`}
              onClick={() => setMode(item.id)}
            >
              {item.icon}
              <span>{item.label}</span>
            </button>
          ))}
        </nav>

        <div className="header-right">
          <button className="icon-action" onClick={toggleTheme} title="Basculer le thème">
            <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
              <path d="M21 12.8a9 9 0 11-9.8-9.8 7 7 0 009.8 9.8z" strokeLinejoin="round" />
            </svg>
          </button>
          <button className="btn-capture-new" onClick={() => setMode('home')}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.9">
              <rect x="2" y="4" width="20" height="15" rx="2.5" />
              <circle cx="12" cy="11.5" r="3.6" />
            </svg>
            Nouvelle capture
          </button>
        </div>
      </header>

      {/* Contenu */}
      <main className="app-main">
        {mode === 'home' && (
          <div className="home-view">
            {/* Héros */}
            <section className="hero">
              <div className="hero-text">
                <span className="hero-eyebrow">Capture d'écran pro · gratuite · instantanée</span>
                <h2>
                  Capturez, <span className="hero-accent">annotez</span> et <span className="hero-underline">partagez</span>
                </h2>
                <p>
                  Plein écran, région, fenêtre, enregistrement vidéo, OCR et un éditeur complet —
                  tout est à portée de clic.
                </p>
              </div>
              <div className="hero-art">
                <div className="hero-card hero-card-a">
                  <span className="hero-shot">
                    <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" strokeWidth="1.8">
                      <path d="M4 7V5a2 2 0 012-2h3l2 2h7a2 2 0 012 2v2M4 7v10a2 2 0 002 2h12a2 2 0 002-2V9" strokeLinejoin="round" />
                      <path d="M21 12l-4 5-4-4 6-6 2 5z" strokeLinejoin="round" />
                      <path d="M9 10.5a1.5 1.5 0 11-3 0 1.5 1.5 0 013 0z" fill="currentColor" />
                    </svg>
                  </span>
                  <span className="hero-caption">Plein écran &amp; région</span>
                </div>
                <div className="hero-card hero-card-b">
                  <span className="hero-pen">
                    <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M4 20c4-2 11.5-6.5 15-10 1.5-1.5 1-3-.5-4-1.5-1-3-1.5-4.5 0C11 10 6 17 4 20z" />
                    </svg>
                  </span>
                  <span className="hero-caption">Flèches, texte, surlignage</span>
                </div>
                <div className="hero-card hero-card-c">
                  <span className="hero-rec">
                    <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" strokeWidth="1.8">
                      <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4" strokeLinecap="round" />
                      <circle cx="12" cy="12" r="3.2" />
                    </svg>
                  </span>
                  <span className="hero-caption">Vidéo + webcam</span>
                </div>
              </div>
            </section>

            {/* Actions de capture */}
            <section className="quick-capture">
              <div className="quick-grid">
                <button className="quick-card quick-fullscreen" onClick={() => handleCapture('fullscreen')}>
                  <span className="quick-icon">
                    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.7">
                      <rect x="2" y="3" width="20" height="18" rx="2.5" />
                    </svg>
                  </span>
                  <span className="quick-text">
                    <strong>Plein écran</strong>
                    <small>Capturer tout l'écran</small>
                  </span>
                  <kbd>Impr. écran</kbd>
                </button>

                <button className="quick-card quick-region" onClick={() => handleCapture('region')}>
                  <span className="quick-icon">
                    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.7">
                      <path d="M5 5h14v14H5z" strokeDasharray="4 3" />
                      <path d="M5 5l2-2v4l-2 2M19 5l-2-2v4l2 2M5 19l2 2v-4l-2-2M19 19l-2 2v-4l2-2" strokeLinejoin="round" />
                    </svg>
                  </span>
                  <span className="quick-text">
                    <strong>Région</strong>
                    <small>Sélectionner une zone</small>
                  </span>
                  <kbd>Ctrl+Maj+R</kbd>
                </button>

                <button className="quick-card quick-window" onClick={handleOpenWindowSelector}>
                  <span className="quick-icon">
                    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.7">
                      <rect x="3" y="4" width="18" height="17" rx="2.5" />
                      <path d="M9 4v17M3 9.5h18" />
                    </svg>
                  </span>
                  <span className="quick-text">
                    <strong>Fenêtre</strong>
                    <small>Une fenêtre ouverte</small>
                  </span>
                  <kbd>Ctrl+Maj+W</kbd>
                </button>

                <button className="quick-card quick-record" onClick={() => setMode('record')}>
                  <span className="quick-icon">
                    <svg viewBox="0 0 24 24" width="30" height="30" fill="none" stroke="currentColor" strokeWidth="1.7">
                      <rect x="2" y="5" width="14" height="14" rx="2.5" />
                      <path d="M16 10l6-3v10l-6-3" strokeLinejoin="round" />
                    </svg>
                  </span>
                  <span className="quick-text">
                    <strong>Enregistrer</strong>
                    <small>Vidéo d'écran</small>
                  </span>
                  <kbd>Ctrl+Maj+E</kbd>
                </button>
              </div>
            </section>

            {/* Captures récentes */}
            {history.length > 0 && (
              <section className="recent">
                <div className="section-head">
                  <h3>Captures récentes</h3>
                  <button className="link-btn" onClick={() => setMode('library')}>
                    Tout voir ({history.length})
                    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2">
                      <path d="M5 12h14M13 6l6 6-6 6" strokeLinecap="round" strokeLinejoin="round" />
                    </svg>
                  </button>
                </div>
                <div className="captures-grid">
                  {history.slice(0, 6).map((record) => (
                    <CaptureCard
                      key={record.id}
                      record={record}
                      thumb={thumbnails[record.id]}
                      onOpen={() => handleOpenHistory(record)}
                      onCopy={() => handleCopyFromHistory(record.path)}
                      onDelete={() => deleteCapture(record.id).catch((e) => console.error(e))}
                    />
                  ))}
                </div>
              </section>
            )}

            {/* Atouts */}
            <section className="atouts">
              <div className="atout">
                <span className="atout-icon atout-a">
                  <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M12 2l10 5-10 5L2 7l10-5z" strokeLinejoin="round" />
                    <path d="M2 12l10 5 10-5" strokeLinejoin="round" opacity="0.7" />
                  </svg>
                </span>
                <div>
                  <h4>Éditeur complet</h4>
                  <p>12 outils : stylo, formes, numéros, recadrage, flou…</p>
                </div>
              </div>
              <div className="atout">
                <span className="atout-icon atout-b">
                  <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M4 6V4a2 2 0 012-2h12a2 2 0 012 2v2M4 6h16M4 6v12a2 2 0 002 2h3" />
                    <circle cx="18" cy="15" r="3.5" />
                    <path d="M20.5 20.5L18 18" />
                  </svg>
                </span>
                <div>
                  <h4>OCR intégré</h4>
                  <p>Extrayez le texte d'une capture en un clic.</p>
                </div>
              </div>
              <div className="atout">
                <span className="atout-icon atout-c">
                  <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4M7 10l5 5 5-5M12 15V3" strokeLinecap="round" strokeLinejoin="round" />
                  </svg>
                </span>
                <div>
                  <h4>Export flexible</h4>
                  <p>PNG, JPEG, presse-papiers, partage instantané.</p>
                </div>
              </div>
            </section>
          </div>
        )}

        {mode === 'library' && (
          <div className="library-view">
            <div className="section-head library-head">
              <div>
                <h3>Mes captures</h3>
                <p>{history.length} capture{history.length > 1 ? 's' : ''} sur ce poste</p>
              </div>
              <div className="library-actions">
                <button className="link-btn danger" onClick={() => clearHistory().catch((e) => console.error(e))}>
                  Tout supprimer
                </button>
              </div>
            </div>

            {history.length === 0 ? (
              <div className="empty-state">
                <svg viewBox="0 0 24 24" width="44" height="44" fill="none" stroke="var(--text-muted)" strokeWidth="1.3">
                  <rect x="2" y="4" width="20" height="16" rx="3" />
                  <circle cx="12" cy="11.5" r="3.5" />
                </svg>
                <h4>Aucune capture pour l'instant</h4>
                <p>Lancez votre première capture depuis l'accueil.</p>
                <button className="btn-capture-new" onClick={() => setMode('home')}>
                  Démarrer une capture
                </button>
              </div>
            ) : (
              <>
                <div className="captures-grid library-grid">
                  {history.slice(0, libraryCount).map((record) => (
                    <CaptureCard
                      key={record.id}
                      record={record}
                      thumb={thumbnails[record.id]}
                      onOpen={() => handleOpenHistory(record)}
                      onCopy={() => handleCopyFromHistory(record.path)}
                      onDelete={() => deleteCapture(record.id).catch((e) => console.error(e))}
                    />
                  ))}
                </div>
                {libraryCount < history.length && (
                  <div className="load-more">
                    <button className="btn-ghost" onClick={() => setLibraryCount((c) => c + 18)}>
                      Charger plus
                    </button>
                  </div>
                )}
              </>
            )}
          </div>
        )}

        {mode === 'edit' && capturePreview && (
          <PreviewPanel
            capture={capturePreview}
            settings={settings}
            onClose={handleCloseEdit}
            onCaptureNew={handleNewCapture}
            onSaved={handleSavedEdit}
          />
        )}

        {mode === 'edit' && !capturePreview && (
          <div className="empty-state">
            <h4>Capture indisponible</h4>
            <button className="btn-capture-new" onClick={() => setMode('home')}>
              Retour à l'accueil
            </button>
          </div>
        )}

        {mode === 'record' && <RecordingPanel settings={settings} onClose={() => setMode('home')} />}

        {mode === 'settings' && settings && (
          <SettingsPanel settings={settings} onSave={saveSettings} onClose={() => setMode('home')} />
        )}
      </main>

      {/* Sélecteur de fenêtre */}
      {showWindowSelector && (
        <div className="window-selector-overlay">
          <div className="window-selector">
            <div className="window-selector-header">
              <h3>Choisir une fenêtre à capturer</h3>
              <button className="close-btn" onClick={handleClearSelection}>
                ×
              </button>
            </div>
            <div className="window-selector-list">
              {windows.map((win) => (
                <button
                  key={win.id}
                  className={`window-selector-item ${selectedWindow?.id === win.id ? 'selected' : ''}`}
                  onClick={() => handleSelectWindow(win)}
                >
                  <div className="window-selector-icon">
                    <svg viewBox="0 0 24 24" width="20" height="20">
                      <rect x="2" y="3" width="20" height="18" rx="2" fill="var(--bg-tertiary)" stroke="currentColor" strokeWidth="1.7" />
                    </svg>
                  </div>
                  <div className="window-selector-info">
                    <span className="window-selector-title">{win.title || 'Sans titre'}</span>
                    <span className="window-selector-size">
                      {win.app_name ? `${win.app_name} · ` : ''}
                      {win.width} × {win.height}
                    </span>
                  </div>
                  {selectedWindow?.id === win.id && (
                    <div className="window-selector-check">
                      <svg viewBox="0 0 24 24" width="20" height="20">
                        <path d="M20 6L9 17l-5-5" stroke="var(--primary)" strokeWidth="2" fill="none" />
                      </svg>
                    </div>
                  )}
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ---- Carte de capture (accueil + bibliothèque) ---- */
function CaptureCard({
  record,
  thumb,
  onOpen,
  onCopy,
  onDelete,
}: {
  record: HistoryItem;
  thumb?: string;
  onOpen: () => void;
  onCopy: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="capture-card">
      <button className="capture-card-thumb" onClick={onOpen} title={`Ouvrir ${record.filename}`}>
        {thumb ? (
          <img src={thumb} alt={record.filename} className="capture-card-image" loading="lazy" />
        ) : (
          <div className="capture-card-placeholder">
            <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" strokeWidth="1.5">
              <rect x="2" y="4" width="20" height="16" rx="3" />
              <circle cx="12" cy="11.5" r="3.5" />
            </svg>
          </div>
        )}
        <span className="capture-card-hover">
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
          Ouvrir
        </span>
      </button>
      <div className="capture-card-info">
        <span className="capture-card-filename" title={record.filename}>
          {record.filename}
        </span>
        <div className="capture-card-meta">
          <span className="capture-card-dimensions">
            {record.width} × {record.height}
          </span>
          <span className="capture-card-date">
            {new Date(record.timestamp).toLocaleString('fr-FR', { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' })}
          </span>
        </div>
      </div>
      <div className="capture-card-actions">
        <button onClick={onCopy} title="Copier dans le presse-papiers">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8">
            <rect x="9" y="9" width="11" height="11" rx="2" />
            <path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1" />
          </svg>
        </button>
        <button onClick={onDelete} title="Supprimer" className="danger">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8">
            <path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2M10 11v6M14 11v6M6 6v14a2 2 0 002 2h8a2 2 0 002-2V6" strokeLinecap="round" />
          </svg>
        </button>
      </div>
    </div>
  );
}

export default App;