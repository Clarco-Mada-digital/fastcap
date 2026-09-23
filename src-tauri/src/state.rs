// FastCap - Gestion de l'état de l'application (réglages + historique)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Un enregistrement de capture dans l'historique
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub id: String,
    pub timestamp: String,
    pub filename: String,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub size_bytes: u64,
}

/// Paramètres de l'application
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// "png" | "jpeg"
    pub default_export_format: String,
    /// 0-100, utilisé pour JPEG
    pub default_export_quality: u8,
    pub default_save_path: PathBuf,
    pub auto_save: bool,
    pub auto_copy_to_clipboard: bool,
    pub screenshot_shortcut: String,
    pub capture_on_startup: bool,
    /// "light" | "dark" | "system"
    pub theme: String,
    /// Couleur d'accent de l'interface (hexadécimale, ex. "#22d3ee")
    pub accent_color: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_export_format: "png".to_string(),
            default_export_quality: 95,
            default_save_path: default_captures_dir(),
            auto_save: true,
            auto_copy_to_clipboard: true,
            screenshot_shortcut: "PrintScreen".to_string(),
            capture_on_startup: false,
            theme: "dark".to_string(),
            accent_color: "#22d3ee".to_string(),
        }
    }
}

/// Dossier où les captures sont enregistrées
pub fn default_captures_dir() -> PathBuf {
    dirs::picture_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("FastCap")
}

/// Dossier de configuration (réglages + historique)
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FastCap")
}

/// Capture figée en attente d'une sélection de région
pub struct PendingRegion {
    /// Image complète de l'écran (résolution physique)
    pub image: image::RgbaImage,
    /// Même image, prête à afficher dans la fenêtre de sélection
    pub data_url: String,
    /// Position du moniteur capturé, en coordonnées globales
    pub monitor_x: i32,
    pub monitor_y: i32,
    /// true : la sélection sert à délimiter une zone d'enregistrement
    pub for_recording: bool,
}

/// Un enregistrement vidéo terminé
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingRecord {
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
}

/// État global de l'application
pub struct AppState {
    pub settings: AppSettings,
    pub capture_history: Vec<CaptureRecord>,
    pub recordings: Vec<RecordingRecord>,
    pub captures_dir: PathBuf,
    pub pending_region: Option<PendingRegion>,
    pub recording: Option<crate::recorder::RecordingSession>,
    /// Un démarrage est en cours.
    ///
    /// Le lancement prend une à deux secondes (sondes, ouverture des
    /// périphériques) : sans ce verrou, deux clics rapprochés lançaient deux
    /// processus ffmpeg, dont le premier restait orphelin à filmer
    /// indéfiniment.
    pub recording_starting: bool,
    /// Caméra maintenue ouverte pour l'aperçu de l'interface.
    ///
    /// Conserver la poignée évite de rouvrir le périphérique à chaque
    /// vignette : une ouverture v4l2 coûte plusieurs centaines de ms.
    pub preview_camera: Option<(String, crate::webcam::CameraHandle)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let mut captures_dir = default_captures_dir();
        let mut settings = AppSettings::default();
        let mut capture_history = Vec::new();

        // Restaurer les réglages persistés
        let settings_path = config_dir().join("settings.json");
        if let Ok(raw) = std::fs::read_to_string(&settings_path) {
            if let Ok(loaded) = serde_json::from_str::<AppSettings>(&raw) {
                settings = loaded;
            }
        }

        // Restaurer l'historique persisté
        let history_path = config_dir().join("history.json");
        if let Ok(raw) = std::fs::read_to_string(&history_path) {
            if let Ok(loaded) = serde_json::from_str::<Vec<CaptureRecord>>(&raw) {
                capture_history = loaded;
            }
        }

        // Restaurer les enregistrements persistés
        let mut recordings = Vec::new();
        let recordings_path = config_dir().join("recordings.json");
        if let Ok(raw) = std::fs::read_to_string(&recordings_path) {
            if let Ok(loaded) = serde_json::from_str::<Vec<RecordingRecord>>(&raw) {
                recordings = loaded;
            }
        }

        // Le dossier de sauvegarde choisi par l'utilisateur est prioritaire
        if settings.default_save_path.as_os_str().is_empty() {
            settings.default_save_path = captures_dir.clone();
        } else {
            captures_dir = settings.default_save_path.clone();
        }

        let _ = std::fs::create_dir_all(&captures_dir);

        Self {
            settings,
            capture_history,
            recordings,
            captures_dir,
            pending_region: None,
            recording: None,
            recording_starting: false,
            preview_camera: None,
        }
    }

    /// Ajouter un enregistrement en tête de liste
    pub fn add_recording(&mut self, record: RecordingRecord) {
        self.recordings.insert(0, record);
        self.recordings.truncate(100);
    }

    /// Ajouter une capture en tête de l'historique
    pub fn add_capture(&mut self, record: CaptureRecord) {
        self.capture_history.insert(0, record);
        // Conserver les 200 dernières captures
        self.capture_history.truncate(200);
    }

    /// Supprimer une capture de l'historique
    pub fn remove_capture(&mut self, id: &str) {
        self.capture_history.retain(|r| r.id != id);
    }

    /// Persiste les réglages sur le disque
    pub fn save_settings(&self) -> Result<(), String> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&self.settings).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("settings.json"), json).map_err(|e| e.to_string())
    }

    /// Persiste l'historique sur le disque
    pub fn save_history(&self) -> Result<(), String> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&self.capture_history).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("history.json"), json).map_err(|e| e.to_string())
    }

    /// Persiste la liste des enregistrements
    pub fn save_recordings(&self) -> Result<(), String> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&self.recordings).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("recordings.json"), json).map_err(|e| e.to_string())
    }
}
