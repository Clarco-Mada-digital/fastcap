// FastCap - Source webcam partagée
//
// Un unique processus ffmpeg par périphérique alimente en continu une
// « dernière image connue ». L'aperçu de l'interface et l'enregistrement
// consomment tous deux cette source, avec un simple compteur de références.
//
// La version précédente lançait un processus ffmpeg **par vignette d'aperçu**
// (toutes les 900 ms) : ouvrir un périphérique v4l2 coûte plusieurs centaines
// de millisecondes, la caméra était sans cesse ouverte/fermée, et
// l'enregistrement se retrouvait en concurrence avec l'aperçu pour un
// périphérique exclusif.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use image::RgbaImage;

use crate::process_util::{kill, StderrTail};

const FFMPEG: &str = "ffmpeg";

/// Résolution demandée à la caméra. 640x480 est accepté par la quasi-totalité
/// des périphériques et suffit largement pour une incrustation.
pub const CAM_WIDTH: u32 = 640;
pub const CAM_HEIGHT: u32 = 480;

/// Cadence demandée à la caméra.
///
/// Cette source ne sert **qu'aux aperçus** de l'interface : pendant
/// l'enregistrement, c'est ffmpeg qui ouvre lui-même la caméra. Dix images par
/// seconde suffisent donc largement à se cadrer, et évitent de recopier
/// 1,2 Mo trente fois par seconde sur une machine modeste.
const CAM_FPS: u32 = 10;

/// Image la plus récente produite par une caméra
#[derive(Clone)]
pub struct Frame {
    pub image: Arc<RgbaImage>,
    /// Numéro de séquence : permet aux consommateurs de savoir si l'image a changé
    pub sequence: u64,
}

struct Shared {
    latest: Mutex<Option<Frame>>,
    stop: AtomicBool,
    errors: StderrTail,
    /// Renseigné dès qu'une première image arrive (ou qu'on abandonne)
    ready: Mutex<Option<Result<(), String>>>,
    /// Processus ffmpeg de l'aperçu, pour pouvoir le terminer sans attendre
    /// que le thread de lecture remarque l'arrêt.
    child: Mutex<Option<std::process::Child>>,
    /// Passe à `true` quand le périphérique est réellement libéré
    closed: AtomicBool,
}

/// Poignée sur une source webcam. La source s'arrête quand la dernière
/// poignée est libérée.
pub struct CameraHandle {
    device: String,
    shared: Arc<Shared>,
}

impl CameraHandle {
    /// Dernière image disponible, si la caméra en a déjà produit une
    pub fn latest(&self) -> Option<Frame> {
        self.shared.latest.lock().ok().and_then(|f| f.clone())
    }

    /// Attend la première image, au plus `timeout`.
    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let start = Instant::now();
        loop {
            if let Ok(guard) = self.shared.ready.lock() {
                if let Some(result) = guard.clone() {
                    return result;
                }
            }

            if start.elapsed() >= timeout {
                let detail = self.shared.errors.tail(3);
                return Err(if detail.is_empty() {
                    format!("La caméra {} n'a produit aucune image", self.device)
                } else {
                    format!("Caméra {} indisponible — ffmpeg: {detail}", self.device)
                });
            }

            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

impl Drop for CameraHandle {
    fn drop(&mut self) {
        release(&self.device);
    }
}

// --- Registre des sources actives ---

struct Entry {
    shared: Arc<Shared>,
    references: usize,
}

#[derive(Default)]
struct Registry {
    sources: HashMap<String, Entry>,
    /// Périphériques réservés à un enregistrement en cours.
    ///
    /// Un périphérique v4l2 n'accepte qu'un seul lecteur : sans cette
    /// réservation, l'aperçu — qui interroge la caméra toutes les 300 ms —
    /// pouvait la rouvrir entre sa libération et l'ouverture par ffmpeg, qui
    /// échouait alors sur « Device or resource busy ».
    reserved: std::collections::HashSet<String>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Réserve un périphérique pour l'enregistrement : ferme l'aperçu éventuel,
/// attend que le pilote l'ait réellement relâché, et interdit toute
/// réouverture jusqu'à `unreserve`.
pub fn reserve(device: &str) -> Result<(), String> {
    let shared = {
        let mut guard = registry()
            .lock()
            .map_err(|_| "Registre caméra verrouillé".to_string())?;

        guard.reserved.insert(device.to_string());
        guard.sources.remove(device).map(|entry| entry.shared)
    };

    let Some(shared) = shared else {
        return Ok(());
    };

    // Terminer le processus tout de suite, sans attendre la prochaine image
    shared.stop.store(true, Ordering::Relaxed);
    if let Ok(mut child) = shared.child.lock() {
        if let Some(child) = child.as_mut() {
            kill(child);
        }
    }

    // Le pilote ne rend la main qu'une fois le processus effectivement mort
    let deadline = Instant::now() + Duration::from_secs(3);
    while !shared.closed.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(15));
    }

    // Petite marge : v4l2 peut mettre quelques millisecondes à se libérer
    std::thread::sleep(Duration::from_millis(80));
    Ok(())
}

/// Lève la réservation posée par [`reserve`]
pub fn unreserve(device: &str) {
    if let Ok(mut guard) = registry().lock() {
        guard.reserved.remove(device);
    }
}

/// Ouvre (ou rejoint) la source correspondant à `device`.
pub fn acquire(device: &str) -> Result<CameraHandle, String> {
    let mut guard = registry()
        .lock()
        .map_err(|_| "Registre caméra verrouillé".to_string())?;

    if guard.reserved.contains(device) {
        return Err("La caméra est utilisée par l'enregistrement en cours".to_string());
    }

    if let Some(entry) = guard.sources.get_mut(device) {
        entry.references += 1;
        return Ok(CameraHandle {
            device: device.to_string(),
            shared: entry.shared.clone(),
        });
    }

    let shared = Arc::new(Shared {
        latest: Mutex::new(None),
        stop: AtomicBool::new(false),
        errors: StderrTail::new(),
        ready: Mutex::new(None),
        child: Mutex::new(None),
        closed: AtomicBool::new(false),
    });

    spawn_reader(device.to_string(), shared.clone())?;

    guard.sources.insert(
        device.to_string(),
        Entry {
            shared: shared.clone(),
            references: 1,
        },
    );

    Ok(CameraHandle {
        device: device.to_string(),
        shared,
    })
}

fn release(device: &str) {
    let Ok(mut guard) = registry().lock() else {
        return;
    };

    let Some(entry) = guard.sources.get_mut(device) else {
        return;
    };

    entry.references = entry.references.saturating_sub(1);
    if entry.references == 0 {
        entry.shared.stop.store(true, Ordering::Relaxed);
        // Le processus est terminé sans délai : attendre la prochaine image
        // laissait le périphérique occupé une centaine de millisecondes.
        if let Ok(mut child) = entry.shared.child.lock() {
            if let Some(child) = child.as_mut() {
                kill(child);
            }
        }
        guard.sources.remove(device);
    }
}

/// Lance le processus ffmpeg et le thread de lecture des images brutes.
fn spawn_reader(device: String, shared: Arc<Shared>) -> Result<(), String> {
    let mut command = Command::new(FFMPEG);
    command
        .args(["-hide_banner", "-loglevel", "error"])
        .args(input_args(&device))
        .args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{CAM_WIDTH}x{CAM_HEIGHT}"),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| format!("Ouverture de la caméra impossible: {e}"))?;

    shared.errors.attach(&mut child);

    let Some(mut stdout) = child.stdout.take() else {
        kill(&mut child);
        return Err("Flux caméra indisponible".to_string());
    };

    if let Ok(mut slot) = shared.child.lock() {
        *slot = Some(child);
    }

    std::thread::spawn(move || {
        let frame_bytes = (CAM_WIDTH * CAM_HEIGHT * 4) as usize;
        let mut buffer = vec![0u8; frame_bytes];
        let mut sequence = 0u64;

        loop {
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }

            match stdout.read_exact(&mut buffer) {
                Ok(()) => {
                    let Some(image) =
                        RgbaImage::from_raw(CAM_WIDTH, CAM_HEIGHT, buffer.clone())
                    else {
                        break;
                    };

                    sequence += 1;
                    if let Ok(mut latest) = shared.latest.lock() {
                        *latest = Some(Frame {
                            image: Arc::new(image),
                            sequence,
                        });
                    }

                    if sequence == 1 {
                        if let Ok(mut ready) = shared.ready.lock() {
                            *ready = Some(Ok(()));
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // Le flux s'est interrompu : on le signale aux consommateurs en attente
        if let Ok(mut ready) = shared.ready.lock() {
            if ready.is_none() {
                let detail = shared.errors.tail(3);
                *ready = Some(Err(if detail.is_empty() {
                    "La caméra n'a produit aucune image".to_string()
                } else {
                    format!("Caméra indisponible — ffmpeg: {detail}")
                }));
            }
        }

        // Le périphérique n'est libre qu'une fois le processus disparu
        if let Ok(mut slot) = shared.child.lock() {
            if let Some(child) = slot.as_mut() {
                kill(child);
            }
            *slot = None;
        }
        shared.closed.store(true, Ordering::Relaxed);
    });

    Ok(())
}

/// Arguments d'entrée ffmpeg propres à la plateforme.
///
/// Partagés entre l'aperçu et l'enregistrement, pour que la caméra soit
/// ouverte exactement de la même façon dans les deux cas.
pub fn input_args(device: &str) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        vec![
            "-f".into(),
            "v4l2".into(),
            "-framerate".into(),
            CAM_FPS.to_string(),
            "-video_size".into(),
            format!("{CAM_WIDTH}x{CAM_HEIGHT}"),
            "-i".into(),
            device.to_string(),
        ]
    }

    #[cfg(target_os = "windows")]
    {
        vec![
            "-f".into(),
            "dshow".into(),
            "-i".into(),
            format!("video={device}"),
        ]
    }

    #[cfg(target_os = "macos")]
    {
        vec![
            "-f".into(),
            "avfoundation".into(),
            "-framerate".into(),
            CAM_FPS.to_string(),
            "-i".into(),
            format!("{device}:none"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_args_target_the_requested_device() {
        let args = input_args("/dev/video0");
        assert!(args.iter().any(|a| a.contains("video0")));
    }

    #[test]
    fn releasing_an_unknown_device_is_harmless() {
        release("/dev/video-inexistant");
    }
}
