import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import './Dependencies.css';

export interface Dependency {
  id: string;
  name: string;
  description: string;
  installed: boolean;
  version: string | null;
  installable: boolean;
  install_command: string | null;
  required: boolean;
}

interface InstallOutcome {
  success: boolean;
  message: string;
  log: string;
}

interface DependenciesProps {
  /** Rejouée après une installation réussie, pour relancer les détections */
  onInstalled?: () => void;
  onClose: () => void;
}

/**
 * Panneau d'installation des binaires externes.
 *
 * L'installation passe par le gestionnaire de paquets du système et demande
 * une authentification : plutôt que de renvoyer l'utilisateur vers une ligne
 * de commande, FastCap la construit et l'exécute pour lui.
 */
export function Dependencies({ onInstalled, onClose }: DependenciesProps) {
  const [dependencies, setDependencies] = useState<Dependency[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<(InstallOutcome & { id: string }) | null>(null);

  const refresh = useCallback(async () => {
    try {
      setDependencies(await invoke<Dependency[]>('dependency_status'));
    } catch (e) {
      console.error('Erreur état des dépendances:', e);
      setDependencies([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const install = useCallback(
    async (id: string) => {
      setBusy(id);
      setOutcome(null);
      try {
        const result = await invoke<InstallOutcome>('install_dependency', { name: id });
        setOutcome({ ...result, id });
        await refresh();
        if (result.success) onInstalled?.();
      } catch (e) {
        setOutcome({
          id,
          success: false,
          message: String(e),
          log: '',
        });
      } finally {
        setBusy(null);
      }
    },
    [refresh, onInstalled]
  );

  return (
    <div className="deps-backdrop" onClick={onClose}>
      <div className="deps-shell" onClick={(event) => event.stopPropagation()}>
        <header className="deps-header">
          <div>
            <h2>Dépendances</h2>
            <p>Outils externes utilisés par FastCap</p>
          </div>
          <button className="icon-btn" onClick={onClose} title="Fermer">
            <svg viewBox="0 0 24 24" width="18" height="18">
              <path d="M18 6L6 18M6 6L18 18" stroke="currentColor" strokeWidth="2" />
            </svg>
          </button>
        </header>

        <div className="deps-body">
          {dependencies === null ? (
            <div className="deps-loading">
              <span className="spinner" />
              <span>Vérification…</span>
            </div>
          ) : (
            dependencies.map((dependency) => (
              <div className="dep-card" key={dependency.id}>
                <div className={`dep-state ${dependency.installed ? 'ok' : 'missing'}`}>
                  {dependency.installed ? (
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path
                        d="M5 13L9 17L19 7"
                        stroke="currentColor"
                        strokeWidth="2.4"
                        fill="none"
                      />
                    </svg>
                  ) : (
                    <svg viewBox="0 0 24 24" width="16" height="16">
                      <path d="M12 7V13" stroke="currentColor" strokeWidth="2" />
                      <circle cx="12" cy="17" r="1.2" fill="currentColor" />
                    </svg>
                  )}
                </div>

                <div className="dep-info">
                  <span className="dep-name">
                    {dependency.name}
                    {dependency.required ? (
                      <em className="dep-tag required">requis</em>
                    ) : (
                      <em className="dep-tag">facultatif</em>
                    )}
                  </span>
                  <span className="dep-desc">{dependency.description}</span>

                  {dependency.installed && dependency.version && (
                    <code className="dep-version">{dependency.version}</code>
                  )}

                  {!dependency.installed && dependency.install_command && (
                    <code className="dep-command" title="Commande qui sera exécutée">
                      {dependency.install_command}
                    </code>
                  )}

                  {!dependency.installed && !dependency.installable && (
                    <span className="dep-manual">
                      Installation automatique indisponible sur ce système : installez{' '}
                      <code>{dependency.id}</code> avec les outils de votre distribution.
                    </span>
                  )}

                  {outcome?.id === dependency.id && (
                    <div className={`dep-outcome ${outcome.success ? 'ok' : 'error'}`}>
                      <span>{outcome.message}</span>
                      {!outcome.success && outcome.log && (
                        <details>
                          <summary>Détails</summary>
                          <pre>{outcome.log}</pre>
                        </details>
                      )}
                    </div>
                  )}
                </div>

                <div className="dep-action">
                  {dependency.installed ? (
                    <span className="dep-ok-label">Installé</span>
                  ) : (
                    <button
                      className="btn-secondary btn-small"
                      onClick={() => install(dependency.id)}
                      disabled={!dependency.installable || busy !== null}
                    >
                      {busy === dependency.id ? 'Installation…' : 'Installer'}
                    </button>
                  )}
                </div>
              </div>
            ))
          )}
        </div>

        <footer className="deps-footer">
          <p className="rec-muted small">
            L'installation utilise le gestionnaire de paquets du système et demande
            une authentification. Elle peut prendre quelques minutes.
          </p>
          <button className="btn-secondary btn-small" onClick={refresh} disabled={busy !== null}>
            Revérifier
          </button>
        </footer>
      </div>
    </div>
  );
}
