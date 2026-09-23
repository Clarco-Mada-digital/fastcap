// FastCap - Commandes Tauri exposées au frontend

use crate::capture::{self, CaptureResult, MonitorInfo, WindowInfo};
use crate::image_utils::{self, Annotation};
use crate::ocr::{self, OcrOutcome};
use crate::recorder;
use crate::state::{AppSettings, AppState, CaptureRecord};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

/// Délai laissé au gestionnaire de fenêtres pour masquer l'application
const HIDE_DELAY_MS: u64 = 200;

// --- Types échangés avec le frontend ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsResponse {
    pub settings: AppSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureHistoryResponse {
    pub captures: Vec<CaptureRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOptions {
    /// "png" | "jpeg"
    pub format: String,
    /// 0-100, utilisé pour JPEG
    pub quality: Option<u8>,
    pub path: Option<String>,
    pub filename: Option<String>,
}

// --- Informations et réglages ---

#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: env!("CARGO_PKG_DESCRIPTION").to_string(),
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, Mutex<AppState>>) -> Result<SettingsResponse, String> {
    let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    Ok(SettingsResponse {
        settings: state.settings.clone(),
    })
}

#[tauri::command]
pub fn save_settings(
    state: State<'_, Mutex<AppState>>,
    settings: AppSettings,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    state.captures_dir = settings.default_save_path.clone();
    std::fs::create_dir_all(&state.captures_dir).map_err(|e| e.to_string())?;
    state.settings = settings;
    state.save_settings()
}

// --- Historique ---

#[tauri::command]
pub fn get_capture_history(
    state: State<'_, Mutex<AppState>>,
) -> Result<CaptureHistoryResponse, String> {
    let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    Ok(CaptureHistoryResponse {
        captures: state.capture_history.clone(),
    })
}

#[tauri::command]
pub fn clear_capture_history(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    state.capture_history.clear();
    state.save_history()
}

/// Supprime une capture de l'historique et le fichier associé
#[tauri::command]
pub fn delete_capture(state: State<'_, Mutex<AppState>>, id: String) -> Result<(), String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;

    if let Some(record) = state.capture_history.iter().find(|r| r.id == id) {
        let path = PathBuf::from(&record.path);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Suppression impossible: {e}"))?;
        }
    }

    state.remove_capture(&id);
    state.save_history()
}

/// Sauvegarde les modifications apportées à une capture : écrase le même
/// fichier et met à jour l'entrée de l'historique (dimensions, taille).
/// Si la capture n'existait pas encore (auto-sauvegarde désactivée), elle est
/// créée à la volée dans le dossier de captures.
#[tauri::command]
pub fn save_capture_edit(
    state: State<'_, Mutex<AppState>>,
    id: String,
    data_url: String,
) -> Result<CaptureRecord, String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;

    let bytes = image_utils::decode_data_url_bytes(&data_url)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Image invalide: {e}"))?
        .to_rgba8();
    let width = img.width();
    let height = img.height();

    // La capture existe déjà : on écrase son fichier
    if let Some(record) = state.capture_history.iter_mut().find(|r| r.id == id) {
        std::fs::write(&record.path, &bytes).map_err(|e| format!("Écriture impossible: {e}"))?;
        record.width = width;
        record.height = height;
        record.size_bytes = bytes.len() as u64;
        let saved = record.clone();
        state.save_history()?;
        return Ok(saved);
    }

    // Première sauvegarde : nouveau fichier + entrée d'historique
    let filename = format!(
        "capture_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    );
    std::fs::create_dir_all(&state.captures_dir)
        .map_err(|e| format!("Création du dossier impossible: {e}"))?;
    let path = state.captures_dir.join(&filename);
    std::fs::write(&path, &bytes).map_err(|e| format!("Écriture impossible: {e}"))?;

    let record = CaptureRecord {
        id,
        timestamp: crate::capture::generate_timestamp(),
        filename,
        path: path.to_string_lossy().to_string(),
        width,
        height,
        format: "png".to_string(),
        size_bytes: bytes.len() as u64,
    };
    state.add_capture(record.clone());
    state.save_history()?;
    Ok(record)
}

/// Recharge une capture de l'historique sous forme de data URL (vignettes)
#[tauri::command]
pub fn read_capture_image(path: String) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("Lecture impossible: {e}"))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Image invalide: {e}"))?
        .to_rgba8();
    image_utils::to_png_data_url(&img)
}

/// Recharge une capture en tant que vignette réduite (meilleure perf pour l'aperçu)
#[tauri::command]
pub fn read_capture_thumbnail(path: String, max_width: Option<u32>) -> Result<String, String> {
    let max = max_width.unwrap_or(360).clamp(80, 800);
    let bytes = std::fs::read(&path).map_err(|e| format!("Lecture impossible: {e}"))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Image invalide: {e}"))?
        .to_rgba8();
    let thumb = if img.width() > max {
        let ratio = max as f32 / img.width() as f32;
        let height = ((img.height() as f32) * ratio).max(1.0) as u32;
        image::imageops::resize(&img, max, height, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    image_utils::to_jpeg_data_url(&thumb, 80)
}

// --- Capture d'écran ---

#[tauri::command]
pub async fn capture_fullscreen(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> Result<CaptureResult, String> {
    let result = capture_with_window_hidden(&app, capture::capture_fullscreen).await?;
    persist_capture(&state, result)
}

#[tauri::command]
pub async fn capture_region(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<CaptureResult, String> {
    let result =
        capture_with_window_hidden(&app, move || capture::capture_region(x, y, width, height))
            .await?;
    persist_capture(&state, result)
}

#[tauri::command]
pub async fn capture_window(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    window_id: u32,
) -> Result<CaptureResult, String> {
    let result = capture_with_window_hidden(&app, move || capture::capture_window(window_id)).await?;
    persist_capture(&state, result)
}

#[tauri::command]
pub async fn list_windows() -> Result<Vec<WindowInfo>, String> {
    tauri::async_runtime::spawn_blocking(capture::enumerate_windows)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    capture::enumerate_monitors()
}

/// Aperçu réduit de ce qui sera enregistré (ne masque pas l'application)
#[tauri::command]
pub async fn preview_recording_source(
    source: String,
    window_id: Option<u32>,
    region: Option<recorder::RegionRect>,
) -> Result<String, String> {
    // Travail lourd (balayage des fenêtres, capture plein écran) hors du thread principal
    tauri::async_runtime::spawn_blocking(move || {
        let image = match source.as_str() {
            "window" => {
                let id = window_id.ok_or_else(|| "Aucune fenêtre sélectionnée".to_string())?;
                let window = xcap::Window::all()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|w| w.id().map(|wid| wid == id).unwrap_or(false))
                    .ok_or_else(|| "Fenêtre introuvable".to_string())?;
                window.capture_image().map_err(|e| e.to_string())?
            }
            "region" => {
                let rect = region.ok_or_else(|| "Aucune zone définie".to_string())?;
                let (full, _, _) = capture::capture_primary_raw()?;
                let x = (rect.x.max(0) as u32).min(full.width().saturating_sub(2));
                let y = (rect.y.max(0) as u32).min(full.height().saturating_sub(2));
                let w = rect.width.min(full.width() - x).max(2);
                let h = rect.height.min(full.height() - y).max(2);
                image::imageops::crop_imm(&full, x, y, w, h).to_image()
            }
            _ => capture::capture_primary_raw()?.0,
        };

        // Réduire l'aperçu pour ne pas saturer le pont JS
        let max_width = 560u32;
        let thumb = if image.width() > max_width {
            let ratio = max_width as f32 / image.width() as f32;
            let height = ((image.height() as f32) * ratio).max(1.0) as u32;
            image::imageops::resize(
                &image,
                max_width,
                height,
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };

        // JPEG plutôt que PNG : l'encodage sans perte d'une capture plein
        // écran coûtait plusieurs dizaines de millisecondes par rafraîchissement,
        // pour une vignette où la compression ne se voit pas.
        image_utils::to_jpeg_data_url(&thumb, 72)
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- Sélection interactive de région ---

/// Fige l'écran et ouvre la fenêtre de sélection par-dessus
#[tauri::command]
pub async fn start_region_selection(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    for_recording: Option<bool>,
) -> Result<(), String> {
    let for_recording = for_recording.unwrap_or(false);
    // Masquer la fenêtre principale pour qu'elle n'apparaisse pas dans la capture
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
        std::thread::sleep(Duration::from_millis(HIDE_DELAY_MS));
    }

    let (image, monitor_x, monitor_y) = capture::capture_primary_raw()?;

    // Cette image ne sert qu'à afficher l'écran figé pendant la sélection : le
    // découpage final repart de `image`, en pleine qualité. Un PNG plein écran
    // demandait plusieurs centaines de millisecondes avant que la fenêtre de
    // sélection n'apparaisse, ce qui donnait l'impression d'un raccourci lent.
    let data_url = image_utils::to_jpeg_data_url(&image, 92)?;

    {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        state.pending_region = Some(crate::state::PendingRegion {
            image,
            data_url,
            monitor_x,
            monitor_y,
            for_recording,
        });
    }

    // Fermer une éventuelle fenêtre de sélection résiduelle
    if let Some(previous) = app.get_webview_window("region-overlay") {
        let _ = previous.close();
    }

    let monitors = capture::enumerate_monitors()?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first());

    let (w, h) = monitor
        .map(|m| (m.width as f64, m.height as f64))
        .unwrap_or((1920.0, 1080.0));
    let (px, py) = monitor
        .map(|m| (m.x as f64, m.y as f64))
        .unwrap_or((0.0, 0.0));

    tauri::WebviewWindowBuilder::new(
        &app,
        "region-overlay",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("FastCap - Sélection de région")
    .inner_size(w, h)
    .position(px, py)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(true)
    .build()
    .map_err(|e| format!("Ouverture de la sélection impossible: {e}"))?;

    Ok(())
}

// --- Indicateur d'enregistrement à l'écran ---

/// Petite fenêtre flottante « ● REC mm:ss » toujours au premier plan, même
/// quand la fenêtre principale est masquée. Ignore les clics (transparente).
pub(crate) fn show_rec_indicator(
    app: &AppHandle,
    recorded: Option<crate::screen_input::Geometry>,
) {
    if app.get_webview_window("rec-indicator").is_none() {
        let _ = tauri::WebviewWindowBuilder::new(
            app,
            "rec-indicator",
            tauri::WebviewUrl::App("index.html".into()),
        )
        .title("Enregistrement en cours")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .inner_size(330.0, 116.0)
        .transparent(true)
        .shadow(false)
        .build();
    }

    if let Some(window) = app.get_webview_window("rec-indicator") {
        let _ = window.set_ignore_cursor_events(true);

        let (x, y) = indicator_position(recorded);
        let _ = window.set_position(tauri::LogicalPosition::new(x, y));
        let _ = window.show();
    }
}

/// Dimensions de l'indicateur, en points
const INDICATOR_SIZE: (f64, f64) = (330.0, 116.0);

/// Place l'indicateur **hors de la zone filmée** quand c'est possible.
///
/// La capture d'écran native photographie tout ce qui est affiché, y compris
/// nos propres fenêtres : posé sur la zone filmée, l'indicateur se retrouverait
/// incrusté dans la vidéo. On cherche donc un emplacement sur une autre partie
/// du bureau, et l'on ne retombe sur un coin de la zone qu'en dernier recours.
fn indicator_position(recorded: Option<crate::screen_input::Geometry>) -> (f64, f64) {
    let (width, height) = INDICATOR_SIZE;
    let margin = 16.0;

    let monitors = capture::enumerate_monitors().unwrap_or_default();

    let Some(recorded) = recorded else {
        // Sans zone connue : coin supérieur droit de l'écran principal
        let screen = monitors.iter().find(|m| m.is_primary).or(monitors.first());
        return match screen {
            Some(m) => (
                (m.x as f64 + m.width as f64 - width - margin).max(0.0),
                m.y as f64 + margin,
            ),
            None => (margin, margin),
        };
    };

    let intersects = |x: f64, y: f64| {
        let (left, top) = (recorded.x as f64, recorded.y as f64);
        let right = left + recorded.width as f64;
        let bottom = top + recorded.height as f64;

        x < right && x + width > left && y < bottom && y + height > top
    };

    // 1. Un autre écran, entièrement hors du cadre filmé
    for monitor in &monitors {
        let x = (monitor.x as f64 + monitor.width as f64 - width - margin).max(monitor.x as f64);
        let y = monitor.y as f64 + margin;
        if !intersects(x, y) {
            return (x, y);
        }
    }

    // 2. Juste sous la zone filmée, si le bureau le permet
    let below_y = recorded.y as f64 + recorded.height as f64 + margin;
    let below_x = recorded.x as f64;
    let fits_below = monitors.iter().any(|m| {
        below_y + height <= m.y as f64 + m.height as f64 && below_x >= m.x as f64
    });
    if fits_below && !intersects(below_x, below_y) {
        return (below_x, below_y);
    }

    // 3. Dernier recours : dans le cadre. L'indicateur apparaîtra dans la
    //    vidéo — l'utilisateur peut le désactiver dans les réglages.
    (
        (recorded.x as f64 + recorded.width as f64 - width - margin).max(0.0),
        recorded.y as f64 + margin,
    )
}

/// Masque l'indicateur d'enregistrement
pub(crate) fn hide_rec_indicator(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("rec-indicator") {
        let _ = window.hide();
    }
}

#[cfg(test)]
mod indicator_tests {
    use super::*;
    use crate::screen_input::Geometry;

    /// Vrai si l'indicateur, posé en (x, y), mordrait sur la zone filmée
    fn overlaps(position: (f64, f64), area: Geometry) -> bool {
        let (width, height) = INDICATOR_SIZE;
        let (x, y) = position;
        let (left, top) = (area.x as f64, area.y as f64);
        let right = left + area.width as f64;
        let bottom = top + area.height as f64;

        x < right && x + width > left && y < bottom && y + height > top
    }

    /// Filmer une petite zone laisse de la place ailleurs : l'indicateur ne
    /// doit pas se retrouver incrusté dans la vidéo.
    #[test]
    #[ignore = "nécessite un écran"]
    fn the_indicator_avoids_a_small_recorded_area() {
        let area = Geometry {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        };

        let position = indicator_position(Some(area));
        eprintln!("  → zone 400x300 en (0,0) : indicateur en {position:?}");
        assert!(
            !overlaps(position, area),
            "l'indicateur serait filmé : {position:?}"
        );
    }

    /// Sans zone connue, l'indicateur se pose simplement en haut à droite.
    #[test]
    #[ignore = "nécessite un écran"]
    fn the_indicator_has_a_sensible_default_position() {
        let (x, y) = indicator_position(None);
        assert!(x >= 0.0 && y >= 0.0, "position hors écran : {x}, {y}");
    }
}

/// Image figée affichée dans la fenêtre de sélection
#[tauri::command]
pub fn get_region_selection_image(state: State<'_, Mutex<AppState>>) -> Result<String, String> {
    let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    state
        .pending_region
        .as_ref()
        .map(|p| p.data_url.clone())
        .ok_or_else(|| "Aucune sélection en cours".to_string())
}

/// Annule la sélection et rend la main à la fenêtre principale
#[tauri::command]
pub fn cancel_region_selection(app: AppHandle, state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    if let Ok(mut state) = state.lock() {
        state.pending_region = None;
    }
    restore_main_window(&app);
    Ok(())
}

/// Découpe la région choisie, l'enregistre et l'envoie à la fenêtre principale.
/// `x` / `y` / `width` / `height` sont exprimés dans les pixels de l'image capturée.
#[tauri::command]
pub async fn finish_region_selection(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<CaptureResult, String> {
    if width == 0 || height == 0 {
        return Err("Sélection vide".to_string());
    }

    let cropped = {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        let pending = state
            .pending_region
            .take()
            .ok_or_else(|| "Aucune sélection en cours".to_string())?;

        // Sélection destinée à l'enregistrement : on renvoie la zone, sans capture
        if pending.for_recording {
            let rect = crate::recorder::RegionRect {
                x: pending.monitor_x + x,
                y: pending.monitor_y + y,
                width,
                height,
            };

            let app_handle = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                if let Some(overlay) = app_handle.get_webview_window("region-overlay") {
                    let _ = overlay.close();
                }
                if let Some(main) = app_handle.get_webview_window("main") {
                    let _ = main.show();
                    let _ = main.set_focus();
                }
                let _ = app_handle.emit_to("main", "region-selected", rect);
            });

            return Ok(CaptureResult {
                id: capture::generate_capture_id(),
                timestamp: capture::generate_timestamp(),
                width,
                height,
                format: "region".to_string(),
                data_url: String::new(),
                path: None,
            });
        }

        let (iw, ih) = pending.image.dimensions();
        let cx = (x.max(0) as u32).min(iw.saturating_sub(1));
        let cy = (y.max(0) as u32).min(ih.saturating_sub(1));
        let cw = width.min(iw - cx);
        let ch = height.min(ih - cy);

        if cw == 0 || ch == 0 {
            return Err("Sélection en dehors de l'écran".to_string());
        }

        image::imageops::crop_imm(&pending.image, cx, cy, cw, ch).to_image()
    };

    let result = persist_capture(&state, CaptureResult {
        id: capture::generate_capture_id(),
        timestamp: capture::generate_timestamp(),
        width: cropped.width(),
        height: cropped.height(),
        format: "png".to_string(),
        data_url: image_utils::to_png_data_url(&cropped)?,
        path: None,
    })?;

    // La fenêtre de sélection se ferme juste après la réponse à son appel
    let app_handle = app.clone();
    let payload = result.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        if let Some(overlay) = app_handle.get_webview_window("region-overlay") {
            let _ = overlay.close();
        }
        if let Some(main) = app_handle.get_webview_window("main") {
            let _ = main.show();
            let _ = main.set_focus();
        }
        let _ = app_handle.emit_to("main", "region-captured", payload);
    });

    Ok(result)
}

fn restore_main_window(app: &AppHandle) {
    if let Some(overlay) = app.get_webview_window("region-overlay") {
        let _ = overlay.close();
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }
}

// --- Annotation et export ---

#[tauri::command]
pub fn annotate_image(data_url: String, annotations: Vec<Annotation>) -> Result<String, String> {
    let mut img = image_utils::decode_data_url(&data_url)?;
    image_utils::apply_annotations(&mut img, &annotations)?;
    image_utils::to_png_data_url(&img)
}

#[tauri::command]
pub fn export_image(
    state: State<'_, Mutex<AppState>>,
    data_url: String,
    options: ExportOptions,
) -> Result<String, String> {
    let img = image_utils::decode_data_url(&data_url)?;

    let format = options.format.to_lowercase();
    let quality = options.quality.unwrap_or(95);
    let format_ext = match format.as_str() {
        "png" => "png",
        "jpeg" | "jpg" => "jpg",
        other => return Err(format!("Format d'export non supporté: {other}")),
    };

    let filename = options.filename.unwrap_or_else(|| {
        format!(
            "capture_{}.{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            format_ext
        )
    });

    let dir = options
        .path
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            state
                .lock()
                .map(|s| s.captures_dir.clone())
                .unwrap_or_else(|_| crate::state::default_captures_dir())
        });

    std::fs::create_dir_all(&dir).map_err(|e| format!("Création du dossier impossible: {e}"))?;
    let full_path = dir.join(&filename);

    let bytes = match format.as_str() {
        "png" => {
            let data_url = image_utils::to_png_data_url(&img)?;
            image_utils::decode_data_url_bytes(&data_url)?
        }
        _ => {
            let data_url = image_utils::to_jpeg_data_url(&img, quality)?;
            image_utils::decode_data_url_bytes(&data_url)?
        }
    };

    std::fs::write(&full_path, bytes).map_err(|e| format!("Écriture impossible: {e}"))?;
    Ok(full_path.to_string_lossy().to_string())
}

// --- OCR ---

#[tauri::command]
pub fn ocr_image(data_url: String, language: Option<String>) -> Result<OcrOutcome, String> {
    ocr::recognize(&data_url, language.as_deref())
}

#[tauri::command]
pub fn ocr_is_available() -> bool {
    ocr::is_available()
}

#[tauri::command]
pub fn ocr_available_languages() -> Vec<String> {
    ocr::available_languages()
}

// --- Enregistrement vidéo ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingCapabilities {
    pub available: bool,
    pub version: Option<String>,
    pub cameras: Vec<crate::recorder::CaptureDevice>,
    pub audio_devices: Vec<crate::recorder::CaptureDevice>,
    /// Encodeur qui serait retenu, pour informer l'utilisateur
    pub encoder: String,
    pub hardware_encoding: bool,
}

#[tauri::command]
pub async fn recorder_capabilities() -> Result<RecordingCapabilities, String> {
    // Détection des périphériques (ffmpeg/ffprobe) hors du thread principal :
    // ne gèle jamais l'interface même si une caméra est occupée.
    tauri::async_runtime::spawn_blocking(|| {
        let available = recorder::is_available();
        if !available {
            return RecordingCapabilities {
                available: false,
                version: None,
                cameras: Vec::new(),
                audio_devices: Vec::new(),
                encoder: "indisponible".to_string(),
                hardware_encoding: false,
            };
        }

        // Sonde représentative : 720p30, la configuration par défaut
        let encoder = crate::encoder::select(1280, 720, 30, true);

        RecordingCapabilities {
            available: true,
            version: recorder::version(),
            cameras: recorder::list_cameras(),
            audio_devices: recorder::list_audio_devices(),
            hardware_encoding: encoder.is_hardware(),
            encoder: encoder.label(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Démarre un enregistrement vidéo
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    options: recorder::RecordingOptions,
) -> Result<recorder::RecordingStart, String> {
    // Le verrou est posé **avant** toute opération longue, et dans la même
    // section critique que la vérification : deux clics rapprochés ne peuvent
    // donc plus lancer deux processus ffmpeg concurrents.
    let output_dir = {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;

        if state.recording.is_some() || state.recording_starting {
            return Err("Un enregistrement est déjà en cours".to_string());
        }
        state.recording_starting = true;

        // Le périphérique vidéo est exclusif : l'aperçu doit lâcher la caméra
        // avant que ffmpeg ne tente de l'ouvrir pour l'enregistrement.
        state.preview_camera = None;

        options
            .output_dir
            .clone()
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| state.captures_dir.clone())
    };

    let hide_app = options.hide_app;

    // La fenêtre est retirée **avant** que ffmpeg ne commence à filmer :
    // sinon l'application figurait dans les premières images de la vidéo.
    let main_window = app.get_webview_window("main");
    if hide_app {
        if let Some(window) = &main_window {
            let _ = window.hide();
            // Laisser le gestionnaire de fenêtres effectuer le retrait
            std::thread::sleep(Duration::from_millis(HIDE_DELAY_MS));
        }
    }

    // Le démarrage sonde ffmpeg, la caméra et le micro : plusieurs secondes
    // possibles. Hors du fil principal, l'interface ne se fige jamais.
    let started =
        tauri::async_runtime::spawn_blocking(move || recorder::start(options, output_dir)).await;

    // Quoi qu'il arrive, le verrou de démarrage doit être relâché
    let session = match started.map_err(|e| e.to_string()).and_then(|r| r) {
        Ok(session) => session,
        Err(error) => {
            if let Ok(mut state) = state.lock() {
                state.recording_starting = false;
            }
            // L'utilisateur doit revoir l'application pour lire l'erreur
            if let Some(window) = &main_window {
                let _ = window.show();
                let _ = window.set_focus();
            }
            return Err(error);
        }
    };

    let start = recorder::RecordingStart {
        id: session.id.clone(),
        path: session.path.to_string_lossy().to_string(),
        started_ms: session.started_ms,
        width: session.width,
        height: session.height,
        fps: session.fps,
        encoder: session.encoder_label.clone(),
        hardware: !session.encoder_label.starts_with("Logiciel"),
    };

    let recorded_area = session.recorded_area;

    {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        state.recording = Some(session);
        state.recording_starting = false;
    }

    // Indicateur "REC" toujours visible à l'écran pendant l'enregistrement
    show_rec_indicator(&app, Some(recorded_area));

    // État initial de l'incrustation, pour que le retour à l'écran soit
    // renseigné avant même le premier ajustement.
    if let Ok(state) = state.lock() {
        if let Some(live) = state.recording.as_ref().and_then(|s| s.live.clone()) {
            let _ = app.emit_to(
                "rec-indicator",
                "live-feedback",
                live.feedback(&recorder::LiveAction::NextCorner),
            );
        }
    }

    // Retour visuel immédiat : le fichier cible + les moyens d'arrêter
    let name = std::path::Path::new(&start.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&start.path);
    let _ = app
        .notification()
        .builder()
        .title("Enregistrement démarré")
        .body(format!(
            "{name} — arrêtez depuis la zone de notification ou avec Ctrl+Shift+E"
        ))
        .show();

    Ok(start)
}

/// Arrête l'enregistrement en cours et renvoie le fichier finalisé
#[tauri::command]
pub async fn stop_recording(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> Result<recorder::RecordingInfo, String> {
    let session = {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        state
            .recording
            .take()
            .ok_or_else(|| "Aucun enregistrement en cours".to_string())?
    };

    // La finalisation (sortie de ffmpeg, écriture de l'index MP4) prend un
    // temps non négligeable : on ne bloque pas le fil principal.
    let outcome = tauri::async_runtime::spawn_blocking(move || recorder::stop(session))
        .await
        .map_err(|e| e.to_string())?;

    // Toujours réafficher la fenêtre principale, même en cas d'échec
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }

    // Cacher l'indicateur "REC"
    hide_rec_indicator(&app);

    let info = outcome?;

    {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        state.add_recording(crate::state::RecordingRecord {
            id: info.id.clone(),
            path: info.path.clone(),
            filename: info.filename.clone(),
            started_at: info.started_at.clone(),
            duration_ms: info.duration_ms,
            width: info.width,
            height: info.height,
            fps: info.fps,
            size_bytes: info.size_bytes,
            has_webcam: info.has_webcam,
        });
        state.save_recordings()?;
    }

    // Retour visuel de fin d'enregistrement
    let _ = app
        .notification()
        .builder()
        .title("Enregistrement terminé")
        .body(format!("{} — durée {}", info.filename, fmt_duration(info.duration_ms)))
        .show();

    Ok(info)
}

/// Arrête l'enregistrement en cours, sans passer par l'interface.
///
/// Le raccourci global et la zone de notification doivent fonctionner même
/// quand la fenêtre est masquée ou fermée : router l'arrêt par le frontend le
/// rendait dépendant d'une fenêtre vivante et prête à recevoir l'évènement.
///
/// L'appel bloque le temps que ffmpeg finalise le fichier : il est donc lancé
/// depuis un fil dédié.
pub(crate) fn stop_recording_in_background(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        finalize_recording_on_exit(&app);

        hide_rec_indicator(&app);
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    });
}

/// Finalise un enregistrement encore actif au moment de quitter.
///
/// Sans cela, fermer FastCap pendant une capture laissait un processus ffmpeg
/// orphelin continuer à filmer, et le MP4 restait sans index — donc illisible.
pub(crate) fn finalize_recording_on_exit(app: &AppHandle) {
    let state = app.state::<Mutex<AppState>>();

    let session = {
        let Ok(mut state) = state.lock() else {
            return;
        };
        state.recording.take()
    };

    let Some(session) = session else {
        return;
    };

    match recorder::stop(session) {
        Ok(info) => {
            if let Ok(mut state) = state.lock() {
                state.add_recording(crate::state::RecordingRecord {
                    id: info.id.clone(),
                    path: info.path.clone(),
                    filename: info.filename.clone(),
                    started_at: info.started_at.clone(),
                    duration_ms: info.duration_ms,
                    width: info.width,
                    height: info.height,
                    fps: info.fps,
                    size_bytes: info.size_bytes,
                    has_webcam: info.has_webcam,
                });
                let _ = state.save_recordings();
            }
            tracing::info!("Enregistrement finalisé à la fermeture: {}", info.filename);
        }
        Err(error) => tracing::error!("Finalisation impossible à la fermeture: {error}"),
    }
}

/// Formate une durée en millisecondes en « m:ss » ou « h:mm:ss »
pub fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStatus {
    pub active: bool,
    pub elapsed_ms: u64,
    pub path: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub has_webcam: bool,
    /// Encodeur utilisé par la session
    pub encoder: Option<String>,
    /// Compteurs de capture, pour signaler des réglages trop lourds
    pub stats: Option<crate::recorder::StatsSnapshot>,
}

#[tauri::command]
pub fn recording_status(state: State<'_, Mutex<AppState>>) -> Result<RecordingStatus, String> {
    let state = state.lock().map_err(|_| "État verrouillé".to_string())?;

    Ok(match &state.recording {
        Some(session) => RecordingStatus {
            active: true,
            elapsed_ms: session.started.elapsed().as_millis() as u64,
            path: Some(session.path.to_string_lossy().to_string()),
            width: session.width,
            height: session.height,
            fps: session.fps,
            has_webcam: session.has_webcam,
            encoder: Some(session.encoder_label.clone()),
            stats: Some(session.stats_snapshot()),
        },
        None => RecordingStatus {
            active: false,
            elapsed_ms: 0,
            path: None,
            width: 0,
            height: 0,
            fps: 0,
            has_webcam: false,
            encoder: None,
            stats: None,
        },
    })
}

#[tauri::command]
pub fn get_recordings(
    state: State<'_, Mutex<AppState>>,
) -> Result<Vec<crate::state::RecordingRecord>, String> {
    let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    Ok(state.recordings.clone())
}

/// Ouvre la caméra et la maintient ouverte pour l'aperçu de l'interface.
///
/// À appeler une fois quand l'aperçu devient visible : les vignettes
/// suivantes se contentent alors de lire la dernière image produite.
#[tauri::command]
pub async fn webcam_preview_start(
    state: State<'_, Mutex<AppState>>,
    device: String,
) -> Result<(), String> {
    // Déjà ouverte sur le bon périphérique : rien à faire
    {
        let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        if let Some((current, _)) = &state.preview_camera {
            if *current == device {
                return Ok(());
            }
        }
    }

    let opened = tauri::async_runtime::spawn_blocking({
        let device = device.clone();
        move || {
            let handle = crate::webcam::acquire(&device)?;
            handle.wait_ready(Duration::from_secs(6))?;
            Ok::<_, String>(handle)
        }
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    // L'ancienne poignée est libérée ici, après l'ouverture de la nouvelle
    state.preview_camera = Some((device, opened));
    Ok(())
}

/// Ferme la caméra d'aperçu et libère le périphérique
#[tauri::command]
pub fn webcam_preview_stop(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    state.preview_camera = None;
    Ok(())
}

/// Aperçu caméra : dernière image JPEG de la source partagée.
///
/// La caméra reste ouverte entre deux appels, au lieu d'être rouverte par un
/// nouveau processus ffmpeg à chaque vignette comme auparavant.
#[tauri::command]
pub async fn webcam_preview_frame(device: String) -> Result<String, String> {
    let jpeg =
        tauri::async_runtime::spawn_blocking(move || recorder::webcam_snapshot(&device, 480))
            .await
            .map_err(|e| e.to_string())??;

    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    ))
}

/// Rendu composite one-shot : capture une seule frame écran + une seule
/// image webcam, puis les compose via ffmpeg avec EXACTEMENT le même graphe
/// que l'enregistrement. L'aperçu est donc fidèle au rendu final.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn render_webcam_composite_preview(
    device: String,
    layout: String,
    shape: String,
    size_percent: u32,
    margin: u32,
    presentation: String,
    source: String,
    resolution: Option<String>,
    window_id: Option<u32>,
    region: Option<recorder::RegionRect>,
) -> Result<String, String> {
    let options = recorder::RecordingOptions {
        source,
        window_id,
        monitor: None,
        region,
        fps: 30,
        quality: "balanced".to_string(),
        resolution: resolution.unwrap_or_else(|| "720".to_string()),
        backdrop: None,
        audio: false,
        audio_device: None,
        system_audio_device: None,
        webcam: Some(recorder::WebcamOptions {
            device,
            layout,
            shape,
            size_percent,
            margin,
            presentation,
        }),
        hide_app: false,
        timer_in_video: false,
        capture_cursor: true,
        output_dir: None,
    };

    let jpeg =
        tauri::async_runtime::spawn_blocking(move || recorder::composite_preview(&options, 620))
            .await
            .map_err(|e| e.to_string())??;

    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    ))
}
/// Ajuste l'incrustation pendant l'enregistrement.
///
/// Les changements sont transmis à ffmpeg sous forme de commandes de filtre :
/// ni la vidéo ni le son ne sont interrompus.
#[tauri::command]
pub fn recorder_live(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    action: recorder::LiveAction,
) -> Result<recorder::LiveState, String> {
    let updated = {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        let session = state
            .recording
            .as_mut()
            .ok_or_else(|| "Aucun enregistrement en cours".to_string())?;

        session.apply_live(action.clone())?
    };

    // La fenêtre principale étant masquée pendant la capture, le retour passe
    // par l'indicateur flottant : c'est le seul endroit visible à l'écran.
    let _ = app.emit_to("rec-indicator", "live-feedback", updated.feedback(&action));

    Ok(updated)
}

/// Vignette d'un enregistrement, en data URL
#[tauri::command]
pub async fn recording_thumbnail(path: String) -> Result<String, String> {
    let jpeg = tauri::async_runtime::spawn_blocking(move || {
        recorder::recording_thumbnail(&path, 320)
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    ))
}

// --- Montage ---

/// Caractéristiques d'un enregistrement, pour alimenter l'éditeur
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSummary {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub has_audio: bool,
}

#[tauri::command]
pub async fn media_info(path: String) -> Result<MediaSummary, String> {
    let info = tauri::async_runtime::spawn_blocking(move || crate::editor::probe(&path))
        .await
        .map_err(|e| e.to_string())??;

    Ok(MediaSummary {
        width: info.width,
        height: info.height,
        fps: info.fps,
        duration: info.duration,
        has_audio: info.has_audio,
    })
}

/// Forme d'onde et silences d'un enregistrement, pour le ruban de montage.
///
/// L'analyse décode toute la piste sonore : elle part sur un fil dédié pour ne
/// pas figer l'interface, qui affiche le ruban dès qu'elle arrive.
#[tauri::command]
pub async fn analyze_audio(path: String) -> Result<crate::timeline::AudioAnalysis, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let info = crate::editor::probe(&path)?;
        crate::timeline::analyze_audio(&path, info.duration, info.has_audio)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Vignettes réparties sur la durée, pour se repérer dans l'image.
#[tauri::command]
pub async fn filmstrip(
    path: String,
    count: usize,
    height: u32,
) -> Result<Vec<crate::timeline::FilmFrame>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let info = crate::editor::probe(&path)?;
        Ok(crate::timeline::filmstrip(
            &path,
            info.duration,
            count,
            height,
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Mesure le niveau sonore d'un passage : c'est le « profil de bruit »
/// présenté à l'utilisateur avant d'appliquer la réduction.
#[tauri::command]
pub async fn measure_noise(
    path: String,
    start: f64,
    end: f64,
) -> Result<f32, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::editor::measure_noise_floor(&path, crate::editor::TimeRange { start, end })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Applique un montage. L'avancement est diffusé par l'évènement `edit-progress`.
#[tauri::command]
pub async fn apply_edit(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    plan: crate::editor::EditPlan,
) -> Result<crate::editor::EditOutcome, String> {
    let output_dir = {
        let state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        state.captures_dir.clone()
    };

    let progress: crate::editor::Progress = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Un fil dédié relaie l'avancement, faute de quoi l'utilisateur n'aurait
    // aucun retour pendant un rendu de plusieurs minutes.
    let reporter = {
        let app = app.clone();
        let progress = progress.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();

        std::thread::spawn(move || {
            let mut last = u64::MAX;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let value = progress.load(std::sync::atomic::Ordering::Relaxed);
                if value != last {
                    let _ = app.emit("edit-progress", value);
                    last = value;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        done
    };

    let worker_progress = progress.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::editor::run(&plan, &output_dir, worker_progress)
    })
    .await
    .map_err(|e| e.to_string())?;

    reporter.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = app.emit("edit-progress", 100u64);

    let outcome = outcome?;

    // Le montage rejoint la liste des enregistrements
    {
        let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
        let source = state
            .recordings
            .iter()
            .find(|record| outcome.filename.starts_with(
                Path::new(&record.filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(""),
            ))
            .cloned();

        state.add_recording(crate::state::RecordingRecord {
            id: uuid::Uuid::new_v4().to_string(),
            path: outcome.path.clone(),
            filename: outcome.filename.clone(),
            started_at: crate::capture::generate_timestamp(),
            duration_ms: outcome.duration_ms,
            width: source.as_ref().map(|s| s.width).unwrap_or(0),
            height: source.as_ref().map(|s| s.height).unwrap_or(0),
            fps: source.as_ref().map(|s| s.fps).unwrap_or(0),
            size_bytes: outcome.size_bytes,
            has_webcam: source.map(|s| s.has_webcam).unwrap_or(false),
        });
        state.save_recordings()?;
    }

    Ok(outcome)
}

// --- Dépendances externes ---

/// État des binaires externes (ffmpeg, tesseract)
#[tauri::command]
pub async fn dependency_status() -> Result<Vec<crate::dependencies::Dependency>, String> {
    tauri::async_runtime::spawn_blocking(crate::dependencies::status)
        .await
        .map_err(|e| e.to_string())
}

/// Installe une dépendance via le gestionnaire de paquets du système.
///
/// L'opération peut durer plusieurs minutes et déclenche la demande
/// d'authentification du système.
#[tauri::command]
pub async fn install_dependency(
    name: String,
) -> Result<crate::dependencies::InstallOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || crate::dependencies::install(&name))
        .await
        .map_err(|e| e.to_string())
}

/// Supprime un enregistrement (fichier inclus)
#[tauri::command]
pub fn delete_recording(state: State<'_, Mutex<AppState>>, id: String) -> Result<(), String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;

    if let Some(record) = state.recordings.iter().find(|r| r.id == id) {
        let path = PathBuf::from(&record.path);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Suppression impossible: {e}"))?;
        }
    }

    state.recordings.retain(|r| r.id != id);
    state.save_recordings()
}

// --- Presse-papiers ---

/// Copie une image (data URL) dans le presse-papiers via le plugin Tauri
#[tauri::command]
pub fn copy_image_to_clipboard(app: AppHandle, data_url: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let bytes = image_utils::decode_data_url_bytes(&data_url)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Image invalide: {e}"))?
        .to_rgba8();

    let png = image_utils::to_png_data_url(&img)?;
    let png_bytes = image_utils::decode_data_url_bytes(&png)?;

    app.clipboard()
        .write_image(&tauri::image::Image::from_bytes(&png_bytes).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Copie impossible: {e}"))
}

// --- Helpers internes ---

/// Masque la fenêtre principale, exécute la capture, puis la réaffiche.
///
/// L'attente et l'encodage se font hors du fil principal : ils totalisaient
/// plusieurs centaines de millisecondes pendant lesquelles l'interface était
/// entièrement figée.
async fn capture_with_window_hidden<F>(app: &AppHandle, capture: F) -> Result<CaptureResult, String>
where
    F: FnOnce() -> Result<CaptureResult, String> + Send + 'static,
{
    let window = app.get_webview_window("main");
    let was_visible = window
        .as_ref()
        .map(|w| w.is_visible().unwrap_or(true))
        .unwrap_or(false);

    if let Some(w) = &window {
        let _ = w.hide();
    }

    let result = tauri::async_runtime::spawn_blocking(move || {
        // Laisser le gestionnaire de fenêtres retirer l'application
        if was_visible {
            std::thread::sleep(Duration::from_millis(HIDE_DELAY_MS));
        }
        capture()
    })
    .await
    .map_err(|e| e.to_string())?;

    // La fenêtre est toujours restaurée, même si la capture a échoué
    if let Some(w) = &window {
        if was_visible {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    result
}

/// Enregistre la capture sur disque si l'auto-sauvegarde est active,
/// puis l'ajoute à l'historique.
fn persist_capture(
    state: &State<'_, Mutex<AppState>>,
    result: CaptureResult,
) -> Result<CaptureResult, String> {
    let mut state = state.lock().map_err(|_| "État verrouillé".to_string())?;
    let settings = state.settings.clone();

    let mut result = result;

    if settings.auto_save {
        let filename = format!(
            "capture_{}.png",
            chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
        );
        let dir = state.captures_dir.clone();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Création du dossier impossible: {e}"))?;

        let bytes = image_utils::decode_data_url_bytes(&result.data_url)?;
        let path = dir.join(&filename);
        std::fs::write(&path, &bytes).map_err(|e| format!("Écriture impossible: {e}"))?;

        result.path = Some(path.to_string_lossy().to_string());

        state.add_capture(CaptureRecord {
            id: result.id.clone(),
            timestamp: result.timestamp.clone(),
            filename,
            path: path.to_string_lossy().to_string(),
            width: result.width,
            height: result.height,
            format: "png".to_string(),
            size_bytes: bytes.len() as u64,
        });
        state.save_history()?;
    }

    Ok(result)
}

/// Utilitaire : encode des octets en base64 (exposé pour les tests)
#[allow(dead_code)]
fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
