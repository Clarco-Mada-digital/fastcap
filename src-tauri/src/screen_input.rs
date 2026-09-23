// FastCap - Capture d'écran native par ffmpeg
//
// La capture est confiée au grabber natif de la plateforme (`x11grab`,
// `gdigrab`, `avfoundation`) plutôt qu'à une boucle Rust qui pousserait des
// images brutes dans un tube.
//
// Mesures sur la machine de développement (2 cœurs, écran 1712x963) :
//
//   - boucle Rust + tube brut : ~3 images/s (6,6 Mo par image à recopier,
//     puis à redimensionner côté ffmpeg) ;
//   - `x11grab` : 24 images/s, durée exacte, mémoire partagée X11.
//
// Le grabber natif apporte en prime le **pointeur de souris**, que `xcap` ne
// sait pas capturer.

use serde::{Deserialize, Serialize};

/// Zone d'écran à capturer, en coordonnées globales
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Ce qui doit être filmé
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Une zone de l'écran (plein écran = zone du moniteur)
    Area(Geometry),
    /// Une fenêtre précise, suivie par son identifiant système
    Window { id: u32, geometry: Geometry },
}

impl Source {
    pub fn geometry(&self) -> Geometry {
        match self {
            Source::Area(geometry) => *geometry,
            Source::Window { geometry, .. } => *geometry,
        }
    }
}

/// Affichage X11 visé (`DISPLAY`), avec repli sur `:0.0`
#[cfg(target_os = "linux")]
fn x11_display() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".to_string());
    // x11grab veut un numéro d'écran : « :0 » devient « :0.0 »
    if display.contains('.') {
        display
    } else {
        format!("{display}.0")
    }
}

/// Arguments d'entrée ffmpeg pour filmer `source` à `fps` images par seconde.
pub fn input_args(source: &Source, fps: u32, draw_mouse: bool) -> Vec<String> {
    let mouse = if draw_mouse { "1" } else { "0" };

    #[cfg(target_os = "linux")]
    {
        let display = x11_display();
        let mut args = vec![
            "-f".to_string(),
            "x11grab".to_string(),
            "-framerate".to_string(),
            fps.to_string(),
            "-draw_mouse".to_string(),
            mouse.to_string(),
        ];
        // `xcbgrab` recourt de lui-même à la mémoire partagée X11 quand elle
        // est disponible : aucune option à passer.

        match source {
            Source::Window { id, .. } => {
                args.extend(["-window_id".to_string(), id.to_string()]);
                args.extend(["-i".to_string(), display]);
            }
            Source::Area(geometry) => {
                args.extend([
                    "-video_size".to_string(),
                    format!("{}x{}", geometry.width, geometry.height),
                ]);
                args.extend([
                    "-i".to_string(),
                    format!("{display}+{},{}", geometry.x, geometry.y),
                ]);
            }
        }

        args
    }

    #[cfg(target_os = "windows")]
    {
        let mut args = vec![
            "-f".to_string(),
            "gdigrab".to_string(),
            "-framerate".to_string(),
            fps.to_string(),
            "-draw_mouse".to_string(),
            mouse.to_string(),
        ];

        let geometry = source.geometry();
        args.extend([
            "-offset_x".to_string(),
            geometry.x.to_string(),
            "-offset_y".to_string(),
            geometry.y.to_string(),
            "-video_size".to_string(),
            format!("{}x{}", geometry.width, geometry.height),
        ]);
        args.extend(["-i".to_string(), "desktop".to_string()]);
        args
    }

    #[cfg(target_os = "macos")]
    {
        // AVFoundation ne sait filmer qu'un écran entier : la zone est
        // découpée ensuite par le graphe de filtres.
        let _ = mouse;
        vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-capture_cursor".to_string(),
            if draw_mouse { "1" } else { "0" }.to_string(),
            "-framerate".to_string(),
            fps.to_string(),
            "-i".to_string(),
            "Capture screen 0:none".to_string(),
        ]
    }
}

/// Sous macOS, l'entrée couvre tout l'écran : il faut recadrer après coup.
/// Ailleurs, le grabber sait déjà se limiter à la zone demandée.
pub fn needs_software_crop() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, width: u32, height: u32) -> Source {
        Source::Area(Geometry {
            x,
            y,
            width,
            height,
        })
    }

    #[test]
    fn an_area_capture_carries_its_size_and_offset() {
        let args = input_args(&area(100, 80, 640, 480), 24, true);
        let joined = args.join(" ");

        assert!(joined.contains("640x480"), "taille absente: {joined}");
        assert!(joined.contains("24"), "cadence absente: {joined}");

        #[cfg(target_os = "linux")]
        assert!(joined.contains("+100,80"), "décalage absent: {joined}");
    }

    #[test]
    fn the_mouse_pointer_can_be_turned_off() {
        let with = input_args(&area(0, 0, 100, 100), 15, true).join(" ");
        let without = input_args(&area(0, 0, 100, 100), 15, false).join(" ");
        assert_ne!(with, without);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_window_capture_targets_its_identifier() {
        let source = Source::Window {
            id: 0x3400003,
            geometry: Geometry {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
        };

        let args = input_args(&source, 30, true);
        let position = args.iter().position(|a| a == "-window_id").expect("window_id");
        assert_eq!(args[position + 1], "54525955");
    }

    #[test]
    fn geometry_is_reachable_for_both_kinds() {
        assert_eq!(area(1, 2, 3, 4).geometry().width, 3);

        let window = Source::Window {
            id: 7,
            geometry: Geometry {
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
            },
        };
        assert_eq!(window.geometry().height, 720);
    }
}
