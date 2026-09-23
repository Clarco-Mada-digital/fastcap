// FastCap - Enregistrement vidéo
//
// Chaîne de traitement : un seul processus ffmpeg fait tout.
//
//   écran (x11grab / gdigrab / avfoundation) ─┐
//   webcam (v4l2 / dshow / avfoundation) ─────┼─> filtres ─> H.264 ─> MP4
//   micro (pulse / dshow / avfoundation) ─────┘
//
// Choix structurants, issus des défauts mesurés de la version précédente :
//
//  1. **La capture est native.** Une boucle Rust qui poussait des images
//     brutes dans un tube plafonnait à ~3 images/s (6,6 Mo par image) et
//     produisait des vidéos accélérées. Le grabber natif tient la cadence
//     demandée et horodate lui-même : la durée est exacte par construction.
//  2. **Les décorations sont pré-calculées.** Masques, ombres et arrière-plans
//     sont rendus une fois en PNG (voir `visuals`) puis simplement incrustés.
//     Les filtres `geq` / `boxblur` d'avant coûtaient à eux seuls ~10x le
//     temps réel.
//  3. **Les erreurs sont lisibles.** La sortie d'erreur de ffmpeg est
//     conservée et remontée telle quelle à l'utilisateur.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::RgbaImage;

use crate::compositor::{CamStyle, Compositor, Layout};
use crate::encoder::{self, Encoder};
use crate::process_util::{kill, run_bounded, run_output_bounded, StderrTail};
use crate::screen_input::{self, Geometry, Source};
use crate::visuals::{self, Backdrop, Presentation, Shape};
use crate::webcam;

const FFMPEG: &str = "ffmpeg";

/// Délai laissé à ffmpeg pour finaliser le fichier après la demande d'arrêt
const SHUTDOWN_GRACE: Duration = Duration::from_secs(12);

// --- Types échangés avec le frontend ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebcamOptions {
    pub device: String,
    /// "pip-tl" | "pip-tr" | "pip-bl" | "pip-br" | "side-left" | "side-right" | "full"
    pub layout: String,
    /// "square" | "rounded" | "circle"
    pub shape: String,
    /// "minimal" | "classic" | "studio" | "bubble"
    #[serde(default = "default_presentation")]
    pub presentation: String,
    pub size_percent: u32,
    pub margin: u32,
}

fn default_presentation() -> String {
    "minimal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingOptions {
    /// "fullscreen" | "region" | "window"
    pub source: String,
    pub window_id: Option<u32>,
    /// Écran à filmer, dans l'ordre renvoyé par `list_monitors`.
    /// Absent : l'écran principal.
    #[serde(default)]
    pub monitor: Option<usize>,
    pub region: Option<RegionRect>,
    pub fps: u32,
    /// "high" | "balanced" | "small"
    pub quality: String,
    /// Hauteur maximale de la vidéo : "native" | "1080" | "720" | "480"
    #[serde(default = "default_resolution")]
    pub resolution: String,
    /// Arrière-plan décoratif : "none" | "aurora" | "sunset" | "mint" | "slate" | "cream"
    #[serde(default)]
    pub backdrop: Option<String>,
    pub audio: bool,
    /// Microphone à enregistrer ; `None` pour ne pas capter la voix
    #[serde(default)]
    pub audio_device: Option<String>,
    /// Sortie haut-parleurs à enregistrer (« ce que joue la machine »).
    ///
    /// Combinée au microphone, elle permet de capter à la fois sa propre voix
    /// et celle des autres participants pendant une réunion.
    #[serde(default)]
    pub system_audio_device: Option<String>,
    pub webcam: Option<WebcamOptions>,
    pub hide_app: bool,
    #[serde(default)]
    pub timer_in_video: bool,
    /// Inclure le pointeur de souris (possible grâce à la capture native)
    #[serde(default = "default_true")]
    pub capture_cursor: bool,
    pub output_dir: Option<String>,
}

fn default_resolution() -> String {
    "720".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStart {
    pub id: String,
    pub path: String,
    pub started_ms: i64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    pub hardware: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub size_bytes: u64,
    pub has_webcam: bool,
    /// Cadence réellement atteinte, rapportée par ffmpeg
    pub captured_fps: f32,
    pub encoder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureDevice {
    pub id: String,
    pub label: String,
    /// "video" | "mic" | "system" : permet à l'interface de séparer le
    /// microphone du son joué par la machine.
    #[serde(default)]
    pub kind: String,
}

/// Compteurs alimentés par le flux `-progress` de ffmpeg
#[derive(Default)]
pub struct Stats {
    /// Images encodées
    pub frames: AtomicU64,
    /// Cadence instantanée, en millièmes (pour rester sans verrou)
    pub fps_milli: AtomicU64,
    /// Images perdues par le grabber
    pub dropped: AtomicU64,
    /// Position dans la vidéo, en millisecondes
    pub out_time_ms: AtomicU64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub frames: u64,
    pub dropped: u64,
    pub captured_fps: f32,
    /// Cadence demandée, pour situer la cadence réelle
    pub target_fps: u32,
    /// Part de la cadence demandée réellement atteinte, en pourcentage
    pub health_percent: f32,
}

/// Ce qui peut être ajusté pendant l'enregistrement
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveAction {
    /// Passe au coin suivant
    NextCorner,
    /// Place l'incrustation dans un coin précis
    Corner(String),
    /// Agrandit ou réduit l'incrustation, en points de pourcentage
    Resize(i32),
    /// Forme suivante (carré, arrondi, cercle)
    NextShape,
    /// Affiche ou masque la caméra
    ToggleCamera,
    /// Échange les rôles : caméra en grand, capture en médaillon
    ToggleSwap,
}

/// Retour affiché à l'écran après un ajustement.
///
/// Pendant l'enregistrement la fenêtre principale est masquée : sans ce
/// retour, l'utilisateur pilotait à l'aveugle et ne découvrait le résultat
/// qu'en relisant la vidéo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveFeedback {
    /// Ce qui vient de changer, en clair (« Caméra masquée »)
    pub action: String,
    /// Position de l'incrustation (« Bas droit »)
    pub corner: String,
    /// Forme (« Cercle »)
    pub shape: String,
    pub size_percent: u32,
    pub camera_visible: bool,
    pub swapped: bool,
}

/// État ajustable de l'incrustation pendant l'enregistrement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveState {
    pub layout: String,
    pub shape: String,
    pub size_percent: u32,
    pub margin: u32,
    pub camera_visible: bool,
    /// `true` : la caméra occupe le cadre et la capture passe en médaillon
    pub swapped: bool,
}

impl LiveState {
    /// Position de l'incrustation, en clair
    pub fn corner_label(&self) -> &'static str {
        match self.layout.as_str() {
            "pip-tl" => "Haut gauche",
            "pip-tr" => "Haut droit",
            "pip-bl" => "Bas gauche",
            _ => "Bas droit",
        }
    }

    /// Forme de l'incrustation, en clair
    pub fn shape_label(&self) -> &'static str {
        match self.shape.as_str() {
            "circle" => "Cercle",
            "square" => "Carré",
            _ => "Arrondi",
        }
    }

    /// Décrit l'ajustement qui vient d'être appliqué, pour l'affichage à l'écran
    pub fn describe(&self, action: &LiveAction) -> String {
        match action {
            LiveAction::ToggleCamera => {
                if self.camera_visible {
                    "Caméra affichée".to_string()
                } else {
                    "Caméra masquée".to_string()
                }
            }
            LiveAction::ToggleSwap => {
                if self.swapped {
                    "Caméra en grand".to_string()
                } else {
                    "Écran en grand".to_string()
                }
            }
            LiveAction::NextCorner | LiveAction::Corner(_) => self.corner_label().to_string(),
            LiveAction::NextShape => format!("Forme : {}", self.shape_label()),
            LiveAction::Resize(_) => format!("Taille : {} %", self.size_percent),
        }
    }

    /// Instantané complet, prêt à être affiché
    pub fn feedback(&self, action: &LiveAction) -> LiveFeedback {
        LiveFeedback {
            action: self.describe(action),
            corner: self.corner_label().to_string(),
            shape: self.shape_label().to_string(),
            size_percent: self.size_percent,
            camera_visible: self.camera_visible,
            swapped: self.swapped,
        }
    }
}

/// Session d'enregistrement en cours
pub struct RecordingSession {
    pub id: String,
    pub path: PathBuf,
    pub started: Instant,
    pub started_ms: i64,
    pub started_at: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder_label: String,
    pub has_webcam: bool,
    pub stats: Arc<Stats>,
    pub errors: StderrTail,
    /// Processus ffmpeg ; `None` une fois l'arrêt effectué
    pub child: Option<Child>,
    /// Réglages modifiables en direct ; `None` sans incrustation pilotable
    pub live: Option<LiveState>,
    /// Zone réellement filmée, en coordonnées écran
    pub recorded_area: Geometry,
    /// Caméra réservée à cette session, libérée à l'arrêt
    reserved_camera: Option<String>,
    /// Dimensions de sortie, nécessaires pour recalculer la géométrie
    output: (u32, u32),
    /// Fichiers temporaires à supprimer en fin de session
    assets: SessionAssets,
}

impl RecordingSession {
    pub fn stats_snapshot(&self) -> StatsSnapshot {
        let frames = self.stats.frames.load(Ordering::Relaxed);
        let dropped = self.stats.dropped.load(Ordering::Relaxed);
        let fps = self.stats.fps_milli.load(Ordering::Relaxed) as f32 / 1000.0;

        StatsSnapshot {
            frames,
            dropped,
            captured_fps: fps,
            target_fps: self.fps,
            health_percent: if self.fps == 0 {
                100.0
            } else {
                (fps / self.fps as f32 * 100.0).clamp(0.0, 100.0)
            },
        }
    }
}

// --- Disponibilité de ffmpeg ---

/// Version de ffmpeg, ou `None` s'il est introuvable.
///
/// Le résultat est mémorisé : la disponibilité et la version étaient
/// auparavant obtenues par deux lancements distincts de `ffmpeg -version`,
/// soit près d'une seconde à chaque ouverture du panneau.
pub fn version() -> Option<String> {
    static VERSION: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
        std::sync::OnceLock::new();
    let cache = VERSION.get_or_init(|| std::sync::Mutex::new(None));

    if let Some(cached) = cache.lock().ok().and_then(|guard| guard.clone()) {
        return Some(cached);
    }

    let detected = (|| {
        let output = run_output_bounded(
            Command::new(FFMPEG)
                .arg("-version")
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
            Duration::from_secs(5),
        )?;

        if !output.status.success() {
            return None;
        }

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .map(|line| line.trim().to_string())
    })();

    // Seul un succès est mémorisé : si ffmpeg vient d'être installé, la
    // détection suivante doit pouvoir le voir sans redémarrer l'application.
    if let (Some(found), Ok(mut guard)) = (&detected, cache.lock()) {
        *guard = Some(found.clone());
    }

    detected
}

pub fn is_available() -> bool {
    version().is_some()
}

// --- Énumération des périphériques ---

/// Liste les caméras. Sous Linux, les noms viennent de sysfs (instantané) et
/// la validation se fait **en parallèle**, avec un délai court.
///
/// Auparavant, dix `ffprobe` séquentiels de 2 s chacun pouvaient bloquer
/// l'ouverture du panneau d'enregistrement une vingtaine de secondes.
pub fn list_cameras() -> Vec<CaptureDevice> {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") else {
            return Vec::new();
        };

        let mut nodes: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("video"))
                    .unwrap_or(false)
            })
            .collect();
        nodes.sort();

        let mut candidates: Vec<(String, String)> = Vec::new();
        for node in nodes {
            let Some(name) = node.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let path = format!("/dev/{name}");
            if !Path::new(&path).exists() {
                continue;
            }

            let label = std::fs::read_to_string(node.join("name"))
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Caméra {name}"));

            candidates.push((path, label));
        }

        // Validation concurrente : le coût total est celui du périphérique le
        // plus lent, pas la somme de tous.
        let probes: Vec<_> = candidates
            .into_iter()
            .map(|(path, label)| {
                std::thread::spawn(move || {
                    let usable = run_bounded(
                        Command::new("ffprobe")
                            .args([
                                "-hide_banner",
                                "-loglevel",
                                "error",
                                "-f",
                                "v4l2",
                                "-i",
                                &path,
                            ])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null()),
                        Duration::from_millis(1500),
                    );

                    usable.then_some(CaptureDevice {
                        label: format!("{label} ({path})"),
                        id: path,
                        kind: "video".to_string(),
                    })
                })
            })
            .collect();

        probes
            .into_iter()
            .filter_map(|probe| probe.join().ok().flatten())
            .collect()
    }

    #[cfg(target_os = "windows")]
    {
        list_devices_from(
            &["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
            "DirectShow video devices",
        )
    }

    #[cfg(target_os = "macos")]
    {
        list_devices_from(
            &["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
            "AVFoundation video devices",
        )
    }
}

/// Liste les entrées audio.
///
/// Sous Linux, l'énumération passe par `pactl`, qui répond instantanément.
/// La version précédente *enregistrait réellement* 0,1 s de son pour savoir si
/// une entrée existait : l'appel dépassait régulièrement son délai de 3 s et
/// concluait à tort qu'aucun micro n'était disponible.
///
/// Les sources `.monitor` correspondent à la sortie haut-parleurs : elles
/// permettent d'enregistrer **le son du système**.
pub fn list_audio_devices() -> Vec<CaptureDevice> {
    #[cfg(target_os = "linux")]
    {
        let Some(output) = run_output_bounded(
            Command::new("pactl")
                .args(["list", "short", "sources"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
            Duration::from_secs(2),
        ) else {
            // `pactl` absent : on se rabat sur l'entrée par défaut de ffmpeg
            return vec![CaptureDevice {
                id: "default".to_string(),
                label: "Entrée par défaut".to_string(),
                kind: "mic".to_string(),
            }];
        };

        let text = String::from_utf8_lossy(&output.stdout);
        let mut devices = vec![CaptureDevice {
            id: "default".to_string(),
            label: "Micro par défaut".to_string(),
            kind: "mic".to_string(),
        }];

        for line in text.lines() {
            let mut fields = line.split('\t');
            let (Some(_index), Some(name)) = (fields.next(), fields.next()) else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }

            let is_monitor = name.ends_with(".monitor");
            devices.push(CaptureDevice {
                label: format!(
                    "{} — {}",
                    if is_monitor { "Son du système" } else { "Micro" },
                    pretty_source_name(name)
                ),
                id: name.to_string(),
                kind: if is_monitor { "system" } else { "mic" }.to_string(),
            });
        }

        devices
    }

    #[cfg(target_os = "windows")]
    {
        list_devices_from(
            &["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
            "DirectShow audio devices",
        )
    }

    #[cfg(target_os = "macos")]
    {
        list_devices_from(
            &["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
            "AVFoundation audio devices",
        )
    }
}

/// Rend lisible un nom de source PulseAudio
/// (`alsa_input.pci-0000_00_0e.0-platform-sof.HiFi__hw_x__source` → `hw x`).
#[cfg(target_os = "linux")]
fn pretty_source_name(name: &str) -> String {
    let trimmed = name.trim_end_matches(".monitor");

    let readable = trimmed
        .rsplit('.')
        .next()
        .unwrap_or(trimmed)
        .trim_start_matches("HiFi__")
        .trim_end_matches("__source")
        .trim_end_matches("__sink")
        .replace('_', " ")
        .trim()
        .to_string();

    if readable.is_empty() {
        trimmed.to_string()
    } else {
        readable
    }
}

#[allow(dead_code)]
fn list_devices_from(args: &[&str], section: &str) -> Vec<CaptureDevice> {
    let Some(output) = run_output_bounded(
        Command::new(FFMPEG)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
        Duration::from_secs(6),
    ) else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(&output.stderr).to_string();
    let mut devices = Vec::new();
    let mut in_section = false;

    for line in text.lines() {
        if line.contains(section) {
            in_section = true;
            continue;
        }
        if in_section && line.contains("devices") && !line.contains(section) {
            break;
        }
        if !in_section {
            continue;
        }

        if let (Some(start), Some(end)) = (line.find('"'), line.rfind('"')) {
            if end > start + 1 {
                let label = line[start + 1..end].to_string();
                devices.push(CaptureDevice {
                    id: label.clone(),
                    label,
                    kind: String::new(),
                });
            }
        }
    }

    devices
}

// --- Résolution de la source ---

/// Détermine la zone (ou la fenêtre) à filmer.
pub(crate) fn resolve_source(options: &RecordingOptions) -> Result<Source, String> {
    match options.source.as_str() {
        "window" => {
            let window_id = options
                .window_id
                .ok_or_else(|| "Aucune fenêtre sélectionnée".to_string())?;

            let window = xcap::Window::all()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|w| w.id().map(|id| id == window_id).unwrap_or(false))
                .ok_or_else(|| format!("Fenêtre {window_id} introuvable"))?;

            if window.is_minimized().unwrap_or(false) {
                return Err("La fenêtre est réduite : restaurez-la puis relancez".to_string());
            }

            Ok(Source::Window {
                id: window_id,
                geometry: Geometry {
                    x: window.x().unwrap_or(0),
                    y: window.y().unwrap_or(0),
                    width: even_floor(window.width().unwrap_or(1280)),
                    height: even_floor(window.height().unwrap_or(720)),
                },
            })
        }
        "region" => {
            let region = options
                .region
                .clone()
                .ok_or_else(|| "Aucune région définie".to_string())?;

            Ok(Source::Area(Geometry {
                x: region.x,
                y: region.y,
                width: even_floor(region.width),
                height: even_floor(region.height),
            }))
        }
        _ => {
            // Sur une installation multi-écrans, l'utilisateur choisit lequel
            // filmer ; à défaut, l'écran principal.
            let monitor = match options.monitor {
                Some(index) => monitor_at(index)?,
                None => primary_monitor()?,
            };

            Ok(Source::Area(Geometry {
                x: monitor.x().unwrap_or(0),
                y: monitor.y().unwrap_or(0),
                width: even_floor(monitor.width().unwrap_or(1920)),
                height: even_floor(monitor.height().unwrap_or(1080)),
            }))
        }
    }
}

/// Écran d'indice donné, dans l'ordre d'énumération de `list_monitors`
fn monitor_at(index: usize) -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let count = monitors.len();

    monitors
        .into_iter()
        .nth(index)
        .ok_or_else(|| format!("Écran {index} introuvable ({count} détecté(s))"))
}

fn primary_monitor() -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .cloned()
        .or_else(|| monitors.into_iter().next())
        .ok_or_else(|| "Aucun écran détecté".to_string())
}

/// Capture ponctuelle par `xcap`, pour les aperçus de l'interface.
///
/// `xcap` reste adapté ici : une image isolée coûte ~60 ms, ce qui est sans
/// importance pour une vignette, alors que c'était rédhibitoire en continu.
pub(crate) fn capture_once(source: &Source) -> Result<RgbaImage, String> {
    match source {
        Source::Window { id, .. } => {
            let window = xcap::Window::all()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|w| w.id().map(|wid| wid == *id).unwrap_or(false))
                .ok_or_else(|| "Fenêtre introuvable".to_string())?;

            window
                .capture_image()
                .map_err(|e| format!("Capture de la fenêtre impossible: {e}"))
        }
        Source::Area(geometry) => {
            let monitor = monitor_for(geometry.x, geometry.y).unwrap_or(primary_monitor()?);
            let screen = monitor
                .capture_image()
                .map_err(|e| format!("Capture impossible: {e}"))?;

            let local_x = (geometry.x - monitor.x().unwrap_or(0)).max(0) as u32;
            let local_y = (geometry.y - monitor.y().unwrap_or(0)).max(0) as u32;

            let x = local_x.min(screen.width().saturating_sub(2));
            let y = local_y.min(screen.height().saturating_sub(2));
            let width = geometry.width.min(screen.width() - x).max(2);
            let height = geometry.height.min(screen.height() - y).max(2);

            Ok(image::imageops::crop_imm(&screen, x, y, width, height).to_image())
        }
    }
}

fn monitor_for(x: i32, y: i32) -> Option<xcap::Monitor> {
    xcap::Monitor::all().ok()?.into_iter().find(|m| {
        let mx = m.x().unwrap_or(0);
        let my = m.y().unwrap_or(0);
        let mw = m.width().unwrap_or(0) as i32;
        let mh = m.height().unwrap_or(0) as i32;
        x >= mx && x < mx + mw && y >= my && y < my + mh
    })
}

/// Les encodeurs H.264 exigent des dimensions paires.
///
/// On arrondit **vers le bas** : arrondir vers le haut obligeait à
/// redimensionner chaque image capturée, à l'insu de l'utilisateur.
fn even_floor(value: u32) -> u32 {
    let value = value.max(2);
    value - (value % 2)
}

/// Dimensions de sortie après application du plafond de résolution choisi
fn output_size(width: u32, height: u32, resolution: &str) -> (u32, u32) {
    let max_height = match resolution {
        "1080" => 1080,
        "720" => 720,
        "480" => 480,
        _ => return (even_floor(width), even_floor(height)),
    };

    if height <= max_height {
        return (even_floor(width), even_floor(height));
    }

    let ratio = max_height as f32 / height as f32;
    (
        even_floor(((width as f32) * ratio) as u32),
        even_floor(max_height),
    )
}

// --- Visuels pré-rendus de la session ---

/// Fichiers PNG rendus au démarrage puis fournis à ffmpeg comme entrées fixes
#[derive(Default)]
struct SessionAssets {
    dir: Option<PathBuf>,
    backdrop: Option<PathBuf>,
    inset_mask: Option<PathBuf>,
    cam_mask: Option<PathBuf>,
    cam_decoration: Option<PathBuf>,
    cam_border: Option<PathBuf>,
}

impl SessionAssets {
    fn cleanup(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Taille de l'incrustation webcam pour une sortie donnée
fn cam_size(style: &CamStyle, output: (u32, u32)) -> (u32, u32) {
    let layout = style.layout;
    match layout {
        Layout::Full => output,
        Layout::SideLeft | Layout::SideRight => (even_floor(output.0 / 2), output.1),
        _ => {
            // Le médaillon est **carré**, quelle que soit la forme choisie.
            //
            // La caméra est masquée à une taille de référence carrée, puis
            // redimensionnée : un médaillon rectangulaire écraserait l'image.
            // C'est aussi la convention des bulles caméra (Loom, Tella), et
            // cela permet de passer du cercle au carré en direct sans
            // déformer le visage.
            let size = ((output.0 as f32) * (style.size_percent.clamp(8, 60) as f32) / 100.0) as u32;
            let size = even_floor(size.clamp(80, output.0.min(output.1)));
            (size, size)
        }
    }
}

/// Position de l'incrustation dans le cadre
fn cam_position(style: &CamStyle, output: (u32, u32), cam: (u32, u32)) -> (i32, i32) {
    let margin = style.margin.min(300) as i32;
    let (screen_w, screen_h) = (output.0 as i32, output.1 as i32);
    let (cam_w, cam_h) = (cam.0 as i32, cam.1 as i32);

    let (x, y) = match style.layout {
        Layout::PipTopLeft => (margin, margin),
        Layout::PipTopRight => (screen_w - margin - cam_w, margin),
        Layout::PipBottomLeft => (margin, screen_h - margin - cam_h),
        _ => (screen_w - margin - cam_w, screen_h - margin - cam_h),
    };

    (
        x.clamp(0, (screen_w - cam_w).max(0)),
        y.clamp(0, (screen_h - cam_h).max(0)),
    )
}

/// Le calque caméra englobe la marge décorative : pour afficher une caméra de
/// `cam_w` pixels, le calque entier doit être mis à cette échelle.
fn cam_layer_size(cam_w: u32, cam_h: u32) -> (u32, u32) {
    let reference = visuals::ATLAS_TILE;
    let layer = reference + visuals::SPRITE_PAD * 2;

    let scale = |value: u32| {
        even_floor(((value as u64 * layer as u64) / reference as u64) as u32)
    };

    (scale(cam_w), scale(cam_h))
}

/// Position du calque : décalée de la marge, pour que la caméra elle-même
/// tombe exactement à l'emplacement voulu.
fn cam_layer_position(cam_x: i32, cam_y: i32, cam_w: u32) -> (i32, i32) {
    let reference = visuals::ATLAS_TILE;
    let margin = ((cam_w as u64 * visuals::SPRITE_PAD as u64) / reference as u64) as i32;

    (cam_x - margin, cam_y - margin)
}

fn prepare_assets(
    id: &str,
    output: (u32, u32),
    backdrop: Backdrop,
    style: Option<&CamStyle>,
) -> Result<SessionAssets, String> {
    let needs_backdrop = backdrop.is_enabled();
    let needs_cam = style.map(|style| style.layout.is_pip()).unwrap_or(false);

    if !needs_backdrop && !needs_cam {
        return Ok(SessionAssets::default());
    }

    let dir = std::env::temp_dir().join(format!("fastcap-{id}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Dossier temporaire indisponible: {e}"))?;

    let mut assets = SessionAssets {
        dir: Some(dir.clone()),
        ..Default::default()
    };

    if needs_backdrop {
        let rect = visuals::inset_rect(output.0, output.1);
        let background = visuals::backdrop_image(output.0, output.1, backdrop);
        assets.backdrop = Some(visuals::write_rgba(&dir.join("backdrop.png"), &background)?);

        let mask = visuals::shape_mask(rect.width, rect.height, Shape::Rounded);
        assets.inset_mask = Some(visuals::write_gray(&dir.join("inset-mask.png"), &mask)?);
    }

    if let Some(style) = style.filter(|s| s.layout.is_pip()) {
        // Les décors sont rendus à la **taille de référence** du calque, pas à
        // la taille choisie : ils restent ainsi corrects après n'importe quel
        // redimensionnement en direct.
        let reference = visuals::ATLAS_TILE;

        // La « bulle » impose un cercle, quelle que soit la forme demandée
        let shape = if style.presentation == Presentation::Bubble {
            Shape::Circle
        } else {
            style.shape
        };

        // Atlas des trois formes : la découpe se déplace en direct, donc on
        // le fournit même pour un carré (l'utilisateur peut changer d'avis).
        let atlas = visuals::shape_atlas();
        assets.cam_mask = Some(visuals::write_gray(&dir.join("shapes.png"), &atlas)?);
        let _ = shape;

        if style.presentation.is_decorated() {
            let sprite = visuals::decoration_sprite(reference, reference, shape, style.presentation);
            assets.cam_decoration =
                Some(visuals::write_rgba(&dir.join("cam-shadow.png"), &sprite)?);
        }

        if let Some(border) = visuals::border_sprite(reference, reference, shape, style.presentation)
        {
            assets.cam_border = Some(visuals::write_rgba(&dir.join("cam-border.png"), &border)?);
        }
    }

    Ok(assets)
}

// --- Construction de la commande ffmpeg ---

/// Graphe de filtres et arguments d'entrée assemblés ensemble : les index
/// d'entrée dépendent des options actives, il faut les attribuer en un seul
/// endroit pour rester cohérent.
struct Plan {
    args: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn build_plan(
    source: &Source,
    output: (u32, u32),
    fps: u32,
    quality: &str,
    encoder: &Encoder,
    assets: &SessionAssets,
    style: Option<&CamStyle>,
    webcam_device: Option<&str>,
    audio_devices: &[String],
    backdrop: Backdrop,
    timer: bool,
    capture_cursor: bool,
    destination: &Path,
) -> Plan {
    let (out_w, out_h) = output;
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        // Suivi de la progression, lu pour alimenter les compteurs de l'interface
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ];

    args.extend(encoder.init_args());

    // --- Entrées ---
    let mut index = 0usize;

    // 0 : l'écran
    args.extend(screen_input::input_args(source, fps, capture_cursor));
    let screen_index = index;
    index += 1;

    // 1 : la webcam
    let webcam_index = webcam_device.map(|device| {
        args.extend(webcam::input_args(device));
        let current = index;
        index += 1;
        current
    });

    // Images fixes : arrière-plan, masques, ombres
    let mut still = |path: &Option<PathBuf>, args: &mut Vec<String>| -> Option<usize> {
        let path = path.as_ref()?;
        args.extend(["-i".to_string(), path.to_string_lossy().to_string()]);
        let current = index;
        index += 1;
        Some(current)
    };

    let backdrop_index = still(&assets.backdrop, &mut args);
    let inset_mask_index = still(&assets.inset_mask, &mut args);
    let cam_mask_index = still(&assets.cam_mask, &mut args);
    let cam_decoration_index = still(&assets.cam_decoration, &mut args);
    let cam_border_index = still(&assets.cam_border, &mut args);

    // Dernières entrées : les sources sonores (micro, son du système, ou les deux)
    let mut audio_indexes: Vec<usize> = Vec::new();
    for device in audio_devices {
        args.extend(audio_input_args(device));
        audio_indexes.push(index);
        index += 1;
    }
    let _ = index;

    // --- Graphe de filtres ---
    let mut graph: Vec<String> = Vec::new();

    // macOS capture l'écran entier : on recadre sur la zone demandée
    let screen_label = if screen_input::needs_software_crop() {
        let geometry = source.geometry();
        graph.push(format!(
            "[{screen_index}:v]crop={w}:{h}:{x}:{y}[cropped]",
            w = geometry.width,
            h = geometry.height,
            x = geometry.x.max(0),
            y = geometry.y.max(0)
        ));
        "cropped".to_string()
    } else {
        format!("{screen_index}:v")
    };

    // 1. L'écran, à la résolution de sortie (ou à celle de l'encart)
    let mut current = if backdrop.is_enabled() {
        let rect = visuals::inset_rect(out_w, out_h);
        graph.push(format!(
            "[{screen_label}]scale={w}:{h}:flags=fast_bilinear,setsar=1[inset]",
            w = rect.width,
            h = rect.height
        ));

        match (backdrop_index, inset_mask_index) {
            (Some(background), Some(mask)) => {
                graph.push(format!("[{mask}:v]format=gray[insetmask]"));
                graph.push("[inset][insetmask]alphamerge[insetr]".to_string());
                // L'arrière-plan est rendu en petit (dégradé + ombre floue,
                // sans détail fin) : c'est ici qu'il retrouve sa taille.
                graph.push(format!(
                    "[{background}:v]scale={out_w}:{out_h},setsar=1[bg]"
                ));
                graph.push(format!(
                    "[bg][insetr]overlay={x}:{y}[base]",
                    x = rect.x,
                    y = rect.y
                ));
                "base".to_string()
            }
            _ => "inset".to_string(),
        }
    } else {
        graph.push(format!(
            "[{screen_label}]scale={out_w}:{out_h}:flags=fast_bilinear,setsar=1[base]"
        ));
        "base".to_string()
    };

    // 2. L'incrustation webcam
    if let (Some(webcam), Some(style)) = (webcam_index, style) {
        let (cam_w, cam_h) = cam_size(style, output);

        match style.layout {
            Layout::Full => {
                graph.push(format!(
                    "[{webcam}:v]scale={out_w}:{out_h}:force_original_aspect_ratio=increase,\
                     crop={out_w}:{out_h},setsar=1[vfull]"
                ));
                current = "vfull".to_string();
            }
            Layout::SideLeft | Layout::SideRight => {
                let half = even_floor(out_w / 2);
                graph.push(format!(
                    "[{current}]scale={half}:{out_h}:force_original_aspect_ratio=decrease,\
                     pad={half}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=#0c0e14,setsar=1[sidescreen]"
                ));
                graph.push(format!(
                    "[{webcam}:v]scale={half}:{out_h}:force_original_aspect_ratio=increase,\
                     crop={half}:{out_h},setsar=1[sidecam]"
                ));

                let stacked = if style.layout == Layout::SideRight {
                    "[sidescreen][sidecam]hstack=inputs=2[vside]"
                } else {
                    "[sidecam][sidescreen]hstack=inputs=2[vside]"
                };
                graph.push(stacked.to_string());
                current = "vside".to_string();
            }
            _ => {
                let (cam_x, cam_y) = cam_position(style, output, (cam_w, cam_h));
                let reference = visuals::ATLAS_TILE;

                // Règle de sûreté du graphe : **aucun filtre dont la sortie
                // alimente `alphamerge` ne change de dimensions en direct**.
                // `alphamerge` exige deux entrées de taille identique ; les
                // redimensionner par deux commandes distinctes laissait une
                // fenêtre où elles divergeaient, et ffmpeg s'effondrait sur une
                // corruption de tas (SIGSEGV).
                //
                // Le seul redimensionnement pilotable est donc placé **après**
                // le masquage, et alimente un `overlay`, qui accepte une
                // incrustation de n'importe quelle taille.
                graph.push(format!("[{current}]split[basefull][basepip]"));
                current = "basefull".to_string();

                graph.push(format!("[{webcam}:v]split[campip][camfullsrc]"));

                // Chaîne de masquage, à dimensions fixes de bout en bout
                graph.push(format!(
                    "[campip]scale={reference}:{reference}:force_original_aspect_ratio=increase,\
                     crop={reference}:{reference},setsar=1[camsquare]"
                ));

                let masked = match cam_mask_index {
                    Some(atlas) => {
                        // La tuile de l'atlas fait déjà la taille de référence :
                        // changer de forme ne déplace que la découpe (`y`), sans
                        // jamais toucher aux dimensions.
                        let offset = visuals::atlas_offset(style.shape);
                        graph.push(format!(
                            "[{atlas}:v]crop@shape={reference}:{reference}:0:{offset},\
                             format=gray[cammask]"
                        ));
                        graph.push("[camsquare][cammask]alphamerge[cammasked]".to_string());
                        "cammasked"
                    }
                    None => "camsquare",
                };

                // Ombre/halo et liseré sont assemblés **avec** la caméra, à la
                // taille de référence, pour ne former qu'un seul calque.
                //
                // Les redimensionner séparément multipliait les filtres
                // pilotables : trois tailles à faire coïncider à chaque
                // ajustement, et le moindre décalage produisait un fichier
                // inexploitable. Ici, tout le calque suit un seul `scale`.
                let pad = visuals::SPRITE_PAD;
                let layer = reference + pad * 2;

                let mut decorated = masked.to_string();
                if let Some(decoration) = cam_decoration_index {
                    graph.push(format!("[{decoration}:v]scale={layer}:{layer}[decoref]"));
                    graph.push(format!(
                        "[decoref][{decorated}]overlay={pad}:{pad}[camdecorated]"
                    ));
                    decorated = "camdecorated".to_string();
                } else {
                    // Sans décor, le calque garde quand même sa marge : la
                    // géométrie de placement reste ainsi la même dans les deux cas.
                    graph.push(format!(
                        "[{decorated}]pad={layer}:{layer}:{pad}:{pad}:color=black@0[camdecorated]"
                    ));
                    decorated = "camdecorated".to_string();
                }

                if let Some(border) = cam_border_index {
                    graph.push(format!(
                        "[{border}:v]scale={reference}:{reference}[borderref]"
                    ));
                    graph.push(format!(
                        "[{decorated}][borderref]overlay={pad}:{pad}[cambordered]"
                    ));
                    decorated = "cambordered".to_string();
                }

                // Unique redimensionnement pilotable de toute la chaîne caméra
                let (layer_w, layer_h) = cam_layer_size(cam_w, cam_h);
                graph.push(format!(
                    "[{decorated}]scale@camsize={layer_w}:{layer_h}[camsized]"
                ));

                // Caméra plein cadre, pour l'échange des rôles. Dimensions
                // figées : elles valent toujours celles de la sortie.
                graph.push(format!(
                    "[camfullsrc]scale={out_w}:{out_h}:force_original_aspect_ratio=increase,\
                     crop={out_w}:{out_h},setsar=1[camfull]"
                ));
                graph.push(format!(
                    "[{current}][camfull]overlay@camfull=0:0:enable='0'[withcamfull]"
                ));
                current = "withcamfull".to_string();

                let (layer_x, layer_y) = cam_layer_position(cam_x, cam_y, cam_w);
                graph.push(format!(
                    "[{current}][camsized]overlay@cam={layer_x}:{layer_y}:enable='1'[withcam]"
                ));
                current = "withcam".to_string();

                // Capture en médaillon, dessinée **par-dessus** la caméra.
                // Désactivée tant que les rôles ne sont pas échangés.
                graph.push(format!("[basepip]scale@screenpip={cam_w}:{cam_h}[screenpip]"));
                graph.push(format!(
                    "[{current}][screenpip]overlay@screenpip={cam_x}:{cam_y}:enable='0'[withpip]"
                ));
                current = "withpip".to_string();
            }
        }
    }

    // 3. Minuteur incrusté
    if timer {
        if let Some(font) = timer_font_path() {
            graph.push(format!(
                "[{current}]drawtext=fontfile='{font}':fontsize={size}:fontcolor=white:\
                 box=1:boxcolor=black@0.45:boxborderw=12:x=24:y=24:\
                 text='REC %{{pts\\:hms}}'[withtimer]",
                size = (out_h / 30).clamp(18, 48)
            ));
            current = "withtimer".to_string();
        }
    }

    // 4. Adaptation à l'encodeur matériel
    if let Some(suffix) = encoder.filter_suffix() {
        graph.push(format!("[{current}]{suffix}[vout]"));
        current = "vout".to_string();
    }

    // 5. Mixage audio.
    //
    // Une seule source est mappée telle quelle ; plusieurs sont fondues par
    // `amix`, dans le **même** graphe que la vidéo — ffmpeg n'accepte qu'un
    // seul `-filter_complex`, un second écraserait silencieusement le premier.
    // `normalize=0` conserve le volume d'origine de chaque source, sans quoi
    // le gain serait divisé par le nombre d'entrées.
    let audio_label = match audio_indexes.len() {
        0 | 1 => None,
        count => {
            let inputs: String = audio_indexes
                .iter()
                .map(|index| format!("[{index}:a]"))
                .collect();
            graph.push(format!(
                "{inputs}amix=inputs={count}:duration=longest:normalize=0[aout]"
            ));
            Some("aout".to_string())
        }
    };

    args.push("-filter_complex".into());
    args.push(graph.join(";"));
    args.push("-map".into());
    args.push(format!("[{current}]"));

    match (&audio_label, audio_indexes.first()) {
        (Some(label), _) => {
            args.push("-map".into());
            args.push(format!("[{label}]"));
        }
        (None, Some(index)) => {
            args.push("-map".into());
            args.push(format!("{index}:a"));
        }
        (None, None) => {}
    }

    if !audio_indexes.is_empty() {
        args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "160k".into()]);
    }

    args.extend(encoder.encode_args(quality));
    args.extend([
        // Cadence de sortie constante : lecture fluide partout
        "-r".into(),
        fps.to_string(),
        // Une image-clé par seconde.
        //
        // Les coupes sans réencodage ne peuvent tomber que sur une image-clé :
        // à cinq secondes d'intervalle (le défaut), un montage serait
        // inutilisable. Mesuré à seulement +1,5 % sur la taille du fichier.
        "-g".into(),
        fps.to_string(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        destination.to_string_lossy().to_string(),
    ]);

    Plan { args }
}

pub(crate) fn timer_font_path() -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "C:\\Windows\\Fonts\\arial.ttf",
    ];

    CANDIDATES
        .iter()
        .find(|path| Path::new(path).exists())
        .map(|path| path.to_string())
}

fn audio_input_args(device: &str) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        vec!["-f".into(), "pulse".into(), "-i".into(), device.to_string()]
    }

    #[cfg(target_os = "windows")]
    {
        vec![
            "-f".into(),
            "dshow".into(),
            "-i".into(),
            format!("audio={device}"),
        ]
    }

    #[cfg(target_os = "macos")]
    {
        vec![
            "-f".into(),
            "avfoundation".into(),
            "-i".into(),
            format!("none:{device}"),
        ]
    }
}

// --- Démarrage ---

pub fn start(options: RecordingOptions, output_dir: PathBuf) -> Result<RecordingSession, String> {
    if !is_available() {
        return Err(
            "Enregistrement vidéo indisponible : installez ffmpeg et ajoutez-le au PATH."
                .to_string(),
        );
    }

    let source = resolve_source(&options)?;
    let geometry = source.geometry();
    let fps = options.fps.clamp(5, 60);
    let (width, height) = output_size(geometry.width, geometry.height, &options.resolution);

    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Création du dossier impossible: {e}"))?;

    let id = uuid::Uuid::new_v4().to_string();
    let filename = format!(
        "enregistrement_{}.mp4",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = output_dir.join(&filename);

    // Style de l'incrustation, figé pour toute la session : c'est ffmpeg qui
    // compose, et son graphe de filtres ne se modifie pas en cours de route.
    let style = options.webcam.as_ref().filter(|cam| !cam.device.is_empty()).map(|cam| CamStyle {
        layout: Layout::parse(&cam.layout),
        shape: Shape::parse(&cam.shape),
        presentation: Presentation::parse(&cam.presentation),
        size_percent: cam.size_percent,
        margin: cam.margin,
        offset_x: 0,
        offset_y: 0,
    });

    // La caméra est réservée avant toute chose : l'aperçu est fermé, le pilote
    // a le temps de relâcher le périphérique, et plus rien ne peut le rouvrir
    // avant que ffmpeg n'ait la main. Sans cela, l'aperçu — qui interroge la
    // caméra toutes les 300 ms — la rouvrait et ffmpeg échouait sur
    // « Device or resource busy ».
    let reserved_camera = options
        .webcam
        .as_ref()
        .map(|cam| cam.device.clone())
        .filter(|device| !device.is_empty());

    if let Some(device) = &reserved_camera {
        webcam::reserve(device)?;
    }

    // Micro et son du système sont indépendants : l'un, l'autre, ou les deux.
    let mut audio_devices: Vec<String> = Vec::new();
    if options.audio {
        if let Some(device) = options
            .audio_device
            .clone()
            .filter(|device| !device.is_empty())
        {
            audio_devices.push(device);
        }
        if let Some(device) = options
            .system_audio_device
            .clone()
            .filter(|device| !device.is_empty())
        {
            audio_devices.push(device);
        }

        // Aucune source précisée : on retombe sur la première disponible
        if audio_devices.is_empty() {
            audio_devices.push(
                list_audio_devices()
                    .into_iter()
                    .next()
                    .map(|device| device.id)
                    .ok_or_else(|| "Aucune entrée audio détectée".to_string())?,
            );
        }
    }

    let backdrop = Backdrop::parse(options.backdrop.as_deref().unwrap_or("none"));
    let assets = prepare_assets(&id, (width, height), backdrop, style.as_ref())?;

    // Un encodeur matériel libère le processeur pour la capture et les filtres
    let encoder = encoder::select(width, height, fps, true);
    let encoder_label = encoder.label();

    let plan = build_plan(
        &source,
        (width, height),
        fps,
        &options.quality,
        &encoder,
        &assets,
        style.as_ref(),
        options
            .webcam
            .as_ref()
            .map(|cam| cam.device.as_str())
            .filter(|device| !device.is_empty()),
        &audio_devices,
        backdrop,
        options.timer_in_video,
        options.capture_cursor,
        &path,
    );

    let errors = StderrTail::new();
    let mut child = Command::new(FFMPEG)
        .args(&plan.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            assets.cleanup();
            if let Some(device) = &reserved_camera {
                webcam::unreserve(device);
            }
            format!("Démarrage de ffmpeg impossible: {e}")
        })?;

    errors.attach(&mut child);

    // Origine des mesures : l'instant où ffmpeg commence réellement à filmer.
    // La prendre après la vérification de démarrage fausserait la cadence.
    let started = Instant::now();

    let stats = Arc::new(Stats::default());
    attach_progress(&mut child, stats.clone());

    // Un démarrage rate surtout au tout début (périphérique occupé, zone
    // invalide) : on laisse à ffmpeg le temps d'échouer avant d'annoncer
    // l'enregistrement comme actif.
    std::thread::sleep(Duration::from_millis(400));
    if let Ok(Some(status)) = child.try_wait() {
        assets.cleanup();
        if let Some(device) = &reserved_camera {
            webcam::unreserve(device);
        }
        return Err(errors.explain(&format!(
            "ffmpeg n'a pas pu démarrer la capture (code {})",
            status.code().unwrap_or(-1)
        )));
    }

    // Seules les dispositions en coin sont pilotables en direct : « côte à
    // côte » et « plein cadre » changent la structure du graphe.
    let live = style.as_ref().filter(|s| s.layout.is_pip()).map(|s| LiveState {
        layout: options
            .webcam
            .as_ref()
            .map(|cam| cam.layout.clone())
            .unwrap_or_else(|| "pip-br".to_string()),
        shape: options
            .webcam
            .as_ref()
            .map(|cam| cam.shape.clone())
            .unwrap_or_else(|| "rounded".to_string()),
        size_percent: s.size_percent,
        margin: s.margin,
        camera_visible: true,
        swapped: false,
    });

    Ok(RecordingSession {
        id,
        path,
        started,
        started_ms: chrono::Local::now().timestamp_millis(),
        started_at: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        filename,
        width,
        height,
        fps,
        encoder_label,
        has_webcam: style.is_some(),
        stats,
        errors,
        child: Some(child),
        live,
        recorded_area: geometry,
        reserved_camera,
        output: (width, height),
        assets,
    })
}

/// Lit le flux `-progress` de ffmpeg pour alimenter les compteurs en direct.
fn attach_progress(child: &mut Child, stats: Arc<Stats>) {
    let Some(stdout) = child.stdout.take() else {
        return;
    };

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);

        for line in reader.lines().map_while(Result::ok) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();

            match key.trim() {
                "frame" => {
                    if let Ok(frames) = value.parse::<u64>() {
                        stats.frames.store(frames, Ordering::Relaxed);
                    }
                }
                "fps" => {
                    if let Ok(fps) = value.parse::<f64>() {
                        stats
                            .fps_milli
                            .store((fps * 1000.0) as u64, Ordering::Relaxed);
                    }
                }
                "drop_frames" => {
                    if let Ok(dropped) = value.parse::<u64>() {
                        stats.dropped.store(dropped, Ordering::Relaxed);
                    }
                }
                "out_time_ms" => {
                    if let Ok(micros) = value.parse::<u64>() {
                        // ffmpeg exprime ce champ en microsecondes
                        stats.out_time_ms.store(micros / 1000, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
    });
}

// --- Pilotage en direct ---

/// Envoie une commande de filtre au ffmpeg en cours.
///
/// ffmpeg lit les commandes interactives sur son entrée standard : la touche
/// `c` est consommée seule, puis la ligne
/// `<cible> <temps>|-1 <commande> <argument>` est analysée par `sscanf`.
/// Aucun espace ne doit suivre le `c` — `%[^ ]` ne saute pas les blancs et la
/// commande serait silencieusement ignorée.
fn send_filter_command(child: &mut Child, target: &str, command: &str, argument: &str) {
    let Some(stdin) = child.stdin.as_mut() else {
        return;
    };

    let line = format!("c{target} -1 {command} {argument}\n");
    let _ = stdin.write_all(line.as_bytes());
    let _ = stdin.flush();
}

impl RecordingSession {
    /// Applique un ajustement en direct et renvoie le nouvel état.
    ///
    /// Tout passe par des commandes de filtre : ni le processus ffmpeg ni le
    /// flux audio ne sont interrompus.
    pub fn apply_live(&mut self, action: LiveAction) -> Result<LiveState, String> {
        let Some(mut live) = self.live.clone() else {
            return Err("Cet enregistrement n'a pas d'incrustation pilotable".to_string());
        };

        match action {
            LiveAction::NextCorner => {
                const CORNERS: [&str; 4] = ["pip-tl", "pip-tr", "pip-br", "pip-bl"];
                let position = CORNERS
                    .iter()
                    .position(|corner| *corner == live.layout)
                    .unwrap_or(0);
                live.layout = CORNERS[(position + 1) % CORNERS.len()].to_string();
            }
            LiveAction::Corner(corner) => live.layout = corner,
            LiveAction::Resize(delta) => {
                live.size_percent = ((live.size_percent as i32) + delta).clamp(10, 60) as u32;
            }
            LiveAction::NextShape => {
                live.shape = match live.shape.as_str() {
                    "square" => "rounded",
                    "rounded" => "circle",
                    _ => "square",
                }
                .to_string();
            }
            LiveAction::ToggleCamera => live.camera_visible = !live.camera_visible,
            LiveAction::ToggleSwap => live.swapped = !live.swapped,
        }

        let Some(child) = self.child.as_mut() else {
            return Err("Enregistrement déjà arrêté".to_string());
        };

        apply_live_state(child, &live, self.output);
        self.live = Some(live.clone());
        Ok(live)
    }
}

/// Traduit l'état voulu en commandes de filtres.
fn apply_live_state(child: &mut Child, live: &LiveState, output: (u32, u32)) {
    let style = CamStyle {
        layout: Layout::parse(&live.layout),
        shape: Shape::parse(&live.shape),
        presentation: Presentation::Minimal,
        size_percent: live.size_percent,
        margin: live.margin,
        offset_x: 0,
        offset_y: 0,
    };

    // Géométrie du médaillon, identique que ce soit la caméra ou la capture
    // qui l'occupe.
    let (pip_w, pip_h) = cam_size(&style, output);
    let (pip_x, pip_y) = cam_position(&style, output, (pip_w, pip_h));

    // Forme : simple déplacement de la découpe dans l'atlas. Les dimensions
    // de `crop@shape` ne bougent jamais — elles alimentent `alphamerge`.
    send_filter_command(
        child,
        "crop@shape",
        "y",
        &visuals::atlas_offset(style.shape).to_string(),
    );

    // Caméra plein cadre : ses dimensions sont figées, seule sa visibilité
    // change quand on échange les rôles.
    send_filter_command(
        child,
        "overlay@camfull",
        "enable",
        if live.swapped && live.camera_visible {
            "1"
        } else {
            "0"
        },
    );

    // Médaillon caméra : masqué quand la caméra passe en plein cadre
    let cam_pip_visible = live.camera_visible && !live.swapped;
    if cam_pip_visible {
        // Le calque englobe la marge décorative : on met à l'échelle
        // l'ensemble, puis on le place en compensant cette marge.
        let (layer_w, layer_h) = cam_layer_size(pip_w, pip_h);
        let (layer_x, layer_y) = cam_layer_position(pip_x, pip_y, pip_w);

        send_filter_command(child, "scale@camsize", "w", &layer_w.to_string());
        send_filter_command(child, "scale@camsize", "h", &layer_h.to_string());
        send_filter_command(child, "overlay@cam", "x", &layer_x.to_string());
        send_filter_command(child, "overlay@cam", "y", &layer_y.to_string());

    }

    send_filter_command(
        child,
        "overlay@cam",
        "enable",
        if cam_pip_visible { "1" } else { "0" },
    );

    // Capture en médaillon : visible seulement quand les rôles sont échangés,
    // et dessinée *après* la caméra pour passer au premier plan.
    let swapped_visible = live.swapped && live.camera_visible;
    if swapped_visible {
        send_filter_command(child, "scale@screenpip", "w", &pip_w.to_string());
        send_filter_command(child, "scale@screenpip", "h", &pip_h.to_string());
        send_filter_command(child, "overlay@screenpip", "x", &pip_x.to_string());
        send_filter_command(child, "overlay@screenpip", "y", &pip_y.to_string());
    }
    send_filter_command(
        child,
        "overlay@screenpip",
        "enable",
        if swapped_visible { "1" } else { "0" },
    );
}

// --- Arrêt ---

/// Arrête l'enregistrement et finalise le fichier.
pub fn stop(mut session: RecordingSession) -> Result<RecordingInfo, String> {
    let elapsed = session.started.elapsed();

    // La caméra redevient disponible pour l'aperçu, quoi qu'il advienne
    let reserved = session.reserved_camera.take();
    let release_camera = || {
        if let Some(device) = &reserved {
            webcam::unreserve(device);
        }
    };

    let Some(mut child) = session.child.take() else {
        session.assets.cleanup();
        release_camera();
        return Err("Enregistrement déjà arrêté".to_string());
    };

    // « q » demande à ffmpeg de conclure proprement : sans cela, l'index MP4
    // n'est pas écrit et le fichier reste illisible.
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"q\n");
        let _ = stdin.flush();
    }
    drop(child.stdin.take());

    let deadline = Instant::now() + SHUTDOWN_GRACE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }

        if Instant::now() >= deadline {
            // Dernier recours : le fichier sera peut-être tronqué, mais on ne
            // laisse pas un processus orphelin derrière nous.
            kill(&mut child);
            break None;
        }

        std::thread::sleep(Duration::from_millis(40));
    };

    session.assets.cleanup();
    release_camera();

    if let Some(status) = status {
        if !status.success() {
            return Err(session.errors.explain("L'encodage a échoué"));
        }
    }

    let size = std::fs::metadata(&session.path)
        .map(|meta| meta.len())
        .unwrap_or(0);

    if size == 0 {
        return Err(session
            .errors
            .explain("L'encodage n'a produit aucun fichier"));
    }

    let frames = session.stats.frames.load(Ordering::Relaxed);
    let duration_ms = {
        let reported = session.stats.out_time_ms.load(Ordering::Relaxed);
        if reported > 0 {
            reported
        } else {
            elapsed.as_millis() as u64
        }
    };

    Ok(RecordingInfo {
        id: session.id.clone(),
        path: session.path.to_string_lossy().to_string(),
        filename: session.filename.clone(),
        started_at: session.started_at.clone(),
        duration_ms,
        width: session.width,
        height: session.height,
        fps: session.fps,
        size_bytes: size,
        has_webcam: session.has_webcam,
        captured_fps: frames as f32 / elapsed.as_secs_f32().max(0.001),
        encoder: session.encoder_label.clone(),
    })
}

// --- Aperçus pour l'interface ---

/// Vignette d'un enregistrement : une image extraite de la vidéo.
///
/// La liste des enregistrements affichait une icône générique ; une vraie
/// image rend chaque capture reconnaissable d'un coup d'œil.
pub fn recording_thumbnail(path: &str, max_width: u32) -> Result<Vec<u8>, String> {
    if !Path::new(path).exists() {
        return Err("Fichier introuvable".to_string());
    }

    // On vise une seconde de lecture : la toute première image est souvent
    // encore noire (fondu d'ouverture de la source).
    let output = run_output_bounded(
        Command::new(FFMPEG)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                "1",
                "-i",
                path,
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={max_width}:-2:flags=fast_bilinear"),
                "-f",
                "image2",
                "-c:v",
                "mjpeg",
                "-q:v",
                "5",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        Duration::from_secs(10),
    )
    .ok_or_else(|| "Extraction de la vignette expirée".to_string())?;

    if output.stdout.is_empty() {
        // Vidéo plus courte qu'une seconde : on retente sur la première image
        let first = run_output_bounded(
            Command::new(FFMPEG)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-i",
                    path,
                    "-frames:v",
                    "1",
                    "-vf",
                    &format!("scale={max_width}:-2:flags=fast_bilinear"),
                    "-f",
                    "image2",
                    "-c:v",
                    "mjpeg",
                    "-q:v",
                    "5",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
            Duration::from_secs(10),
        )
        .ok_or_else(|| "Extraction de la vignette expirée".to_string())?;

        if first.stdout.is_empty() {
            return Err("Aucune image extraite".to_string());
        }
        return Ok(first.stdout);
    }

    Ok(output.stdout)
}

/// Image JPEG de la webcam, prise sur la source partagée.
pub fn webcam_snapshot(device: &str, max_width: u32) -> Result<Vec<u8>, String> {
    let camera = webcam::acquire(device)?;
    camera.wait_ready(Duration::from_secs(6))?;

    let frame = camera
        .latest()
        .ok_or_else(|| "Aucune image disponible".to_string())?;

    let image = if frame.image.width() > max_width {
        let ratio = max_width as f32 / frame.image.width() as f32;
        let height = ((frame.image.height() as f32) * ratio).max(1.0) as u32;
        image::imageops::resize(
            frame.image.as_ref(),
            max_width,
            height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        frame.image.as_ref().clone()
    };

    crate::image_utils::to_jpeg_bytes(&image, 78)
}

/// Aperçu composite : la composition telle qu'elle sera enregistrée.
///
/// Le rendu passe par le compositeur Rust, qui applique les mêmes masques et
/// les mêmes ombres pré-rendus que le graphe de filtres.
pub fn composite_preview(options: &RecordingOptions, max_width: u32) -> Result<Vec<u8>, String> {
    let camera_options = options
        .webcam
        .as_ref()
        .filter(|cam| !cam.device.is_empty())
        .ok_or_else(|| "Aucun périphérique webcam configuré".to_string())?;

    let source = resolve_source(options)?;
    let screen = capture_once(&source)?;
    let (width, height) = (screen.width(), screen.height());

    let camera = webcam::acquire(&camera_options.device)?;
    camera.wait_ready(Duration::from_secs(6))?;
    let frame = camera
        .latest()
        .ok_or_else(|| "Aucune image disponible depuis la webcam".to_string())?;

    let style = CamStyle {
        layout: Layout::parse(&camera_options.layout),
        shape: Shape::parse(&camera_options.shape),
        presentation: Presentation::parse(&camera_options.presentation),
        size_percent: camera_options.size_percent,
        margin: camera_options.margin,
        offset_x: 0,
        offset_y: 0,
    };

    let mut compositor = Compositor::new(width, height);
    let composed = compositor.compose(
        screen,
        Some((frame.image.as_ref(), frame.sequence)),
        Some(&style),
    );

    let preview = if composed.width() > max_width {
        let ratio = max_width as f32 / composed.width() as f32;
        let preview_height = ((composed.height() as f32) * ratio).max(1.0) as u32;
        image::imageops::resize(
            &composed,
            max_width,
            preview_height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        composed
    };

    crate::image_utils::to_jpeg_bytes(&preview, 80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn style(layout: Layout) -> CamStyle {
        CamStyle {
            layout,
            shape: Shape::Rounded,
            presentation: Presentation::Minimal,
            size_percent: 25,
            margin: 20,
            offset_x: 0,
            offset_y: 0,
        }
    }

    fn area() -> Source {
        Source::Area(Geometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }

    fn plan_for(
        assets: &SessionAssets,
        style: Option<&CamStyle>,
        webcam: Option<&str>,
        audio: &[String],
        backdrop: Backdrop,
    ) -> Vec<String> {
        let encoder = Encoder::X264 { preset: "ultrafast" };
        build_plan(
            &area(),
            (1280, 720),
            30,
            "balanced",
            &encoder,
            assets,
            style,
            webcam,
            audio,
            backdrop,
            false,
            true,
            Path::new("/tmp/out.mp4"),
        )
        .args
    }

    #[test]
    fn even_floor_never_rounds_up() {
        assert_eq!(even_floor(1081), 1080);
        assert_eq!(even_floor(963), 962);
        assert_eq!(even_floor(1), 2);
    }

    /// Un écran inexistant doit produire une erreur explicite, jamais un
    /// enregistrement silencieux du mauvais moniteur.
    #[test]
    #[ignore = "nécessite un écran"]
    fn an_unknown_monitor_is_reported_clearly() {
        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: Some(99),
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "720".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: None,
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let error = resolve_source(&options).expect_err("l'écran 99 n'existe pas");
        assert!(error.contains("99"), "message peu clair: {error}");
        assert!(error.contains("détecté"), "message peu clair: {error}");
    }

    /// L'écran choisi détermine la zone filmée.
    #[test]
    #[ignore = "nécessite un écran"]
    fn the_selected_monitor_defines_the_captured_area() {
        let monitors = crate::capture::enumerate_monitors().expect("énumération");
        let first = monitors.first().expect("au moins un écran");

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: Some(first.index),
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "720".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: None,
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let geometry = resolve_source(&options).expect("source").geometry();
        eprintln!(
            "  → écran {} « {} » : {}x{} en ({}, {})",
            first.index, first.name, geometry.width, geometry.height, geometry.x, geometry.y
        );

        assert_eq!(geometry.x, first.x);
        assert_eq!(geometry.y, first.y);
        assert_eq!(geometry.width, even_floor(first.width));
    }

    #[test]
    fn native_resolution_is_left_alone() {
        assert_eq!(output_size(1920, 1080, "native"), (1920, 1080));
    }

    #[test]
    fn resolution_cap_preserves_aspect_ratio() {
        assert_eq!(output_size(1920, 1080, "720"), (1280, 720));
    }

    #[test]
    fn small_sources_are_never_upscaled() {
        assert_eq!(output_size(1280, 720, "1080"), (1280, 720));
    }

    #[test]
    fn output_dimensions_are_always_even() {
        let (w, h) = output_size(1712, 963, "720");
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    #[test]
    fn a_plain_recording_scales_the_screen_to_the_output() {
        let args = plan_for(&SessionAssets::default(), None, None, &[], Backdrop::None);
        let graph = args.join(" ");

        assert!(graph.contains("scale=1280:720"), "obtenu: {graph}");
        // Sans webcam ni décor, la sortie est la sortie de la mise à l'échelle
        assert!(args.contains(&"[base]".to_string()), "obtenu: {args:?}");
    }

    /// Une image-clé par seconde : sans cela, les coupes sans réencodage
    /// tomberaient sur des bornes de cinq secondes.
    #[test]
    fn keyframes_are_frequent_enough_to_allow_clean_cuts() {
        let args = plan_for(&SessionAssets::default(), None, None, &[], Backdrop::None);
        let interval = args
            .iter()
            .position(|a| a == "-g")
            .and_then(|i| args.get(i + 1))
            .expect("intervalle d'images-clés");

        // 30 images par seconde dans `plan_for` : une clé par seconde
        assert_eq!(interval, "30");
    }

    #[test]
    fn the_output_framerate_is_pinned() {
        let args = plan_for(&SessionAssets::default(), None, None, &[], Backdrop::None);
        let position = args.iter().rposition(|a| a == "-r").expect("cadence");
        assert_eq!(args[position + 1], "30");
    }

    #[test]
    fn progress_is_requested_so_the_ui_can_show_live_stats() {
        let args = plan_for(&SessionAssets::default(), None, None, &[], Backdrop::None);
        assert!(args.contains(&"-progress".to_string()));
    }

    #[test]
    fn audio_is_mapped_from_the_last_input() {
        // Écran (0) + micro (1)
        let args = plan_for(
            &SessionAssets::default(),
            None,
            None,
            &["default".to_string()],
            Backdrop::None,
        );
        assert!(args.iter().any(|a| a == "1:a"), "obtenu: {args:?}");
    }

    #[test]
    fn still_images_shift_the_audio_input_index() {
        let dir = std::env::temp_dir().join("fastcap-test");
        let assets = SessionAssets {
            dir: Some(dir.clone()),
            backdrop: Some(dir.join("backdrop.png")),
            inset_mask: Some(dir.join("inset-mask.png")),
            ..Default::default()
        };

        // Écran (0), webcam (1), fond (2), masque (3), micro (4)
        let args = plan_for(
            &assets,
            Some(&style(Layout::PipBottomRight)),
            Some("/dev/video0"),
            &["default".to_string()],
            Backdrop::Aurora,
        );

        assert!(args.iter().any(|a| a == "4:a"), "obtenu: {args:?}");
    }

    /// Micro + son du système doivent être **fondus**, dans le graphe unique.
    #[test]
    fn two_audio_sources_are_mixed_in_the_single_filter_graph() {
        let args = plan_for(
            &SessionAssets::default(),
            None,
            None,
            &["default".to_string(), "sortie.monitor".to_string()],
            Backdrop::None,
        );

        // Un seul `-filter_complex` : un second écraserait le premier
        assert_eq!(
            args.iter().filter(|a| *a == "-filter_complex").count(),
            1,
            "obtenu: {args:?}"
        );

        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|i| args.get(i + 1))
            .expect("graphe");

        assert!(graph.contains("amix=inputs=2"), "obtenu: {graph}");
        // Le volume de chaque source doit être préservé
        assert!(graph.contains("normalize=0"), "obtenu: {graph}");
        assert!(args.iter().any(|a| a == "[aout]"), "obtenu: {args:?}");
    }

    #[test]
    fn a_single_audio_source_is_mapped_without_mixing() {
        let args = plan_for(
            &SessionAssets::default(),
            None,
            None,
            &["default".to_string()],
            Backdrop::None,
        );

        let graph = args.join(" ");
        assert!(!graph.contains("amix"), "mixage inutile: {graph}");
        assert!(args.iter().any(|a| a == "1:a"), "obtenu: {args:?}");
    }

    #[test]
    fn the_backdrop_rounds_and_insets_the_capture() {
        let dir = std::env::temp_dir().join("fastcap-test");
        let assets = SessionAssets {
            dir: Some(dir.clone()),
            backdrop: Some(dir.join("backdrop.png")),
            inset_mask: Some(dir.join("inset-mask.png")),
            ..Default::default()
        };

        let graph = plan_for(&assets, None, None, &[], Backdrop::Aurora).join(" ");
        assert!(graph.contains("alphamerge"), "obtenu: {graph}");
        assert!(graph.contains("overlay="), "obtenu: {graph}");
    }

    #[test]
    fn a_masked_webcam_is_merged_then_overlaid() {
        let dir = std::env::temp_dir().join("fastcap-test");
        let assets = SessionAssets {
            dir: Some(dir.clone()),
            cam_mask: Some(dir.join("cam-mask.png")),
            ..Default::default()
        };

        let graph = plan_for(
            &assets,
            Some(&style(Layout::PipBottomRight)),
            Some("/dev/video0"),
            &[],
            Backdrop::None,
        )
        .join(" ");

        assert!(graph.contains("alphamerge"), "obtenu: {graph}");
        // Aucun `geq` : c'est ce filtre qui coûtait ~10x le temps réel
        assert!(!graph.contains("geq"), "le filtre geq est revenu: {graph}");
    }

    /// Garde-fou contre une régression qui faisait **planter ffmpeg**
    /// (SIGSEGV, « corrupted double-linked list ») : `alphamerge` exige deux
    /// entrées de dimensions identiques. Si un filtre situé en amont est
    /// redimensionnable en direct, les deux entrées divergent le temps que les
    /// commandes arrivent, et le tas est corrompu.
    ///
    /// Aucun filtre nommé — donc pilotable — ne doit se trouver en amont de
    /// `alphamerge`.
    #[test]
    fn no_resizable_filter_feeds_alphamerge() {
        let dir = std::env::temp_dir().join("fastcap-test");
        let assets = SessionAssets {
            dir: Some(dir.clone()),
            cam_mask: Some(dir.join("shapes.png")),
            cam_decoration: Some(dir.join("cam-shadow.png")),
            ..Default::default()
        };

        let args = plan_for(
            &assets,
            Some(&style(Layout::PipBottomRight)),
            Some("/dev/video0"),
            &[],
            Backdrop::None,
        );
        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|i| args.get(i + 1))
            .expect("graphe");

        // Tout ce qui précède `alphamerge` doit être à dimensions figées
        let upstream = graph
            .split("alphamerge")
            .next()
            .expect("partie amont du graphe");

        assert!(
            !upstream.contains("scale@"),
            "un `scale` pilotable alimente alphamerge — ffmpeg planterait :\n{upstream}"
        );
        assert!(
            !upstream.contains("crop@camcrop"),
            "un `crop` pilotable alimente alphamerge :\n{upstream}"
        );

        // `crop@shape` est autorisé : seule son ordonnée change, jamais sa taille
        assert!(upstream.contains("crop@shape"), "obtenu: {upstream}");

        // Le redimensionnement pilotable doit exister, mais en aval du masquage
        let downstream = graph
            .rsplit("alphamerge")
            .next()
            .expect("partie aval du graphe");
        assert!(
            downstream.contains("scale@camsize"),
            "le redimensionnement doit suivre le masquage :\n{downstream}"
        );
    }

    /// Affiche le graphe réellement produit, pour le rejouer à la main.
    #[test]
    #[ignore = "diagnostic"]
    fn dump_the_generated_filter_graph() {
        let dir = std::env::temp_dir().join("fastcap-test");
        let assets = SessionAssets {
            dir: Some(dir.clone()),
            cam_mask: Some(dir.join("shapes.png")),
            cam_decoration: Some(dir.join("cam-shadow.png")),
            cam_border: Some(dir.join("cam-border.png")),
            ..Default::default()
        };
        let mut cam = style(Layout::PipBottomRight);
        cam.presentation = Presentation::Classic;

        let args = plan_for(&assets, Some(&cam), Some("/dev/video0"), &[], Backdrop::None);
        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|i| args.get(i + 1))
            .expect("graphe");
        eprintln!("{graph}");
    }

    /// Le médaillon reste carré : la caméra est masquée à une taille de
    /// référence carrée, un médaillon rectangulaire écraserait l'image.
    #[test]
    fn the_camera_pip_is_always_square() {
        for shape in [Shape::Square, Shape::Rounded, Shape::Circle] {
            for presentation in [Presentation::Minimal, Presentation::Bubble] {
                let mut cam = style(Layout::PipBottomRight);
                cam.shape = shape;
                cam.presentation = presentation;

                let (w, h) = cam_size(&cam, (1920, 1080));
                assert_eq!(w, h, "médaillon non carré pour {shape:?} / {presentation:?}");
            }
        }
    }

    #[test]
    fn side_by_side_stacks_screen_and_camera() {
        let graph = plan_for(
            &SessionAssets::default(),
            Some(&style(Layout::SideRight)),
            Some("/dev/video0"),
            &[],
            Backdrop::None,
        )
        .join(" ");

        assert!(graph.contains("hstack"), "obtenu: {graph}");
    }

    #[test]
    fn full_layout_uses_the_camera_alone() {
        let graph = plan_for(
            &SessionAssets::default(),
            Some(&style(Layout::Full)),
            Some("/dev/video0"),
            &[],
            Backdrop::None,
        )
        .join(" ");

        assert!(graph.contains("[vfull]"), "obtenu: {graph}");
    }

    #[test]
    fn hardware_encoding_uploads_at_the_very_end() {
        let encoder = Encoder::Vaapi {
            device: "/dev/dri/renderD128".into(),
        };
        let args = build_plan(
            &area(),
            (1280, 720),
            30,
            "balanced",
            &encoder,
            &SessionAssets::default(),
            None,
            None,
            &[],
            Backdrop::None,
            false,
            true,
            Path::new("/tmp/out.mp4"),
        )
        .args;

        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|i| args.get(i + 1))
            .expect("graphe");

        assert!(graph.ends_with("format=nv12,hwupload[vout]"), "obtenu: {graph}");
    }

    #[test]
    fn camera_geometry_stays_inside_the_frame() {
        let style = style(Layout::PipBottomRight);
        let output = (1280, 720);
        let cam = cam_size(&style, output);
        let (x, y) = cam_position(&style, output, cam);

        assert!(x >= 0 && y >= 0);
        assert!(x + cam.0 as i32 <= output.0 as i32);
        assert!(y + cam.1 as i32 <= output.1 as i32);
    }

    #[test]
    fn circle_and_bubble_cameras_are_square() {
        let mut circle = style(Layout::PipBottomRight);
        circle.shape = Shape::Circle;
        let (w, h) = cam_size(&circle, (1920, 1080));
        assert_eq!(w, h);

        let mut bubble = style(Layout::PipBottomRight);
        bubble.presentation = Presentation::Bubble;
        let (w, h) = cam_size(&bubble, (1920, 1080));
        assert_eq!(w, h);
    }

    /// Diagnostic : ffmpeg est-il détecté, et en combien de temps ?
    #[test]
    #[ignore = "diagnostic"]
    fn diagnose_ffmpeg_detection() {
        let t = Instant::now();
        let available = is_available();
        eprintln!("  → is_available() = {available} en {:?}", t.elapsed());

        let t = Instant::now();
        let version = version();
        eprintln!("  → version() = {version:?} en {:?}", t.elapsed());

        let t = Instant::now();
        let cams = list_cameras();
        eprintln!("  → {} caméra(s) en {:?}", cams.len(), t.elapsed());

        let t = Instant::now();
        let audio = list_audio_devices();
        eprintln!("  → {} micro(s) en {:?}", audio.len(), t.elapsed());

        let t = Instant::now();
        let enc = crate::encoder::select(1280, 720, 30, true);
        eprintln!("  → encodeur {} en {:?}", enc.label(), t.elapsed());
    }

    /// Martèle le pilotage en direct pendant un enregistrement réel.
    ///
    /// C'est le scénario qui faisait planter ffmpeg (SIGSEGV sur corruption de
    /// tas) : on enchaîne redimensionnements, changements de forme, échanges
    /// de rôles et masquages sans laisser le graphe respirer.
    #[test]
    #[ignore = "nécessite un écran, une caméra et ffmpeg"]
    fn hammering_the_live_controls_never_crashes_ffmpeg() {
        let Some(camera) = list_cameras().into_iter().next() else {
            eprintln!("  (aucune caméra, test ignoré)");
            return;
        };

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: Some(WebcamOptions {
                device: camera.id,
                layout: "pip-br".to_string(),
                shape: "rounded".to_string(),
                // « classique » ajoute ombre et liseré : trois filtres
                // redimensionnables de plus à éprouver.
                presentation: "classic".to_string(),
                size_percent: 25,
                margin: 24,
            }),
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let mut session =
            start(options, std::env::temp_dir().join("fastcap-smoke")).expect("démarrage");

        let actions = [
            LiveAction::Resize(10),
            LiveAction::NextShape,
            LiveAction::Resize(-15),
            LiveAction::NextCorner,
            LiveAction::ToggleSwap,
            LiveAction::Resize(20),
            LiveAction::NextShape,
            LiveAction::ToggleSwap,
            LiveAction::ToggleCamera,
            LiveAction::ToggleCamera,
            LiveAction::NextCorner,
            LiveAction::Resize(-10),
        ];

        for action in actions {
            session
                .apply_live(action)
                .expect("ajustement accepté pendant l'enregistrement");
            std::thread::sleep(Duration::from_millis(180));
        }

        // Journal de ffmpeg avant l'arrêt : c'est là que se lit la cause
        let journal = session.errors.tail(12);
        let encoder_used = session.encoder_label.clone();
        let info = stop(session).expect("arrêt après matraquage");
        if !journal.is_empty() {
            eprintln!("  → ffmpeg ({encoder_used}) : {journal}");
        }

        let probe = Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "stream=codec_name,nb_frames", "-of",
                "default=nw=1", &info.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout).to_string();
        let _ = std::fs::remove_file(&info.path);

        eprintln!(
            "  → 12 ajustements enchaînés : {} Ko, {:.1} i/s",
            info.size_bytes / 1024,
            info.captured_fps
        );

        assert!(
            report.contains("codec_name=h264"),
            "fichier illisible après pilotage : {report}"
        );
        assert!(info.size_bytes > 1024, "aucune image enregistrée");
    }

    /// Reproduit fidèlement le bug « Device or resource busy » : l'aperçu
    /// interroge la caméra en boucle (comme l'interface toutes les 300 ms)
    /// pendant que l'enregistrement démarre. La réservation doit empêcher
    /// toute réouverture concurrente du périphérique.
    #[test]
    #[ignore = "nécessite une caméra"]
    fn a_polling_preview_cannot_steal_the_camera_from_a_starting_recording() {
        let Some(camera) = list_cameras().into_iter().next() else {
            eprintln!("  (aucune caméra, test ignoré)");
            return;
        };

        let device = camera.id.clone();
        let polling = Arc::new(AtomicBool::new(true));

        // L'aperçu tient la caméra au départ
        let preview = webcam::acquire(&device).expect("aperçu");
        preview.wait_ready(Duration::from_secs(6)).expect("image");
        drop(preview);

        // …et continue de la solliciter, comme le fait l'interface
        let poller = {
            let device = device.clone();
            let polling = polling.clone();
            std::thread::spawn(move || {
                let mut reopened = 0u32;
                while polling.load(Ordering::Relaxed) {
                    if webcam_snapshot(&device, 320).is_ok() {
                        reopened += 1;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                reopened
            })
        };

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: Some(WebcamOptions {
                device: device.clone(),
                layout: "pip-br".to_string(),
                shape: "rounded".to_string(),
                presentation: "minimal".to_string(),
                size_percent: 25,
                margin: 24,
            }),
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let outcome = start(options, std::env::temp_dir().join("fastcap-smoke"));
        polling.store(false, Ordering::Relaxed);
        let reopened = poller.join().unwrap_or(0);

        let session = outcome.expect("l'enregistrement doit démarrer malgré l'aperçu");
        eprintln!("  → démarrage réussi ({reopened} aperçus servis pendant la réservation)");

        std::thread::sleep(Duration::from_secs(2));
        let info = stop(session).expect("arrêt");
        let _ = std::fs::remove_file(&info.path);

        assert!(info.size_bytes > 1024, "aucune image enregistrée");

        // Une fois l'enregistrement terminé, l'aperçu doit redevenir possible
        let again = webcam::acquire(&device).expect("aperçu de nouveau disponible");
        again.wait_ready(Duration::from_secs(6)).expect("image après arrêt");
        eprintln!("  → aperçu de nouveau opérationnel après l'arrêt");
    }

    /// Pilotage en direct : déplacer, masquer, changer de forme et échanger
    /// les rôles pendant l'enregistrement ne doit ni interrompre ffmpeg ni
    /// corrompre le fichier produit.
    #[test]
    #[ignore = "nécessite un écran, une caméra et ffmpeg"]
    fn the_overlay_can_be_driven_while_recording() {
        let Some(camera) = list_cameras().into_iter().next() else {
            eprintln!("  (aucune caméra, test ignoré)");
            return;
        };

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: Some(WebcamOptions {
                device: camera.id,
                layout: "pip-br".to_string(),
                shape: "rounded".to_string(),
                presentation: "minimal".to_string(),
                size_percent: 25,
                margin: 24,
            }),
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let mut session =
            start(options, std::env::temp_dir().join("fastcap-smoke")).expect("démarrage");

        // Chaque ajustement doit être accepté et refléter le nouvel état
        std::thread::sleep(Duration::from_millis(700));
        let state = session.apply_live(LiveAction::NextCorner).expect("coin suivant");
        assert_eq!(state.layout, "pip-bl");

        std::thread::sleep(Duration::from_millis(500));
        let state = session.apply_live(LiveAction::NextShape).expect("forme suivante");
        assert_eq!(state.shape, "circle");

        std::thread::sleep(Duration::from_millis(500));
        let state = session.apply_live(LiveAction::Resize(10)).expect("agrandir");
        assert_eq!(state.size_percent, 35);

        std::thread::sleep(Duration::from_millis(500));
        let state = session.apply_live(LiveAction::ToggleSwap).expect("échange");
        assert!(state.swapped);

        std::thread::sleep(Duration::from_millis(500));
        let state = session.apply_live(LiveAction::ToggleCamera).expect("masquer");
        assert!(!state.camera_visible);

        std::thread::sleep(Duration::from_millis(500));
        let info = stop(session).expect("arrêt après pilotage");

        eprintln!(
            "  → {} Ko, {:.1} i/s après 5 ajustements en direct",
            info.size_bytes / 1024,
            info.captured_fps
        );

        // Le fichier doit rester parfaitement lisible
        let probe = Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "stream=codec_name,nb_frames", "-of",
                "default=nw=1", &info.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout).to_string();
        let _ = std::fs::remove_file(&info.path);

        assert!(report.contains("codec_name=h264"), "obtenu: {report}");
        assert!(info.captured_fps > 5.0, "cadence effondrée: {}", info.captured_fps);
    }

    /// Reproduit le scénario de l'interface : l'aperçu tient la caméra, puis
    /// l'utilisateur lance l'enregistrement. Le périphérique v4l2 étant
    /// exclusif, la bascule doit être fiable.
    #[test]
    #[ignore = "nécessite une caméra"]
    fn recording_starts_right_after_the_preview_released_the_camera() {
        let Some(camera) = list_cameras().into_iter().next() else {
            eprintln!("  (aucune caméra, test ignoré)");
            return;
        };

        // 1. L'aperçu ouvre la caméra
        let preview = webcam::acquire(&camera.id).expect("ouverture aperçu");
        preview.wait_ready(Duration::from_secs(6)).expect("première image");
        eprintln!("  → aperçu actif sur {}", camera.id);

        // 2. L'utilisateur clique sur Démarrer : l'aperçu est relâché
        drop(preview);

        // 3. ffmpeg tente aussitôt d'ouvrir la même caméra
        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: Some(WebcamOptions {
                device: camera.id.clone(),
                layout: "pip-br".to_string(),
                shape: "rounded".to_string(),
                presentation: "minimal".to_string(),
                size_percent: 25,
                margin: 24,
            }),
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let began = Instant::now();
        let session = start(options, std::env::temp_dir().join("fastcap-smoke"))
            .expect("démarrage juste après l'aperçu");
        eprintln!("  → démarrage en {:?}", began.elapsed());

        std::thread::sleep(Duration::from_secs(3));
        let info = stop(session).expect("arrêt");
        eprintln!("  → {} Ko, {:.1} i/s", info.size_bytes / 1024, info.captured_fps);
        let _ = std::fs::remove_file(&info.path);

        assert!(info.size_bytes > 1024, "aucune image enregistrée");
    }

    /// Vérifie que l'identifiant de fenêtre fourni par `xcap` est bien celui
    /// que le grabber natif attend : c'est l'hypothèse sur laquelle repose la
    /// source « fenêtre ».
    #[test]
    #[ignore = "nécessite un écran et ffmpeg"]
    fn window_ids_are_understood_by_the_native_grabber() {
        let windows = crate::capture::enumerate_windows().expect("énumération");
        let Some(window) = windows
            .into_iter()
            .find(|w| !w.is_minimized && w.width > 200 && w.height > 100)
        else {
            eprintln!("  (aucune fenêtre exploitable, test ignoré)");
            return;
        };

        eprintln!(
            "  → fenêtre « {} » id={} ({}x{})",
            window.title, window.id, window.width, window.height
        );

        let options = RecordingOptions {
            source: "window".to_string(),
            window_id: Some(window.id),
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: None,
            hide_app: false,
            timer_in_video: false,
            capture_cursor: false,
            output_dir: None,
        };

        let directory = std::env::temp_dir().join("fastcap-smoke");
        let session = start(options, directory).expect("démarrage sur la fenêtre");
        std::thread::sleep(Duration::from_secs(2));
        let info = stop(session).expect("arrêt");

        eprintln!("  → {} Ko, {:.1} i/s", info.size_bytes / 1024, info.captured_fps);
        let _ = std::fs::remove_file(&info.path);

        assert!(info.size_bytes > 1024, "aucune image capturée sur la fenêtre");
    }

    /// Enregistre une zone : vérifie que les dimensions demandées sont
    /// exactement celles du fichier produit.
    #[test]
    #[ignore = "nécessite un écran et ffmpeg"]
    fn a_region_recording_has_the_requested_dimensions() {
        let options = RecordingOptions {
            source: "region".to_string(),
            window_id: None,
            monitor: None,
            region: Some(RegionRect {
                x: 40,
                y: 60,
                width: 640,
                height: 360,
            }),
            fps: 15,
            quality: "small".to_string(),
            resolution: "native".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: None,
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let directory = std::env::temp_dir().join("fastcap-smoke");
        let session = start(options, directory).expect("démarrage sur la zone");
        std::thread::sleep(Duration::from_secs(2));
        let info = stop(session).expect("arrêt");

        let probe = Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "stream=width,height", "-of",
                "default=nw=1", &info.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout).to_string();
        let _ = std::fs::remove_file(&info.path);

        eprintln!("  → zone : {}", report.trim().replace('\n', " "));
        assert!(report.contains("width=640"), "obtenu: {report}");
        assert!(report.contains("height=360"), "obtenu: {report}");
    }

    /// Enregistre réellement avec toutes les décorations activées : c'est le
    /// graphe de filtres le plus complexe (arrière-plan dégradé + encart
    /// arrondi + webcam masquée + ombre + liseré + minuteur).
    ///
    /// Nécessite un écran, une webcam et ffmpeg.
    #[test]
    #[ignore = "nécessite un écran, une webcam et ffmpeg"]
    fn a_decorated_recording_produces_a_playable_file() {
        let Some(camera) = list_cameras().into_iter().next() else {
            eprintln!("  (aucune caméra détectée, test ignoré)");
            return;
        };

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: 15,
            quality: "small".to_string(),
            resolution: "480".to_string(),
            backdrop: Some("aurora".to_string()),
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: Some(WebcamOptions {
                device: camera.id,
                layout: "pip-br".to_string(),
                shape: "circle".to_string(),
                presentation: "bubble".to_string(),
                size_percent: 25,
                margin: 24,
            }),
            hide_app: false,
            timer_in_video: true,
            capture_cursor: true,
            output_dir: None,
        };

        let directory = std::env::temp_dir().join("fastcap-smoke");
        let session = start(options, directory).expect("démarrage");

        std::thread::sleep(Duration::from_secs(3));
        let info = stop(session).expect("arrêt");

        let probe = Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "stream=codec_name,width,height,nb_frames",
                "-of", "default=nw=1", &info.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout).to_string();

        eprintln!(
            "  → décoré : {:.1} i/s · {} Ko\n{}",
            info.captured_fps,
            info.size_bytes / 1024,
            report.trim()
        );

        let _ = std::fs::remove_file(&info.path);

        assert!(report.contains("codec_name=h264"), "obtenu: {report}");
        assert!(report.contains("height=480"), "obtenu: {report}");
        assert!(info.size_bytes > 1024, "fichier trop petit");
    }

    /// Enregistrement réel de bout en bout : la durée de la vidéo doit
    /// correspondre au temps écoulé, et la cadence être effectivement tenue.
    ///
    /// Nécessite un écran et ffmpeg : `cargo test --release -- --ignored`.
    #[test]
    #[ignore = "nécessite un écran et ffmpeg"]
    fn a_real_recording_is_realtime_and_correctly_timed() {
        const SECONDS: f64 = 4.0;
        const FPS: u32 = 24;

        let options = RecordingOptions {
            source: "fullscreen".to_string(),
            window_id: None,
            monitor: None,
            region: None,
            fps: FPS,
            quality: "small".to_string(),
            resolution: "720".to_string(),
            backdrop: None,
            audio: false,
            audio_device: None,
            system_audio_device: None,
            webcam: None,
            hide_app: false,
            timer_in_video: false,
            capture_cursor: true,
            output_dir: None,
        };

        let directory = std::env::temp_dir().join("fastcap-smoke");
        let session = start(options, directory).expect("démarrage de l'enregistrement");

        std::thread::sleep(Duration::from_secs_f64(SECONDS));
        let info = stop(session).expect("arrêt de l'enregistrement");

        let probe = Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "format=duration", "-of",
                "default=nw=1:nk=1", &info.path,
            ])
            .output()
            .expect("ffprobe");
        let duration: f64 = String::from_utf8_lossy(&probe.stdout)
            .trim()
            .parse()
            .expect("durée lisible");

        let _ = std::fs::remove_file(&info.path);

        eprintln!(
            "  → {duration:.2}s de vidéo pour {SECONDS:.2}s réelles · \
             {:.1} i/s · {} · {} Ko",
            info.captured_fps,
            info.encoder,
            info.size_bytes / 1024
        );

        assert!(
            (duration - SECONDS).abs() < SECONDS * 0.2,
            "durée {duration:.2}s au lieu de {SECONDS:.2}s"
        );

        // La cadence réelle doit approcher la cadence demandée : c'est tout
        // l'intérêt de la capture native.
        assert!(
            info.captured_fps > FPS as f32 * 0.7,
            "cadence trop basse: {:.1} i/s pour {FPS} demandées",
            info.captured_fps
        );
    }
}
