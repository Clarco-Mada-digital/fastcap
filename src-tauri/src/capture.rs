// FastCap - Capture d'écran cross-platform (Windows / macOS / Linux) via xcap

use crate::image_utils::to_png_data_url;
use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// Résultat d'une capture d'écran envoyé au frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResult {
    pub id: String,
    pub timestamp: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub data_url: String,
    pub path: Option<String>,
}

/// Description d'une fenêtre ouvrable à la capture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_minimized: bool,
    pub is_maximized: bool,
}

/// Description d'un écran
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f32,
}

impl CaptureResult {
    fn from_image(img: &RgbaImage) -> Result<Self, String> {
        let data_url = to_png_data_url(img)?;
        Ok(Self {
            id: generate_capture_id(),
            timestamp: generate_timestamp(),
            width: img.width(),
            height: img.height(),
            format: "png".to_string(),
            data_url,
            path: None,
        })
    }
}

/// Capture l'écran principal
pub fn capture_fullscreen() -> Result<CaptureResult, String> {
    let (image, _, _) = capture_primary_raw()?;
    CaptureResult::from_image(&image)
}

/// Capture brute de l'écran principal : image + position du moniteur.
/// Utilisée pour la sélection de région (image figée).
pub fn capture_primary_raw() -> Result<(RgbaImage, i32, i32), String> {
    let monitor = primary_monitor()?;
    let image = monitor
        .capture_image()
        .map_err(|e| format!("Capture impossible: {e}"))?;
    let x = monitor.x().unwrap_or(0);
    let y = monitor.y().unwrap_or(0);
    Ok((image, x, y))
}

/// Capture l'écran contenant la région demandée, puis découpe la région
pub fn capture_region(x: i32, y: i32, width: u32, height: u32) -> Result<CaptureResult, String> {
    if width == 0 || height == 0 {
        return Err("La région de capture est vide".to_string());
    }

    let monitor = monitor_containing(x, y).or_else(|_| primary_monitor())?;
    let screen = monitor
        .capture_image()
        .map_err(|e| format!("Capture impossible: {e}"))?;

    // Coordonnées du moniteur, pour ramener la région en coordonnées locales
    let origin_x = monitor.x().unwrap_or(0);
    let origin_y = monitor.y().unwrap_or(0);

    let local_x = (x - origin_x).max(0) as u32;
    let local_y = (y - origin_y).max(0) as u32;
    let w = width.min(screen.width().saturating_sub(local_x));
    let h = height.min(screen.height().saturating_sub(local_y));

    if w == 0 || h == 0 {
        return Err("La région demandée est en dehors de l'écran".to_string());
    }

    let cropped = image::imageops::crop_imm(&screen, local_x, local_y, w, h).to_image();
    CaptureResult::from_image(&cropped)
}

/// Capture une fenêtre précise par son identifiant
pub fn capture_window(window_id: u32) -> Result<CaptureResult, String> {
    let windows = xcap::Window::all().map_err(|e| format!("Énumération impossible: {e}"))?;

    let window = windows
        .into_iter()
        .find(|w| w.id().map(|id| id == window_id).unwrap_or(false))
        .ok_or_else(|| format!("Fenêtre {window_id} introuvable"))?;

    if window.is_minimized().unwrap_or(false) {
        return Err("La fenêtre est réduite, restaurez-la avant la capture".to_string());
    }

    let image = window
        .capture_image()
        .map_err(|e| format!("Capture de la fenêtre impossible: {e}"))?;

    CaptureResult::from_image(&image)
}

/// Liste les fenêtres capturables
pub fn enumerate_windows() -> Result<Vec<WindowInfo>, String> {
    let windows = xcap::Window::all().map_err(|e| format!("Énumération impossible: {e}"))?;

    let mut list: Vec<WindowInfo> = windows
        .into_iter()
        .filter_map(|w| {
            let title = w.title().unwrap_or_default();
            // Ignorer les fenêtres sans titre (surfaces internes du système)
            if title.trim().is_empty() {
                return None;
            }
            let width = w.width().unwrap_or(0);
            let height = w.height().unwrap_or(0);
            if width == 0 || height == 0 {
                return None;
            }

            Some(WindowInfo {
                id: w.id().unwrap_or(0),
                title,
                app_name: w.app_name().unwrap_or_default(),
                x: w.x().unwrap_or(0),
                y: w.y().unwrap_or(0),
                width,
                height,
                is_minimized: w.is_minimized().unwrap_or(false),
                is_maximized: w.is_maximized().unwrap_or(false),
            })
        })
        .collect();

    list.sort_by(|a, b| a.app_name.cmp(&b.app_name).then(a.title.cmp(&b.title)));
    Ok(list)
}

/// Liste les écrans disponibles
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("Énumération impossible: {e}"))?;

    Ok(monitors
        .into_iter()
        .enumerate()
        .map(|(index, m)| MonitorInfo {
            index,
            name: m.name().unwrap_or_else(|_| format!("Écran {index}")),
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
            is_primary: m.is_primary().unwrap_or(false),
            scale_factor: m.scale_factor().unwrap_or(1.0),
        })
        .collect())
}

fn primary_monitor() -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("Énumération impossible: {e}"))?;

    monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .cloned()
        .ok_or_else(|| "Aucun écran détecté".to_string())
}

fn monitor_containing(x: i32, y: i32) -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("Énumération impossible: {e}"))?;

    monitors
        .into_iter()
        .find(|m| {
            let mx = m.x().unwrap_or(0);
            let my = m.y().unwrap_or(0);
            let mw = m.width().unwrap_or(0) as i32;
            let mh = m.height().unwrap_or(0) as i32;
            x >= mx && x < mx + mw && y >= my && y < my + mh
        })
        .ok_or_else(|| "Aucun écran ne contient cette région".to_string())
}

/// Génère un identifiant unique pour une capture
pub fn generate_capture_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Génère un timestamp ISO 8601
pub fn generate_timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
