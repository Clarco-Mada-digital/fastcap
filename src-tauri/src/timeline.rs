// FastCap - Analyse d'un enregistrement pour la ligne de temps du montage
//
// Le ruban de montage était une barre vide : on y posait des repères sans rien
// voir. Trois analyses lui donnent de la matière :
//
//   1. **forme d'onde** — où l'on parle, où l'on se tait
//   2. **silences** — pour aimanter les repères et proposer de les retirer
//   3. **filmstrip** — des vignettes réparties sur la durée, pour se repérer
//      dans l'image
//
// Les deux premières sortent d'une **seule** passe de décodage audio : les
// silences se déduisent de l'enveloppe, sans second appel à ffmpeg.
//
// Aucune de ces analyses n'utilise `run_output_bounded` : cette fonction attend
// la fin du processus avant de lire sa sortie, ce qui bloquerait dès que le tube
// est plein (64 Ko sous Linux). Le flux audio d'un enregistrement de dix minutes
// pèse plusieurs mégaoctets, il est donc lu au fil de l'eau.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::editor::TimeRange;
use crate::process_util::kill;

const FFMPEG: &str = "ffmpeg";

/// Fréquence d'échantillonnage de l'analyse : inutile de décoder en qualité
/// d'écoute pour tracer une enveloppe.
const ANALYSIS_RATE: u32 = 4000;

/// Durée d'un intervalle d'analyse, en secondes
const BUCKET: f64 = 0.05;

/// Nombre maximal d'intervalles, pour borner la charge sur les longs fichiers
const MAX_BUCKETS: usize = 8000;

/// En deçà de cette durée, un silence n'en est pas un : c'est une respiration
const MIN_SILENCE: f64 = 0.35;

/// Analyse sonore d'un enregistrement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioAnalysis {
    /// Enveloppe, une valeur de 0 à 1 par intervalle de `bucket` secondes
    pub peaks: Vec<f32>,
    /// Durée d'un intervalle, en secondes
    pub bucket: f64,
    /// Passages silencieux repérés
    pub silences: Vec<TimeRange>,
    /// Plancher de bruit estimé, en décibels (`None` sans piste sonore)
    pub floor_db: Option<f32>,
    /// `false` quand le fichier n'a pas de son : l'interface le dit au lieu
    /// d'afficher un ruban plat trompeur.
    pub has_audio: bool,
}

/// Une vignette de la bande d'images
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmFrame {
    /// Position dans la vidéo, en secondes
    pub time: f64,
    /// Image encodée en `data:image/jpeg;base64,…`
    pub data: String,
}

// --- Forme d'onde et silences ---

/// Décode la piste sonore et en tire l'enveloppe puis les silences.
///
/// `duration` vient de `editor::probe` : elle sert à dimensionner les
/// intervalles avant même de connaître le nombre d'échantillons.
pub fn analyze_audio(path: &str, duration: f64, has_audio: bool) -> Result<AudioAnalysis, String> {
    if !Path::new(path).exists() {
        return Err("Fichier introuvable".to_string());
    }

    if !has_audio || duration <= 0.0 {
        return Ok(AudioAnalysis {
            peaks: Vec::new(),
            bucket: BUCKET,
            silences: Vec::new(),
            floor_db: None,
            has_audio: false,
        });
    }

    // Sur un enregistrement d'une heure, 50 ms d'intervalle donneraient 72 000
    // valeurs : on élargit l'intervalle plutôt que d'en envoyer autant.
    let bucket = (duration / MAX_BUCKETS as f64).max(BUCKET);
    let per_bucket = (bucket * ANALYSIS_RATE as f64).round().max(1.0) as usize;

    let samples = decode_mono(path)?;
    if samples.is_empty() {
        return Ok(AudioAnalysis {
            peaks: Vec::new(),
            bucket,
            silences: Vec::new(),
            floor_db: None,
            has_audio: false,
        });
    }

    let count = samples.len().div_ceil(per_bucket);
    let mut peaks = Vec::with_capacity(count);
    let mut levels = Vec::with_capacity(count);

    for chunk in samples.chunks(per_bucket) {
        let mut peak = 0.0f32;
        let mut sum = 0.0f64;
        for sample in chunk {
            let value = *sample as f32 / 32768.0;
            peak = peak.max(value.abs());
            sum += (value as f64) * (value as f64);
        }
        peaks.push(peak.min(1.0));
        // La valeur efficace décrit mieux « y a-t-il de la voix » qu'une crête,
        // qui peut naître d'un simple claquement de touche.
        levels.push((sum / chunk.len() as f64).sqrt() as f32);
    }

    let floor = noise_floor(&levels);
    let silences = find_silences(&levels, floor, bucket);

    Ok(AudioAnalysis {
        peaks,
        bucket,
        silences,
        floor_db: Some(to_db(floor)),
        has_audio: true,
    })
}

/// Décode la piste sonore en PCM 16 bits mono, lu au fil de l'eau.
fn decode_mono(path: &str) -> Result<Vec<i16>, String> {
    let mut child = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path,
            "-vn",
            "-ac",
            "1",
            "-ar",
            &ANALYSIS_RATE.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Analyse sonore impossible: {e}"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Flux d'analyse indisponible".to_string())?;

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut raw: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }

        if Instant::now() > deadline {
            kill(&mut child);
            return Err("Analyse sonore expirée".to_string());
        }
    }

    let _ = child.wait();

    Ok(raw
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

/// Plancher de bruit : le 20ᵉ centile des niveaux.
///
/// La moyenne serait tirée vers le haut par la voix ; le minimum, vers le bas
/// par un éventuel blanc numérique. Un centile bas décrit ce qu'on entend
/// « quand personne ne parle ».
fn noise_floor(levels: &[f32]) -> f32 {
    if levels.is_empty() {
        return 0.0;
    }

    let mut sorted: Vec<f32> = levels.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 5]
}

/// Passages dont le niveau reste sous le seuil assez longtemps.
fn find_silences(levels: &[f32], floor: f32, bucket: f64) -> Vec<TimeRange> {
    // Deux fois et demie le plancher : au-dessus, il se passe quelque chose.
    // Le second terme couvre le cas d'un enregistrement vraiment muet, où le
    // plancher vaut zéro et où tout serait sinon déclaré silencieux.
    let threshold = (floor * 2.5).max(0.006);
    let minimum = (MIN_SILENCE / bucket).ceil() as usize;

    let mut silences = Vec::new();
    let mut run: Option<usize> = None;

    for (index, level) in levels.iter().enumerate() {
        if *level < threshold {
            run.get_or_insert(index);
        } else if let Some(start) = run.take() {
            if index - start >= minimum {
                silences.push(TimeRange {
                    start: start as f64 * bucket,
                    end: index as f64 * bucket,
                });
            }
        }
    }

    if let Some(start) = run {
        if levels.len() - start >= minimum {
            silences.push(TimeRange {
                start: start as f64 * bucket,
                end: levels.len() as f64 * bucket,
            });
        }
    }

    silences
}

fn to_db(level: f32) -> f32 {
    if level <= 0.000_01 {
        -100.0
    } else {
        20.0 * level.log10()
    }
}

// --- Bande d'images ---

/// Vignettes réparties sur la durée de la vidéo.
///
/// Chaque image est extraite par une recherche rapide plutôt qu'en décodant
/// tout le fichier : sur un enregistrement long, c'est la différence entre
/// quelques secondes et plusieurs minutes.
pub fn filmstrip(path: &str, duration: f64, count: usize, height: u32) -> Vec<FilmFrame> {
    if !Path::new(path).exists() || duration <= 0.0 {
        return Vec::new();
    }

    let count = count.clamp(4, 80);
    let height = height.clamp(24, 200);

    // Le centre de chaque tranche est plus représentatif que son bord, et la
    // dernière vignette ne tombe pas sur la toute fin, souvent noire.
    let times: Vec<f64> = (0..count)
        .map(|index| duration * (index as f64 + 0.5) / count as f64)
        .collect();

    // Quatre extractions en parallèle : ffmpeg est ici limité par la recherche
    // sur disque, pas par le calcul.
    let workers = 4.min(count);
    let mut frames: Vec<Option<FilmFrame>> = vec![None; count];

    std::thread::scope(|scope| {
        let chunk = count.div_ceil(workers);
        let mut handles = Vec::new();

        for slice in times.chunks(chunk) {
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .map(|time| {
                        extract_frame(path, *time, height).map(|data| FilmFrame { time: *time, data })
                    })
                    .collect::<Vec<_>>()
            }));
        }

        let mut cursor = 0;
        for handle in handles {
            let Ok(produced) = handle.join() else { continue };
            for frame in produced {
                if cursor < frames.len() {
                    frames[cursor] = frame;
                    cursor += 1;
                }
            }
        }
    });

    frames.into_iter().flatten().collect()
}

/// Une image de la vidéo, en JPEG encodé pour le webview.
fn extract_frame(path: &str, time: f64, height: u32) -> Option<String> {
    let mut child = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{time:.3}"),
            "-i",
            path,
            "-frames:v",
            "1",
            "-vf",
            &format!("scale=-2:{height}:flags=fast_bilinear"),
            "-f",
            "image2",
            "-c:v",
            "mjpeg",
            "-q:v",
            "6",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut jpeg = Vec::new();
    let mut buffer = [0u8; 16 * 1024];

    loop {
        match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => jpeg.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }
        if Instant::now() > deadline {
            kill(&mut child);
            return None;
        }
    }

    let _ = child.wait();

    if jpeg.is_empty() {
        return None;
    }

    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silences_need_to_last() {
        // 0,05 s d'intervalle : dix intervalles bas font 0,5 s, au-delà du
        // minimum ; deux n'en font que 0,1 s et ne comptent pas.
        let mut levels = vec![0.5f32; 40];
        levels[5..15].fill(0.0);
        levels[30..32].fill(0.0);

        let found = find_silences(&levels, 0.001, 0.05);

        assert_eq!(found.len(), 1);
        assert!((found[0].start - 0.25).abs() < 0.001);
        assert!((found[0].end - 0.75).abs() < 0.001);
    }

    #[test]
    fn a_silence_running_to_the_end_is_kept() {
        let mut levels = vec![0.5f32; 40];
        levels[20..].fill(0.0);

        let found = find_silences(&levels, 0.001, 0.05);

        assert_eq!(found.len(), 1);
        assert!((found[0].end - 2.0).abs() < 0.001);
    }

    #[test]
    fn a_fully_silent_track_is_not_all_signal() {
        // Plancher nul : sans seuil absolu, le silence deviendrait indétectable.
        let levels = vec![0.0f32; 40];

        let found = find_silences(&levels, 0.0, 0.05);

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn the_noise_floor_ignores_speech() {
        let mut levels = vec![0.01f32; 100];
        levels[50..].fill(0.8);

        assert!((noise_floor(&levels) - 0.01).abs() < 0.001);
    }

    /// Vidéo de test : mire animée et un son qui s'interrompt de 3 s à 6 s.
    fn make_sample(directory: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(directory).expect("dossier de test");
        let path = directory.join("source.mp4");

        let status = Command::new(FFMPEG)
            .args([
                "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc2=s=320x180:r=24:d=10",
                // Le sinus est coupé entre la 3ᵉ et la 6ᵉ seconde
                "-f", "lavfi", "-i",
                "sine=f=440:d=10,volume=enable='between(t,3,6)':volume=0",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-shortest", "-y",
                &path.to_string_lossy(),
            ])
            .status()
            .expect("ffmpeg");

        assert!(status.success(), "préparation de la vidéo de test");
        path
    }

    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn a_real_file_yields_a_waveform_and_its_silence() {
        let dir = std::env::temp_dir().join("fastcap-timeline-test");
        let source = make_sample(&dir);

        let analysis = analyze_audio(&source.to_string_lossy(), 10.0, true).expect("analyse");

        assert!(analysis.has_audio);
        // Dix secondes par intervalles de 50 ms
        assert!(analysis.peaks.len() > 150, "{} valeurs", analysis.peaks.len());
        // Le sinus de `lavfi` sort à environ 0,13 : ce qui compte n'est pas son
        // niveau absolu mais qu'il se détache nettement du silence.
        let loudest = analysis.peaks.iter().copied().fold(0.0f32, f32::max);
        assert!(loudest > 0.05, "signal trop faible: {loudest:.3}");

        let silence = analysis
            .silences
            .iter()
            .max_by(|a, b| {
                (a.end - a.start)
                    .partial_cmp(&(b.end - b.start))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("un silence attendu");

        eprintln!(
            "  → silence repéré : {:.2}s → {:.2}s (attendu 3,00 → 6,00)",
            silence.start, silence.end
        );
        assert!((silence.start - 3.0).abs() < 0.3);
        assert!((silence.end - 6.0).abs() < 0.3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn the_filmstrip_covers_the_whole_duration() {
        let dir = std::env::temp_dir().join("fastcap-filmstrip-test");
        let source = make_sample(&dir);

        let frames = filmstrip(&source.to_string_lossy(), 10.0, 12, 52);

        assert_eq!(frames.len(), 12, "une vignette par tranche");
        assert!(frames[0].time < 1.0 && frames[11].time > 9.0);
        assert!(frames.iter().all(|frame| frame.data.starts_with("data:image/jpeg;base64,")));
        // Les vignettes restent légères : elles transitent vers le webview
        assert!(frames.iter().all(|frame| frame.data.len() < 40_000));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
