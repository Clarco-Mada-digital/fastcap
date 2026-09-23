import { useState, useEffect, useCallback } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import type { AppSettings } from '../hooks/useApp';
import { applyTheme, applyAccent } from '../theme';
import './Settings.css';

/** Raccourcis globaux enregistrés par le backend (voir src-tauri/src/lib.rs) */
const GLOBAL_SHORTCUTS = [
  { keys: 'PrintScreen', action: 'Capture plein écran' },
  { keys: 'Ctrl + Shift + R', action: 'Capture de région' },
  { keys: 'Ctrl + Shift + W', action: 'Capture de fenêtre' },
  { keys: 'Ctrl + Shift + S', action: 'Afficher FastCap' },
];

const ACCENT_PRESETS = ['#22d3ee', '#8b5cf6', '#6366f1', '#38bdf8', '#34d399', '#fb7185', '#f59e0b', '#f472b6'];

interface SettingsProps {
  settings: AppSettings | null;
  onSave: (settings: AppSettings) => Promise<void>;
  onClose: () => void;
}

export function SettingsPanel({ settings, onSave, onClose }: SettingsProps) {
  const [localSettings, setLocalSettings] = useState<AppSettings | null>(settings);
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);

  useEffect(() => {
    setLocalSettings(settings);
  }, [settings]);

  const handleSave = useCallback(async () => {
    if (!localSettings) return;
    
    setIsSaving(true);
    setSaveError(null);
    
    try {
      await onSave(localSettings);
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : 'Erreur de sauvegarde');
    } finally {
      setIsSaving(false);
    }
  }, [localSettings, onSave]);

  // Applique immédiatement le thème et l'accent (aperçu en direct)
  const handleThemeChange = useCallback((theme: AppSettings['theme']) => {
    setLocalSettings((prev) => (prev ? { ...prev, theme } : prev));
    applyTheme(theme);
  }, []);

  const handleAccentChange = useCallback((accent: string) => {
    setLocalSettings((prev) => (prev ? { ...prev, accent_color: accent } : prev));
    applyAccent(accent);
  }, []);

  // Sélecteur natif de dossier pour l'emplacement de sauvegarde
  const handlePickFolder = useCallback(async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: 'Choisir le dossier de sauvegarde',
    });

    if (typeof selected === 'string') {
      setLocalSettings((prev) => (prev ? { ...prev, default_save_path: selected } : prev));
    }
  }, []);

  if (!localSettings) {
    return (
      <div className="settings-panel">
        <div className="settings-loading">
          <div className="spinner"></div>
          <p>Chargement des paramètres...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="settings-panel">
      {/* Header */}
      <div className="settings-header">
        <h2>Paramètres</h2>
        <div className="settings-header-actions">
          <button className="btn-secondary" onClick={onClose}>
            Annuler
          </button>
          <button className="btn-primary" onClick={handleSave} disabled={isSaving}>
            {isSaving ? (
              <>
                <span className="spinner-small"></span>
                Sauvegarde...
              </>
            ) : (
              'Sauvegarder'
            )}
          </button>
        </div>
      </div>

      {/* État de la sauvegarde */}
      {saveSuccess && (
        <div className="settings-notification settings-notification-success">
          <svg viewBox="0 0 24 24" width="18" height="18">
            <path d="M22 11.08V12C21.998 14.155 21.301 16.228 20.004 18H4C2.896 16.228 2.121 14.155 2.002 12V11.08C1.902 8.077 2.836 5.154 4.493 3C6.151 0.846 8.415 0 11 0C13.585 0 15.848 0.846 17.505 3C19.162 5.154 20.096 8.077 20 11.08H22Z" stroke="var(--success)" strokeWidth="2" fill="none" />
            <path d="M12 19V5M8 12L12 8L16 12" stroke="var(--success)" strokeWidth="2" fill="none" />
          </svg>
          Paramètres sauvegardés avec succès
        </div>
      )}

      {saveError && (
        <div className="settings-notification settings-notification-error">
          <svg viewBox="0 0 24 24" width="18" height="18">
            <circle cx="12" cy="12" r="10" stroke="var(--error)" strokeWidth="2" fill="none" />
            <path d="M12 8V12M12 16H12.01" stroke="var(--error)" strokeWidth="2" fill="none" strokeLinecap="round" />
          </svg>
          {saveError}
        </div>
      )}

      {/* Sections */}
      <div className="settings-content">
        {/* Format d'export par défaut */}
        <section className="settings-section">
          <div className="section-header">
            <h3>Export</h3>
            <p>Paramètres par défaut pour l'exportation des captures</p>
          </div>
          <div className="section-content">
            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">Format d'export par défaut</label>
                <p className="setting-description">Le format utilisé lors de l'export rapide</p>
              </div>
              <select
                className="setting-select"
                value={localSettings.default_export_format}
                onChange={(e) => setLocalSettings({
                  ...localSettings,
                  default_export_format: e.target.value,
                })}
              >
                <option value="png">PNG (recommandé)</option>
                <option value="jpeg">JPEG (plus petit)</option>
              </select>
            </div>

            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">Qualité d'export</label>
                <p className="setting-description">Qualité pour les formats compressés (JPEG)</p>
              </div>
              <div className="setting-control">
                <input
                  type="range"
                  min="1"
                  max="100"
                  value={localSettings.default_export_quality}
                  onChange={(e) => setLocalSettings({
                    ...localSettings,
                    default_export_quality: Number(e.target.value),
                  })}
                  className="setting-slider"
                />
                <span className="setting-value">{localSettings.default_export_quality}%</span>
              </div>
            </div>
          </div>
        </section>

        {/* Chemin de sauvegarde */}
        <section className="settings-section">
          <div className="section-header">
            <h3>Sauvegarde</h3>
            <p>Emplacement et comportement de sauvegarde</p>
          </div>
          <div className="section-content">
            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">Dossier de sauvegarde</label>
                <p className="setting-description">Où les captures sont stockées localement</p>
              </div>
              <div className="setting-path">
                <input
                  type="text"
                  className="setting-input"
                  value={localSettings.default_save_path}
                  readOnly
                />
                <button className="btn-secondary btn-small" onClick={handlePickFolder}>
                  Choisir...
                </button>
              </div>
            </div>

            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">
                  <input
                    type="checkbox"
                    checked={localSettings.auto_save}
                    onChange={(e) => setLocalSettings({
                      ...localSettings,
                      auto_save: e.target.checked,
                    })}
                    className="setting-checkbox"
                  />
                  Sauvegarde automatique
                </label>
                <p className="setting-description">Sauvegarder automatiquement après chaque capture</p>
              </div>
            </div>

            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">
                  <input
                    type="checkbox"
                    checked={localSettings.auto_copy_to_clipboard}
                    onChange={(e) => setLocalSettings({
                      ...localSettings,
                      auto_copy_to_clipboard: e.target.checked,
                    })}
                    className="setting-checkbox"
                  />
                  Copie automatique dans le presse-papiers
                </label>
                <p className="setting-description">Copier la capture dans le presse-papiers après capture</p>
              </div>
            </div>
          </div>
        </section>

        {/* Raccourcis clavier */}
        <section className="settings-section">
          <div className="section-header">
            <h3>Raccourcis clavier</h3>
            <p>Raccourcis globaux actifs, même quand FastCap est en arrière-plan</p>
          </div>
          <div className="section-content">
            {GLOBAL_SHORTCUTS.map((shortcut) => (
              <div className="setting-item" key={shortcut.keys}>
                <div className="setting-info">
                  <label className="setting-label">{shortcut.action}</label>
                </div>
                <div className="setting-shortcut">
                  <kbd>{shortcut.keys}</kbd>
                </div>
              </div>
            ))}
          </div>
        </section>

        {/* Apparence */}
        <section className="settings-section">
          <div className="section-header">
            <h3>Apparence</h3>
            <p>Personnalisez l'apparence de l'application</p>
          </div>
          <div className="section-content">
            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">Thème</label>
                <p className="setting-description">Choisissez le thème de l'application</p>
              </div>
              <div className="theme-options">
                <button
                  className={`theme-btn ${localSettings.theme === 'dark' ? 'active' : ''}`}
                  onClick={() => handleThemeChange('dark')}
                >
                  <svg viewBox="0 0 24 24" width="20" height="20">
                    <path d="M21 12.79A9 9 0 1111.21 3a7 7 0 009.79 9.79z" stroke="currentColor" strokeWidth="2" fill="none" />
                  </svg>
                  Sombre
                </button>
                <button
                  className={`theme-btn ${localSettings.theme === 'light' ? 'active' : ''}`}
                  onClick={() => handleThemeChange('light')}
                >
                  <svg viewBox="0 0 24 24" width="20" height="20">
                    <circle cx="12" cy="12" r="5" stroke="currentColor" strokeWidth="2" fill="none" />
                    <path d="M12 1V3M12 21V23M4.22 4.22L5.64 5.64M18.36 18.36L19.78 19.78M1 12H3M21 12H23M4.22 19.78L5.64 18.36M18.36 5.64L19.78 4.22" stroke="currentColor" strokeWidth="2" fill="none" />
                  </svg>
                  Clair
                </button>
                <button
                  className={`theme-btn ${localSettings.theme === 'system' ? 'active' : ''}`}
                  onClick={() => handleThemeChange('system')}
                >
                  <svg viewBox="0 0 24 24" width="20" height="20">
                    <rect x="2" y="3" width="20" height="14" rx="2" stroke="currentColor" strokeWidth="2" fill="none" />
                    <path d="M8 21H16M12 17V21" stroke="currentColor" strokeWidth="2" fill="none" />
                  </svg>
                  Système
                </button>
              </div>
            </div>

            <div className="setting-item">
              <div className="setting-info">
                <label className="setting-label">Couleur d'accent</label>
                <p className="setting-description">La couleur signature de l'application</p>
              </div>
              <div className="accent-picker">
                <label className="accent-swatch accent-custom" style={{ background: localSettings.accent_color }}>
                  <input
                    type="color"
                    value={localSettings.accent_color}
                    onChange={(e) => handleAccentChange(e.target.value)}
                  />
                </label>
                {ACCENT_PRESETS.map((color) => (
                  <button
                    key={color}
                    className={`accent-swatch ${localSettings.accent_color === color ? 'active' : ''}`}
                    style={{ background: color }}
                    onClick={() => handleAccentChange(color)}
                    title={color}
                  />
                ))}
              </div>
            </div>
          </div>
        </section>

        {/* À propos */}
        <section className="settings-section settings-section-about">
          <div className="section-content">
            <div className="about-info">
              <div className="about-logo">
                <svg viewBox="0 0 24 24" width="32" height="32">
                  <path d="M12 2L2 7L12 12L22 7L12 2Z" stroke="var(--primary)" strokeWidth="2" fill="none" />
                  <path d="M2 17L12 22L22 17" stroke="var(--primary)" strokeWidth="2" fill="none" />
                  <path d="M2 12L12 17L22 12" stroke="var(--primary)" strokeWidth="2" fill="none" />
                </svg>
              </div>
              <div className="about-text">
                <h3>fastcap</h3>
                <p>Version {import.meta.env?.VITE_APP_VERSION || '1.0.0'}</p>
                <p className="about-description">
                  Application de capture d'écran complète
                </p>
              </div>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}