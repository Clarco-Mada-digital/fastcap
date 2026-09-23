/** Application du thème et de la couleur d'accent (identité visuelle) */

export type ThemeMode = 'light' | 'dark' | 'system';

const LIGHT_QUERY = '(prefers-color-scheme: light)';

export function resolveTheme(theme: ThemeMode): 'light' | 'dark' {
  if (theme === 'system') {
    return window.matchMedia(LIGHT_QUERY).matches ? 'light' : 'dark';
  }
  return theme;
}

/** Applique le thème (clair / sombre / système) sur le document */
export function applyTheme(theme: ThemeMode): void {
  document.documentElement.setAttribute('data-theme', resolveTheme(theme));
}

/** Vérifie qu'une chaîne est une couleur hexadécimale valide */
export function isValidHex(hex: string): boolean {
  return /^#?([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(hex.trim().replace(/^#/, '').replace(/([0-9a-fA-F]{3})/g, (m) => `${m[0]}${m[0]}${m[1]}${m[1]}${m[2]}${m[2]}`));
}

function normalizeHex(hex: string): string {
  let cleaned = hex.trim().replace(/^#/, '');
  if (cleaned.length === 3) {
    cleaned = cleaned
      .split('')
      .map((c) => c + c)
      .join('');
  }
  return cleaned;
}

/** Mélange deux couleurs hexadécimales (t = 0 → a, t = 1 → b) */
function mixHex(a: string, b: string, t: number): string {
  const channel = (i: number) => {
    const ca = parseInt(a.slice(i, i + 2), 16);
    const cb = parseInt(b.slice(i, i + 2), 16);
    return Math.round(ca + (cb - ca) * t);
  };
  const r = channel(0).toString(16).padStart(2, '0');
  const g = channel(2).toString(16).padStart(2, '0');
  const bl = channel(4).toString(16).padStart(2, '0');
  return `#${r}${g}${bl}`;
}

/** Applique la couleur d'accent de marque sur les variables CSS */
export function applyAccent(hex: string): void {
  if (!isValidHex(hex)) return;
  const cleaned = normalizeHex(hex);
  const root = document.documentElement;
  root.style.setProperty('--primary', `#${cleaned}`);
  root.style.setProperty('--primary-dark', mixHex(cleaned, '000000', 0.3));
  root.style.setProperty('--primary-glow', `rgba(${hexToRgb(cleaned)}, 0.28)`);
}

function hexToRgb(hex: string): string {
  const r = parseInt(hex.slice(0, 2), 16);
  const g = parseInt(hex.slice(2, 4), 16);
  const b = parseInt(hex.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

/** Effectue le rendu complet : thème + accent */
export function applyThemeAndAccent(theme: ThemeMode, accent: string): void {
  applyTheme(theme);
  applyAccent(accent);
}