// FastCap - Choix de l'encodeur vidéo
//
// Le codage H.264 logiciel est de loin le poste le plus coûteux de la chaîne.
// Sur une machine modeste (2 cœurs), `libx264 -preset veryfast` en 1080p30
// plafonne à ~0,3x le temps réel : l'enregistrement ne peut structurellement
// pas suivre. On privilégie donc, dans l'ordre :
//
//   1. un encodeur matériel (VAAPI sous Linux, VideoToolbox sous macOS,
//      NVENC/QSV s'ils sont présents) — le processeur est alors libéré ;
//   2. `libx264` avec un préréglage choisi en fonction du nombre de pixels
//      à traiter par seconde.

use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use crate::process_util::run_bounded;

const FFMPEG: &str = "ffmpeg";

/// Encodeur retenu pour une session
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoder {
    /// VAAPI (Intel / AMD sous Linux)
    Vaapi { device: String },
    /// VideoToolbox (macOS) — construit uniquement sur cette plateforme
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    VideoToolbox,
    /// H.264 logiciel
    X264 { preset: &'static str },
}

impl Encoder {
    /// Nom affiché dans l'interface
    pub fn label(&self) -> String {
        match self {
            Encoder::Vaapi { .. } => "Matériel (VAAPI)".to_string(),
            Encoder::VideoToolbox => "Matériel (VideoToolbox)".to_string(),
            Encoder::X264 { preset } => format!("Logiciel (x264, {preset})"),
        }
    }

    pub fn is_hardware(&self) -> bool {
        !matches!(self, Encoder::X264 { .. })
    }

    /// Arguments à placer **avant** les entrées (initialisation du périphérique)
    pub fn init_args(&self) -> Vec<String> {
        match self {
            Encoder::Vaapi { device } => {
                vec!["-vaapi_device".to_string(), device.clone()]
            }
            _ => Vec::new(),
        }
    }

    /// Filtres à appliquer en fin de chaîne, juste avant l'encodeur
    pub fn filter_suffix(&self) -> Option<&'static str> {
        match self {
            // La conversion en NV12 puis le transfert vers le GPU doivent être
            // les toutes dernières opérations du graphe.
            Encoder::Vaapi { .. } => Some("format=nv12,hwupload"),
            _ => None,
        }
    }

    /// Arguments d'encodage (codec + qualité)
    pub fn encode_args(&self, quality: &str) -> Vec<String> {
        match self {
            Encoder::Vaapi { .. } => vec![
                "-c:v".into(),
                "h264_vaapi".into(),
                "-qp".into(),
                vaapi_qp(quality).to_string(),
            ],
            Encoder::VideoToolbox => vec![
                "-c:v".into(),
                "h264_videotoolbox".into(),
                "-q:v".into(),
                videotoolbox_quality(quality).to_string(),
                "-pix_fmt".into(),
                "yuv420p".into(),
            ],
            Encoder::X264 { preset } => vec![
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                (*preset).to_string(),
                "-crf".into(),
                x264_crf(quality).to_string(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                // Réduit la latence et la mémoire : inutile de garder de longues
                // files d'images pour de la capture d'écran.
                "-tune".into(),
                "zerolatency".into(),
            ],
        }
    }
}

fn x264_crf(quality: &str) -> u32 {
    match quality {
        "high" => 20,
        "small" => 28,
        _ => 23,
    }
}

fn vaapi_qp(quality: &str) -> u32 {
    match quality {
        "high" => 20,
        "small" => 30,
        _ => 24,
    }
}

fn videotoolbox_quality(quality: &str) -> u32 {
    match quality {
        "high" => 60,
        "small" => 35,
        _ => 50,
    }
}

/// Un encodeur est-il connu de cette installation de ffmpeg ?
fn ffmpeg_has_encoder(name: &str) -> bool {
    static ENCODERS: OnceLock<String> = OnceLock::new();

    let list = ENCODERS.get_or_init(|| {
        Command::new(FFMPEG)
            .args(["-hide_banner", "-encoders"])
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    });

    list.contains(name)
}

/// Périphérique de rendu VAAPI utilisable, s'il en existe un
#[cfg(target_os = "linux")]
fn vaapi_device() -> Option<String> {
    if !ffmpeg_has_encoder("h264_vaapi") {
        return None;
    }

    let entries = std::fs::read_dir("/dev/dri").ok()?;
    let mut candidates: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("renderD"))
                .unwrap_or(false)
        })
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    candidates.sort();

    // On ne se contente pas de la présence du fichier : on vérifie qu'un
    // encodage minimal aboutit réellement (pilote présent, droits suffisants).
    candidates.into_iter().find(|device| vaapi_works(device))
}

#[cfg(target_os = "linux")]
fn vaapi_works(device: &str) -> bool {
    run_bounded(
        Command::new(FFMPEG)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-vaapi_device",
                device,
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=320x240:r=5:d=0.4",
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        Duration::from_secs(6),
    )
}

/// Sélectionne le meilleur encodeur disponible pour la charge demandée.
///
/// `width` / `height` / `fps` décrivent le flux **de sortie** : c'est le
/// nombre de pixels par seconde qui détermine le préréglage logiciel.
pub fn select(width: u32, height: u32, fps: u32, allow_hardware: bool) -> Encoder {
    if allow_hardware {
        #[cfg(target_os = "linux")]
        if let Some(device) = vaapi_device() {
            return Encoder::Vaapi { device };
        }

        #[cfg(target_os = "macos")]
        if ffmpeg_has_encoder("h264_videotoolbox") {
            return Encoder::VideoToolbox;
        }

        #[cfg(target_os = "windows")]
        {
            // Sous Windows, QSV/NVENC exigent une configuration plus fine ;
            // on reste sur x264, dont les préréglages rapides suffisent.
        }
    }

    Encoder::X264 {
        preset: x264_preset(width, height, fps),
    }
}

/// Préréglage x264 adapté au débit de pixels demandé.
///
/// Les seuils proviennent de mesures sur une machine 2 cœurs : au-delà de
/// ~30 Mpx/s, seul `ultrafast` tient le temps réel.
fn x264_preset(width: u32, height: u32, fps: u32) -> &'static str {
    let pixels_per_second = (width as u64) * (height as u64) * (fps as u64);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(2);

    // Budget approximatif par cœur, en pixels par seconde
    let budget = pixels_per_second / cores.max(1);

    if budget > 25_000_000 {
        "ultrafast"
    } else if budget > 12_000_000 {
        "superfast"
    } else if budget > 6_000_000 {
        "veryfast"
    } else {
        "faster"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_loads_get_the_fastest_preset() {
        // 1080p60 sur peu de cœurs : seul ultrafast tient
        assert_eq!(x264_preset(1920, 1080, 60), "ultrafast");
    }

    #[test]
    fn light_loads_get_better_compression() {
        // 640x360 à 15 i/s : on peut se permettre un préréglage plus lent
        let preset = x264_preset(640, 360, 15);
        assert!(matches!(preset, "faster" | "veryfast"));
    }

    #[test]
    fn vaapi_filter_suffix_uploads_to_gpu() {
        let encoder = Encoder::Vaapi {
            device: "/dev/dri/renderD128".into(),
        };
        assert_eq!(encoder.filter_suffix(), Some("format=nv12,hwupload"));
        assert!(encoder.is_hardware());
    }

    #[test]
    fn x264_needs_no_filter_suffix() {
        let encoder = Encoder::X264 { preset: "ultrafast" };
        assert_eq!(encoder.filter_suffix(), None);
        assert!(!encoder.is_hardware());
    }
}
