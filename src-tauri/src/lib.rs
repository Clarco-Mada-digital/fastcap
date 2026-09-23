// FastCap - Application de capture d'écran
// Point d'entrée Tauri : plugins, état global, menu de la zone de notification

mod capture;
mod commands;
mod compositor;
mod dependencies;
mod editor;
mod encoder;
mod image_utils;
mod ocr;
mod process_util;
mod recorder;
mod screen_input;
mod state;
mod timeline;
mod visuals;
mod webcam;

use commands::*;
use state::AppState;
use std::sync::Mutex;

use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

/// Raccourcis globaux : capture plein écran / région / fenêtre + affichage
fn shortcut_bindings() -> Vec<(&'static str, Shortcut)> {
    vec![
        ("fullscreen", Shortcut::new(None, Code::PrintScreen)),
        (
            "region",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR),
        ),
        (
            "window",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyW),
        ),
        (
            "focus",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyS),
        ),
        (
            "stop-recording",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyE),
        ),
        // Pilotage de l'incrustation pendant l'enregistrement
        (
            "cam-corner",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyL),
        ),
        (
            "cam-toggle",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyH),
        ),
        (
            "cam-shape",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyF),
        ),
        (
            "cam-swap",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyX),
        ),
        (
            "cam-bigger",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Equal),
        ),
        (
            "cam-smaller",
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Minus),
        ),
    ]
}

/// Configure le logging vers fichier + sortie standard
fn setup_logging() {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("FastCap")
        .join("logs");

    if std::fs::create_dir_all(&log_dir).is_err() {
        // Sans dossier de logs, on se contente de la sortie standard
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .init();
        return;
    }

    let file_appender = rolling_file(log_dir);

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    // Le guard doit rester vivant pendant toute la durée du programme
    std::mem::forget(guard);
}

fn rolling_file(log_dir: std::path::PathBuf) -> tracing_appender::rolling::RollingFileAppender {
    tracing_appender::rolling::RollingFileAppender::new(
        tracing_appender::rolling::Rotation::DAILY,
        log_dir,
        "fastcap.log",
    )
}

/// Zone de notification : afficher, capturer, quitter
fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show_item = MenuItem::with_id(app, "show", "Afficher fastcap", true, None::<&str>)?;
    let capture_item = MenuItem::with_id(app, "capture", "Capture d'écran", true, None::<&str>)?;
    let record_item = MenuItem::with_id(app, "record", "Enregistrer l'écran", true, None::<&str>)?;
    let stop_item = MenuItem::with_id(app, "stop-record", "Arrêter l'enregistrement", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[&show_item, &capture_item, &record_item, &stop_item, &quit_item],
    )?;

    // L'identifiant est indispensable : le minuteur d'enregistrement met à
    // jour l'infobulle via `tray_by_id("main")`, qui restait introuvable.
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("FastCap - capture d'écran et enregistrement")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "capture" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.emit("trigger-capture", ());
                }
            }
            "record" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.emit("trigger-recording", ());
                }
            }
            "stop-record" => {
                commands::stop_recording_in_background(app);
            }
            "quit" => {
                // Ne jamais laisser un enregistrement en cours derrière soi
                commands::finalize_recording_on_exit(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// Plugin des raccourcis globaux.
///
/// Concret plutôt que générique : l'arrêt d'enregistrement est traité ici même,
/// ce qui suppose le `AppHandle` de l'exécution réellement utilisée.
fn global_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let bindings = shortcut_bindings();
    let shortcuts: Vec<Shortcut> = bindings.iter().map(|(_, s)| *s).collect();

    let registered = tauri_plugin_global_shortcut::Builder::new()
        .with_shortcuts(shortcuts)
        .map(move |builder| {
            builder.with_handler(move |app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }

                if let Some((action, _)) = bindings.iter().find(|(_, s)| *s == *shortcut) {
                    // L'arrêt est traité directement : il doit aboutir même
                    // si la fenêtre est masquée ou fermée.
                    if *action == "stop-recording" {
                        commands::stop_recording_in_background(app);
                        return;
                    }
                    let _ = app.emit("global-shortcut", *action);
                }
            })
        });

    match registered {
        Ok(builder) => builder.build(),
        Err(e) => {
            error!("Raccourcis globaux indisponibles: {e}");
            tauri_plugin_global_shortcut::Builder::new().build()
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    setup_logging();
    info!("FastCap démarré - version {}", env!("CARGO_PKG_VERSION"));

    let app_state = AppState::new();
    info!("Dossier de captures: {:?}", app_state.captures_dir);
    info!(
        "OCR {}",
        if ocr::is_available() {
            "disponible (tesseract détecté)"
        } else {
            "indisponible (tesseract introuvable)"
        }
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(global_shortcut_plugin())
        .manage(Mutex::new(app_state))
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_settings,
            save_settings,
            get_capture_history,
            clear_capture_history,
            delete_capture,
            save_capture_edit,
            read_capture_image,
            read_capture_thumbnail,
            capture_fullscreen,
            capture_region,
            capture_window,
            list_windows,
            list_monitors,
            preview_recording_source,
            start_region_selection,
            get_region_selection_image,
            cancel_region_selection,
            finish_region_selection,
            annotate_image,
            export_image,
            copy_image_to_clipboard,
            ocr_image,
            ocr_is_available,
            ocr_available_languages,
            recorder_capabilities,
            webcam_preview_start,
            webcam_preview_stop,
            webcam_preview_frame,
            render_webcam_composite_preview,
            start_recording,
            stop_recording,
            recording_status,
            get_recordings,
            delete_recording,
            recorder_live,
            recording_thumbnail,
            media_info,
            analyze_audio,
            filmstrip,
            measure_noise,
            apply_edit,
            dependency_status,
            install_dependency,
        ])
.setup(|app| {
            if let Err(e) = setup_tray(app) {
                error!("Erreur configuration de la zone de notification: {}", e);
            }
            // Minuteur d'enregistrement dans l'infobulle de la zone de notification :
            // donne un retour visuel même quand la fenêtre principale est masquée.
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut last_tooltip = String::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    let state = app_handle.state::<Mutex<AppState>>();
                    let rec = {
                        let guard = state.lock().ok();
                        guard
                            .as_ref()
                            .and_then(|s| s.recording.as_ref())
                            .map(|session| session.started.elapsed().as_millis() as u64)
                    };
                    match rec {
                        Some(elapsed_ms) => {
                            let secs = elapsed_ms / 1000;
                            let h = secs / 3600;
                            let m = (secs % 3600) / 60;
                            let s = secs % 60;
                            let tip = if h > 0 {
                                format!("FastCap - enregistrement en cours {h:02}:{m:02}:{s:02}")
                            } else {
                                format!("FastCap - enregistrement en cours {m:02}:{s:02}")
                            };
                            if tip != last_tooltip {
                                if let Some(tray) = app_handle.tray_by_id("main") {
                                    let _ = tray.set_tooltip(Some(tip.clone()));
                                }
                                last_tooltip = tip;
                            }
                            // Temps affiché dans la petite fenêtre "REC" à l'écran
                            let _ = app_handle.emit_to("rec-indicator", "rec-tick", elapsed_ms);
                        }
                        None => {
                            // Uniquement à la transition : inutile de diffuser
                            // un évènement par seconde quand rien n'enregistre.
                            if !last_tooltip.is_empty() {
                                if let Some(tray) = app_handle.tray_by_id("main") {
                                    let _ = tray.set_tooltip(Some(
                                        "FastCap - capture d'écran et enregistrement".to_string(),
                                    ));
                                }
                                last_tooltip.clear();
                                let _ = app_handle.emit_to("rec-indicator", "rec-stop", ());
                            }
                        }
                    }
                }
            });
            info!("Application prête");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    // La croix **referme** l'application dans la zone de
                    // notification au lieu de la détruire.
                    //
                    // Sans cela, la fenêtre disparaissait pour de bon : l'icône
                    // de la barre cherchait ensuite une fenêtre inexistante et
                    // ne pouvait plus rien réafficher. On quitte par « Quitter »
                    // dans le menu de la zone de notification.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Erreur lors du démarrage de l'application");
}
