// FastCap - Montage des enregistrements
//
// Le plan se déroule en quatre passes, dont seules les utiles sont exécutées :
//
//   1. découpe (conserver un ou plusieurs passages)
//   2. son : passages muets, réduction du bruit, niveau
//   3. image : cartons d'ouverture et de fin, fondus, vitesse, recadrage
//   4. conversion en image animée
//
// **Le réencodage est évité dès que possible.** Une découpe seule se fait par
// copie de flux : instantanée et sans aucune perte de qualité. Les traitements
// sonores ne réencodent que la piste audio et laissent l'image intacte. Seule
// la troisième passe touche à l'image, et seulement si on le lui demande.
//
// Une exception, explicite : `EditPlan::precise` réencode les passages
// conservés pour que la coupe tombe exactement où l'utilisateur l'a posée, au
// lieu de glisser jusqu'à l'image-clé la plus proche.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::process_util::{run_output_bounded, StderrTail};
use crate::visuals::{self, Backdrop};

const FFMPEG: &str = "ffmpeg";
const FFPROBE: &str = "ffprobe";

/// Un intervalle de la vidéo, en secondes
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    fn is_valid(&self) -> bool {
        self.start >= 0.0 && self.duration() > 0.05
    }
}

/// Réduction du bruit de fond
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenoiseOptions {
    /// Passage silencieux servant de référence, comme un « profil de bruit »
    pub sample: TimeRange,
    /// Intensité, de 1 (léger) à 97 (maximal)
    pub strength: f32,
}

/// Traitement de la piste sonore, appliqué en une seule passe
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioOptions {
    /// Passages rendus muets, exprimés dans le temps du **fichier d'origine**
    #[serde(default)]
    pub mute: Vec<TimeRange>,
    /// Gain appliqué à toute la piste, en décibels
    #[serde(default)]
    pub gain_db: f32,
    /// Normalisation EBU R128 : amène la piste au niveau attendu des
    /// plateformes de diffusion, au lieu d'un gain choisi à l'oreille.
    #[serde(default)]
    pub normalize: bool,
}

impl AudioOptions {
    fn is_empty(&self) -> bool {
        self.mute.is_empty() && self.gain_db.abs() < 0.1 && !self.normalize
    }
}

/// Recadrage, en pixels de l'image d'origine
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CropOptions {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropOptions {
    /// Ramène le cadre dans l'image et sur des dimensions paires, seules
    /// acceptées par l'encodage 4:2:0.
    fn sanitized(&self, source_width: u32, source_height: u32) -> Option<CropOptions> {
        // L'origine est arrondie d'abord : la largeur s'en déduit, et le cadre
        // peut alors aller jusqu'au bord de l'image.
        let x = (self.x & !1).min(source_width.saturating_sub(2));
        let y = (self.y & !1).min(source_height.saturating_sub(2));
        let width = self.width.min(source_width - x) & !1;
        let height = self.height.min(source_height - y) & !1;

        (width >= 16 && height >= 16).then_some(CropOptions {
            x,
            y,
            width,
            height,
        })
    }
}

/// Export en image animée plutôt qu'en vidéo
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GifOptions {
    /// Cadence du GIF : au-delà de 15 images/s le fichier explose
    pub fps: u32,
    /// Largeur de sortie ; la hauteur suit les proportions
    pub width: u32,
}

/// Carton de titre placé en ouverture ou en fermeture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleOptions {
    pub text: String,
    #[serde(default)]
    pub subtitle: String,
    /// Nom d'un arrière-plan dégradé (voir `visuals::Backdrop`)
    pub backdrop: String,
    pub duration: f64,
}

/// Ce que l'utilisateur demande sur un enregistrement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPlan {
    pub source: String,
    /// Passages à conserver ; vide signifie « tout garder »
    #[serde(default)]
    pub keep: Vec<TimeRange>,
    #[serde(default)]
    pub denoise: Option<DenoiseOptions>,
    #[serde(default)]
    pub audio: Option<AudioOptions>,
    #[serde(default)]
    pub title: Option<TitleOptions>,
    /// Carton de fermeture
    #[serde(default)]
    pub outro: Option<TitleOptions>,
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    /// Accélération ou ralenti ; 1 signifie « ne rien changer »
    #[serde(default = "normal_speed")]
    pub speed: f64,
    #[serde(default)]
    pub crop: Option<CropOptions>,
    /// Produire un GIF au lieu d'un MP4
    #[serde(default)]
    pub gif: Option<GifOptions>,
    /// Couper exactement où l'utilisateur l'a demandé, au prix d'un réencodage
    /// des passages conservés. Sans cela, la coupe glisse jusqu'à l'image-clé
    /// la plus proche.
    #[serde(default)]
    pub precise: bool,
}

fn normal_speed() -> f64 {
    1.0
}

impl EditPlan {
    /// La vitesse demandée, bornée à ce que l'oreille supporte encore
    fn speed(&self) -> f64 {
        if self.speed.is_finite() {
            self.speed.clamp(0.25, 4.0)
        } else {
            1.0
        }
    }

    fn changes_speed(&self) -> bool {
        (self.speed() - 1.0).abs() > 0.01
    }

    /// Effets qui imposent de réencoder l'image
    fn needs_reencode(&self) -> bool {
        self.title.is_some()
            || self.outro.is_some()
            || self.fade_in > 0.0
            || self.fade_out > 0.0
            || self.changes_speed()
            || self.crop.is_some()
    }

    /// La découpe demandée est-elle réencodée ?
    fn precise_cut(&self) -> bool {
        self.precise && !self.keep.is_empty()
    }

    /// Traitements ne touchant que la piste sonore
    fn audio_work(&self) -> bool {
        self.denoise.is_some() || self.audio.as_ref().is_some_and(|audio| !audio.is_empty())
    }

    /// Y a-t-il seulement quelque chose à faire ?
    pub fn is_empty(&self) -> bool {
        self.keep.is_empty()
            && !self.audio_work()
            && !self.needs_reencode()
            && self.gif.is_none()
    }
}

/// Reprojette un intervalle du fichier d'origine sur la vidéo découpée.
///
/// Les passages muets se désignent sur la ligne de temps de l'enregistrement,
/// mais s'appliquent après la découpe : sans cette conversion, ils tomberaient
/// à côté dès qu'un passage a été retiré. Un intervalle à cheval sur plusieurs
/// morceaux conservés en produit autant.
fn remap(keep: &[TimeRange], range: TimeRange) -> Vec<TimeRange> {
    if keep.is_empty() {
        return vec![range];
    }

    let mut mapped = Vec::new();
    let mut elapsed = 0.0;

    for segment in keep {
        let start = range.start.max(segment.start);
        let end = range.end.min(segment.end);
        if end > start {
            mapped.push(TimeRange {
                start: elapsed + (start - segment.start),
                end: elapsed + (end - segment.start),
            });
        }
        elapsed += segment.duration();
    }

    mapped
}

/// Caractéristiques du fichier d'origine, pour produire un montage homogène
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub has_audio: bool,
}

/// Résultat d'un montage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditOutcome {
    pub path: String,
    pub filename: String,
    pub duration_ms: u64,
    pub size_bytes: u64,
    /// `true` si la vidéo a été copiée sans réencodage
    pub lossless: bool,
}

/// Avancement, en pourcentage
pub type Progress = Arc<AtomicU64>;

// --- Analyse du fichier source ---

pub fn probe(path: &str) -> Result<MediaInfo, String> {
    let output = run_output_bounded(
        Command::new(FFPROBE)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,avg_frame_rate",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1",
                path,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        Duration::from_secs(20),
    )
    .ok_or_else(|| "Analyse du fichier expirée".to_string())?;

    let text = String::from_utf8_lossy(&output.stdout);
    let field = |name: &str| -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(|value| value.trim().to_string())
    };

    let width: u32 = field("width")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Largeur illisible".to_string())?;
    let height: u32 = field("height")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "Hauteur illisible".to_string())?;
    let duration: f64 = field("duration").and_then(|v| v.parse().ok()).unwrap_or(0.0);

    // `avg_frame_rate` arrive sous forme de fraction, p. ex. « 24000/1001 »
    let fps = field("avg_frame_rate")
        .and_then(|value| {
            let (num, den) = value.split_once('/')?;
            let num: f64 = num.parse().ok()?;
            let den: f64 = den.parse().ok()?;
            (den > 0.0).then_some(num / den)
        })
        .filter(|fps| *fps > 1.0)
        .unwrap_or(30.0);

    // Présence d'une piste sonore
    let audio = run_output_bounded(
        Command::new(FFPROBE)
            .args([
                "-v", "error", "-select_streams", "a:0", "-show_entries",
                "stream=codec_type", "-of", "csv=p=0", path,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        Duration::from_secs(20),
    );

    let has_audio = audio
        .map(|output| String::from_utf8_lossy(&output.stdout).contains("audio"))
        .unwrap_or(false);

    Ok(MediaInfo {
        width,
        height,
        fps,
        duration,
        has_audio,
    })
}

/// Mesure le niveau sonore moyen d'un passage, en décibels.
///
/// C'est l'équivalent du « profil de bruit » d'un éditeur audio : on relève le
/// plancher sur un extrait silencieux pour savoir quoi retirer ensuite.
pub fn measure_noise_floor(path: &str, sample: TimeRange) -> Result<f32, String> {
    if !sample.is_valid() {
        return Err("Le passage de référence est trop court".to_string());
    }

    let output = run_output_bounded(
        Command::new(FFMPEG)
            .args([
                "-hide_banner",
                "-ss",
                &format!("{:.3}", sample.start),
                "-i",
                path,
                "-t",
                &format!("{:.3}", sample.duration()),
                "-af",
                "volumedetect",
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
        Duration::from_secs(60),
    )
    .ok_or_else(|| "Mesure du bruit expirée".to_string())?;

    let text = String::from_utf8_lossy(&output.stderr);
    text.lines()
        .find_map(|line| {
            let position = line.find("mean_volume:")?;
            line[position + "mean_volume:".len()..]
                .trim()
                .trim_end_matches(" dB")
                .trim()
                .parse::<f32>()
                .ok()
        })
        .ok_or_else(|| "Niveau sonore illisible sur ce passage".to_string())
}

// --- Exécution du montage ---

/// Applique le plan et renvoie le fichier produit.
///
/// `progress` est mis à jour de 0 à 100 au fil du traitement.
pub fn run(plan: &EditPlan, output_dir: &Path, progress: Progress) -> Result<EditOutcome, String> {
    if plan.is_empty() {
        return Err("Aucune modification demandée".to_string());
    }

    let source = PathBuf::from(&plan.source);
    if !source.exists() {
        return Err("Fichier introuvable".to_string());
    }

    let info = probe(&plan.source)?;
    // Un identifiant unique et non le seul numéro de processus : deux montages
    // menés de front partageaient sinon le même dossier, et le nettoyage du
    // premier emportait les fichiers intermédiaires du second.
    let workspace = std::env::temp_dir().join(format!(
        "fastcap-montage-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&workspace)
        .map_err(|e| format!("Dossier de travail indisponible: {e}"))?;

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&workspace);
    };

    let result = (|| -> Result<EditOutcome, String> {
        let mut current = source.clone();

        // 1. Découpe — par copie de flux, ou réencodée si l'on exige l'exactitude
        if !plan.keep.is_empty() {
            progress.store(5, Ordering::Relaxed);
            current = cut_segments(&current, &plan.keep, &info, plan.precise, &workspace)?;
        }

        // 2. Son : muet, débruitage, niveau — en une passe, sans toucher l'image
        if plan.audio_work() && info.has_audio {
            progress.store(25, Ordering::Relaxed);

            let floor = match &plan.denoise {
                // Le plancher se mesure sur le fichier d'origine, dont les
                // repères temporels correspondent à ce que l'utilisateur a vu.
                Some(denoise) => Some(measure_noise_floor(&plan.source, denoise.sample)?),
                None => None,
            };

            let filters = audio_filters(plan, floor);
            if !filters.is_empty() {
                current = process_audio(&current, &filters, &workspace)?;
            }
        }

        // 3. Cartons, fondus, vitesse, recadrage : la seule étape qui réencode
        //    l'image
        if plan.needs_reencode() {
            progress.store(45, Ordering::Relaxed);
            current = render_effects(&current, plan, &info, &workspace, progress.clone())?;
        }

        // 4. Image animée, le cas échéant : elle se construit sur le résultat
        let mut rendered_duration = None;
        if let Some(gif) = &plan.gif {
            progress.store(88, Ordering::Relaxed);
            rendered_duration = probe(&current.to_string_lossy()).ok().map(|i| i.duration);
            current = to_gif(&current, gif, &workspace)?;
        }

        // 5. Fichier final, à côté de l'enregistrement d'origine
        progress.store(95, Ordering::Relaxed);
        let extension = if plan.gif.is_some() { "gif" } else { "mp4" };
        let destination = unique_destination(&source, output_dir, extension)?;
        std::fs::copy(&current, &destination)
            .map_err(|e| format!("Écriture du montage impossible: {e}"))?;

        // `ffprobe` ne rapporte pas toujours la durée d'un GIF : on garde alors
        // celle mesurée juste avant la conversion.
        let duration = probe(&destination.to_string_lossy())
            .map(|info| info.duration)
            .ok()
            .filter(|duration| *duration > 0.0)
            .or(rendered_duration)
            .unwrap_or(0.0);

        let size = std::fs::metadata(&destination).map(|m| m.len()).unwrap_or(0);

        progress.store(100, Ordering::Relaxed);
        Ok(EditOutcome {
            filename: destination
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("montage.mp4")
                .to_string(),
            path: destination.to_string_lossy().to_string(),
            duration_ms: (duration * 1000.0) as u64,
            size_bytes: size,
            lossless: !plan.needs_reencode() && !plan.precise_cut() && plan.gif.is_none(),
        })
    })();

    cleanup();
    result
}

/// Nom de fichier libre, dérivé de l'original
fn unique_destination(
    source: &Path,
    output_dir: &Path,
    extension: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Création du dossier impossible: {e}"))?;

    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("enregistrement");

    for attempt in 1..1000 {
        let suffix = if attempt == 1 {
            "montage".to_string()
        } else {
            format!("montage-{attempt}")
        };
        let candidate = output_dir.join(format!("{stem}_{suffix}.{extension}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("Impossible de trouver un nom de fichier libre".to_string())
}

/// Découpe : chaque passage est extrait, puis les morceaux sont recollés.
///
/// Deux régimes, selon ce que l'utilisateur privilégie :
///
/// - **copie de flux** (`precise = false`) — instantané et sans aucune perte,
///   mais les coupes tombent sur l'image-clé la plus proche. Les
///   enregistrements récents en comportent une par seconde ; les plus anciens,
///   une toutes les cinq secondes.
/// - **réencodage** (`precise = true`) — la coupe tombe exactement où elle a
///   été demandée. `-ss` reste placé avant `-i` : ffmpeg décode depuis
///   l'image-clé précédente et jette ce qui précède, donc la recherche reste
///   rapide tout en étant exacte.
fn cut_segments(
    source: &Path,
    keep: &[TimeRange],
    info: &MediaInfo,
    precise: bool,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let valid: Vec<&TimeRange> = keep.iter().filter(|range| range.is_valid()).collect();
    if valid.is_empty() {
        return Err("Aucun passage valide à conserver".to_string());
    }

    // L'encodeur n'est choisi qu'une fois : la détection interroge ffmpeg.
    let encoder = precise.then(|| {
        crate::encoder::select(info.width, info.height, info.fps.round() as u32, true)
    });

    let mut parts = Vec::new();
    for (index, range) in valid.iter().enumerate() {
        let part = workspace.join(format!("part{index}.mp4"));
        let errors = StderrTail::new();

        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
        ];

        if let Some(encoder) = &encoder {
            args.extend(encoder.init_args());
        }

        args.extend([
            "-ss".into(),
            format!("{:.3}", range.start),
            "-i".into(),
            source.to_string_lossy().to_string(),
            "-t".into(),
            format!("{:.3}", range.duration()),
        ]);

        match &encoder {
            Some(encoder) => {
                if let Some(suffix) = encoder.filter_suffix() {
                    args.extend(["-vf".into(), suffix.to_string()]);
                }
                args.extend(encoder.encode_args("high"));
                if info.has_audio {
                    args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
                }
            }
            None => args.extend(["-c".into(), "copy".into()]),
        }

        args.extend([
            // Les horodatages repartent de zéro dans chaque morceau
            "-avoid_negative_ts".into(),
            "make_zero".into(),
            "-y".into(),
            part.to_string_lossy().to_string(),
        ]);

        let mut child = Command::new(FFMPEG)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Découpe impossible: {e}"))?;

        errors.attach(&mut child);
        let status = child.wait().map_err(|e| e.to_string())?;

        if !status.success() || !part.exists() {
            return Err(errors.explain("La découpe a échoué"));
        }
        parts.push(part);
    }

    if parts.len() == 1 {
        return Ok(parts.remove(0));
    }

    // Recollage, toujours par copie de flux
    let list = workspace.join("segments.txt");
    let content: String = parts
        .iter()
        .map(|part| format!("file '{}'\n", part.to_string_lossy().replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list, content).map_err(|e| format!("Liste de montage illisible: {e}"))?;

    let joined = workspace.join("joined.mp4");
    let errors = StderrTail::new();
    let mut child = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &list.to_string_lossy(),
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-y",
            &joined.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Recollage impossible: {e}"))?;

    errors.attach(&mut child);
    let status = child.wait().map_err(|e| e.to_string())?;

    if !status.success() || !joined.exists() {
        return Err(errors.explain("Le recollage des passages a échoué"));
    }

    Ok(joined)
}

/// Chaîne de filtres sonores correspondant au plan.
///
/// Les traitements s'enchaînent dans l'ordre où ils ont un sens : on fait
/// d'abord taire ce qui doit l'être, puis on retire le souffle, et seulement
/// ensuite on règle le niveau — normaliser avant d'avoir débruité reviendrait à
/// remonter le souffle avec le reste.
fn audio_filters(plan: &EditPlan, noise_floor: Option<f32>) -> Vec<String> {
    let mut filters = Vec::new();

    if let Some(audio) = &plan.audio {
        for range in remap_all(&plan.keep, &audio.mute) {
            filters.push(format!(
                "volume=0:enable='between(t,{:.3},{:.3})'",
                range.start, range.end
            ));
        }
    }

    if let (Some(denoise), Some(floor)) = (&plan.denoise, noise_floor) {
        // `afftdn` attend un plancher entre -80 et -20 dB
        filters.push(format!(
            "afftdn=nr={:.1}:nf={:.1}",
            denoise.strength.clamp(1.0, 97.0),
            floor.clamp(-80.0, -20.0)
        ));
    }

    if let Some(audio) = &plan.audio {
        if audio.normalize {
            // Cible de diffusion courante (EBU R128). La normalisation fixe le
            // niveau : un gain manuel par-dessus n'aurait aucun effet durable,
            // c'est l'un ou l'autre.
            filters.push("loudnorm=I=-16:TP=-1.5:LRA=11".to_string());
        } else if audio.gain_db.abs() >= 0.1 {
            filters.push(format!("volume={:.1}dB", audio.gain_db.clamp(-24.0, 24.0)));
        }
    }

    filters
}

/// Applique tous les intervalles à la vidéo découpée
fn remap_all(keep: &[TimeRange], ranges: &[TimeRange]) -> Vec<TimeRange> {
    ranges
        .iter()
        .filter(|range| range.is_valid())
        .flat_map(|range| remap(keep, *range))
        .collect()
}

/// Traite la piste sonore. L'image est copiée telle quelle.
fn process_audio(source: &Path, filters: &[String], workspace: &Path) -> Result<PathBuf, String> {
    let output = workspace.join("audio.mp4");
    let errors = StderrTail::new();

    let mut child = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &source.to_string_lossy(),
            "-af",
            &filters.join(","),
            // L'image n'est pas touchée : aucune perte, et c'est immédiat
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            "-y",
            &output.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Traitement du son impossible: {e}"))?;

    errors.attach(&mut child);
    let status = child.wait().map_err(|e| e.to_string())?;

    if !status.success() || !output.exists() {
        return Err(errors.explain("Le traitement du son a échoué"));
    }

    Ok(output)
}

/// Convertit le montage en image animée.
///
/// Un GIF ne dispose que de 256 couleurs : les choisir au hasard donnerait un
/// résultat sale. On construit donc d'abord une palette à partir des images
/// réelles (`palettegen`), puis on l'applique (`paletteuse`) — le tout en une
/// passe, grâce à `split`.
fn to_gif(source: &Path, gif: &GifOptions, workspace: &Path) -> Result<PathBuf, String> {
    let fps = gif.fps.clamp(5, 30);
    let width = gif.width.clamp(120, 1920) & !1;

    let output = workspace.join("animation.gif");
    let errors = StderrTail::new();

    let mut child = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &source.to_string_lossy(),
            "-filter_complex",
            &format!(
                "fps={fps},scale={width}:-2:flags=lanczos,split[a][b];\
                 [a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3"
            ),
            // Boucle sans fin, l'attendu d'une image animée
            "-loop",
            "0",
            "-y",
            &output.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Conversion en GIF impossible: {e}"))?;

    errors.attach(&mut child);
    let status = child.wait().map_err(|e| e.to_string())?;

    if !status.success() || !output.exists() {
        return Err(errors.explain("La conversion en GIF a échoué"));
    }

    Ok(output)
}

/// Chaîne `atempo` correspondant à une vitesse.
///
/// Le filtre n'accepte qu'un facteur compris entre 0,5 et 2 : au-delà, il faut
/// l'appliquer plusieurs fois. C'est lui qui conserve la hauteur de la voix, là
/// où un simple rééchantillonnage la rendrait comique.
fn atempo_chain(speed: f64) -> Vec<String> {
    let mut factors = Vec::new();
    let mut remaining = speed;

    while remaining > 2.0 {
        factors.push(2.0);
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        factors.push(0.5);
        remaining *= 2.0;
    }
    factors.push(remaining);

    factors
        .into_iter()
        .map(|factor| format!("atempo={factor:.4}"))
        .collect()
}

/// Format commun imposé aux pistes sonores avant leur assemblage
const AUDIO_FORMAT: &str =
    "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";

/// Ajoute un carton en entrée et renvoie son indice et sa durée.
fn push_card(
    options: &Option<TitleOptions>,
    name: &str,
    width: u32,
    height: u32,
    workspace: &Path,
    args: &mut Vec<String>,
    next_input: &mut u32,
) -> Result<Option<(u32, f64)>, String> {
    let Some(options) = options else {
        return Ok(None);
    };

    let duration = options.duration.clamp(0.5, 20.0);
    let image = render_title_image(options, width, height, name, workspace)?;

    args.extend([
        "-loop".into(),
        "1".into(),
        "-t".into(),
        format!("{duration:.3}"),
        "-i".into(),
        image.to_string_lossy().to_string(),
    ]);

    let index = *next_input;
    *next_input += 1;
    Ok(Some((index, duration)))
}

/// Ajoute une source muette de la durée voulue, sous l'étiquette donnée.
fn push_silence(
    duration: f64,
    label: &str,
    args: &mut Vec<String>,
    graph: &mut Vec<String>,
    next_input: &mut u32,
) {
    args.extend([
        "-f".into(),
        "lavfi".into(),
        "-t".into(),
        format!("{duration:.3}"),
        "-i".into(),
        "anullsrc=channel_layout=stereo:sample_rate=48000".into(),
    ]);

    graph.push(format!("[{next_input}:a]{AUDIO_FORMAT}[{label}]"));
    *next_input += 1;
}

/// Cartons, fondus, vitesse et recadrage : l'unique passe qui réencode l'image.
fn render_effects(
    source: &Path,
    plan: &EditPlan,
    info: &MediaInfo,
    workspace: &Path,
    progress: Progress,
) -> Result<PathBuf, String> {
    let body = probe(&source.to_string_lossy())?;

    // Le recadrage fixe le format de tout le montage, cartons compris
    let crop = plan
        .crop
        .and_then(|crop| crop.sanitized(info.width, info.height));
    let (width, height) = match crop {
        Some(crop) => (crop.width, crop.height),
        None => (info.width, info.height),
    };

    let speed = plan.speed();
    let body_duration = body.duration / speed;

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ];

    // Les cartons sont des images fixes, bouclées le temps voulu
    let mut next_input = 0u32;
    let title_clip = push_card(
        &plan.title,
        "title",
        width,
        height,
        workspace,
        &mut args,
        &mut next_input,
    )?;

    // Entrée suivante : la vidéo
    args.extend(["-i".into(), source.to_string_lossy().to_string()]);
    let video_index = next_input;
    next_input += 1;

    let outro_clip = push_card(
        &plan.outro,
        "outro",
        width,
        height,
        workspace,
        &mut args,
        &mut next_input,
    )?;

    let mut graph: Vec<String> = Vec::new();

    // La vidéo est ramenée au format et à la cadence du montage avant tout
    // assemblage : `concat` exige des flux rigoureusement identiques.
    let mut body_filters = Vec::new();
    if let Some(crop) = crop {
        body_filters.push(format!(
            "crop={}:{}:{}:{}",
            crop.width, crop.height, crop.x, crop.y
        ));
    }
    body_filters.push(format!("scale={width}:{height}"));
    if (speed - 1.0).abs() > 0.01 {
        body_filters.push(format!("setpts=PTS/{speed:.4}"));
    }
    body_filters.push(format!("fps={:.4}", info.fps));
    body_filters.push("format=yuv420p".into());
    body_filters.push("setsar=1".into());

    graph.push(format!(
        "[{video_index}:v]{}[body]",
        body_filters.join(",")
    ));
    let mut current = "body".to_string();

    // Fondus, appliqués à la vidéo elle-même (pas aux cartons) et donc sur sa
    // durée après accélération
    if plan.fade_in > 0.0 {
        graph.push(format!(
            "[{current}]fade=t=in:st=0:d={:.3}[fadein]",
            plan.fade_in
        ));
        current = "fadein".to_string();
    }
    if plan.fade_out > 0.0 && body_duration > plan.fade_out {
        graph.push(format!(
            "[{current}]fade=t=out:st={:.3}:d={:.3}[fadeout]",
            body_duration - plan.fade_out,
            plan.fade_out
        ));
        current = "fadeout".to_string();
    }

    // Assemblage : carton d'ouverture, corps, carton de fermeture
    let mut pieces = Vec::new();

    if let Some((index, duration)) = title_clip {
        graph.push(format!(
            "[{index}:v]scale={width}:{height},fps={fps:.4},format=yuv420p,setsar=1,\
             fade=t=out:st={fade_start:.3}:d=0.4[title]",
            fps = info.fps,
            fade_start = (duration - 0.4).max(0.0)
        ));
        pieces.push("title".to_string());
    }

    pieces.push(current);

    if let Some((index, _)) = outro_clip {
        graph.push(format!(
            "[{index}:v]scale={width}:{height},fps={fps:.4},format=yuv420p,setsar=1,\
             fade=t=in:st=0:d=0.4[outro]",
            fps = info.fps
        ));
        pieces.push("outro".to_string());
    }

    let video_out = if pieces.len() > 1 {
        graph.push(format!(
            "{}concat=n={}:v=1:a=0[vout]",
            pieces
                .iter()
                .map(|label| format!("[{label}]"))
                .collect::<String>(),
            pieces.len()
        ));
        "vout".to_string()
    } else {
        pieces.remove(0)
    };

    // Piste sonore : du silence pendant les cartons, le son ajusté au milieu
    let audio_out = if info.has_audio {
        let mut body_audio = Vec::new();
        if (speed - 1.0).abs() > 0.01 {
            body_audio.extend(atempo_chain(speed));
        }
        body_audio.push(AUDIO_FORMAT.to_string());
        graph.push(format!(
            "[{video_index}:a]{}[bodya]",
            body_audio.join(",")
        ));

        let mut parts = Vec::new();

        // Un silence par carton : `concat` veut des durées exactes, une seule
        // source muette ne pourrait pas couvrir les deux.
        if let Some((_, duration)) = title_clip {
            push_silence(duration, "sil_in", &mut args, &mut graph, &mut next_input);
            parts.push("sil_in".to_string());
        }
        parts.push("bodya".to_string());
        if let Some((_, duration)) = outro_clip {
            push_silence(duration, "sil_out", &mut args, &mut graph, &mut next_input);
            parts.push("sil_out".to_string());
        }

        if parts.len() > 1 {
            graph.push(format!(
                "{}concat=n={}:v=0:a=1[aout]",
                parts
                    .iter()
                    .map(|label| format!("[{label}]"))
                    .collect::<String>(),
                parts.len()
            ));
            Some("aout".to_string())
        } else {
            Some("bodya".to_string())
        }
    } else {
        None
    };

    args.push("-filter_complex".into());
    args.push(graph.join(";"));
    args.push("-map".into());
    args.push(format!("[{video_out}]"));

    if let Some(label) = &audio_out {
        args.push("-map".into());
        args.push(format!("[{label}]"));
        args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
    }

    // Le montage réutilise l'encodeur le mieux adapté à la machine
    let encoder = crate::encoder::select(width, height, info.fps.round() as u32, true);

    // L'encodeur matériel exige un dernier filtre : on le greffe sur la sortie
    if let Some(suffix) = encoder.filter_suffix() {
        let position = args
            .iter()
            .position(|a| a == "-filter_complex")
            .expect("graphe présent");
        let graph = &mut args[position + 1];
        *graph = format!("{graph};[{video_out}]{suffix}[venc]");
        let map = args
            .iter()
            .position(|a| *a == format!("[{video_out}]"))
            .expect("sortie vidéo mappée");
        args[map] = "[venc]".to_string();
    }

    // Les options d'initialisation doivent précéder les entrées
    let init = encoder.init_args();
    for (offset, argument) in init.iter().enumerate() {
        args.insert(3 + offset, argument.clone());
    }

    args.extend(encoder.encode_args("balanced"));

    let output = workspace.join("rendered.mp4");
    args.extend([
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        output.to_string_lossy().to_string(),
    ]);

    let total = body_duration
        + title_clip.map_or(0.0, |(_, duration)| duration)
        + outro_clip.map_or(0.0, |(_, duration)| duration);
    run_with_progress(&args, total, progress, 45, 88)?;

    if !output.exists() {
        return Err("Le rendu n'a produit aucun fichier".to_string());
    }

    Ok(output)
}

/// Carton de titre : dégradé pré-rendu, texte incrusté par ffmpeg.
///
/// `name` distingue les fichiers produits : ouverture et fermeture cohabitent
/// dans le même dossier de travail.
fn render_title_image(
    title: &TitleOptions,
    width: u32,
    height: u32,
    name: &str,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let backdrop = Backdrop::parse(&title.backdrop);
    let background = if backdrop.is_enabled() {
        visuals::backdrop_image(width, height, backdrop)
    } else {
        // Fond sombre neutre quand aucun dégradé n'est choisi
        image::RgbaImage::from_pixel(
            width.min(900),
            (height * width.min(900) / width.max(1)).max(2),
            image::Rgba([14, 17, 24, 255]),
        )
    };

    let plain = workspace.join(format!("{name}-bg.png"));
    visuals::write_rgba(&plain, &background)?;

    let Some(font) = crate::recorder::timer_font_path() else {
        // Sans police, le carton reste un fond uni : mieux que rien
        return Ok(plain);
    };

    let output = workspace.join(format!("{name}.png"));
    let escape = |text: &str| {
        text.replace('\\', "\\\\")
            .replace(':', "\\:")
            .replace('\'', "\u{2019}")
            .replace('%', "\\%")
    };

    let mut drawtext = format!(
        "drawtext=fontfile='{font}':text='{}':fontcolor=white:fontsize={}:\
         x=(w-text_w)/2:y=(h-text_h)/2-{}:shadowx=2:shadowy=2:shadowcolor=black@0.35",
        escape(title.text.trim()),
        (height / 12).clamp(28, 96),
        if title.subtitle.trim().is_empty() {
            0
        } else {
            height / 18
        }
    );

    if !title.subtitle.trim().is_empty() {
        drawtext.push_str(&format!(
            ",drawtext=fontfile='{font}':text='{}':fontcolor=white@0.82:fontsize={}:\
             x=(w-text_w)/2:y=(h-text_h)/2+{}",
            escape(title.subtitle.trim()),
            (height / 24).clamp(16, 48),
            height / 16
        ));
    }

    let status = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &plain.to_string_lossy(),
            "-vf",
            &drawtext,
            "-frames:v",
            "1",
            "-y",
            &output.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Rendu du titre impossible: {e}"))?;

    Ok(if status.success() && output.exists() {
        output
    } else {
        plain
    })
}

/// Lance ffmpeg en suivant son avancement, rapporté entre `from` et `to`.
fn run_with_progress(
    args: &[String],
    total_seconds: f64,
    progress: Progress,
    from: u64,
    to: u64,
) -> Result<(), String> {
    use std::io::BufRead;

    let errors = StderrTail::new();
    let mut child = Command::new(FFMPEG)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Rendu impossible: {e}"))?;

    errors.attach(&mut child);

    if let Some(stdout) = child.stdout.take() {
        let progress = progress.clone();
        let span = to.saturating_sub(from);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(value) = line.strip_prefix("out_time_ms=") else {
                    continue;
                };
                let Ok(micros) = value.trim().parse::<u64>() else {
                    continue;
                };

                if total_seconds > 0.0 {
                    let done = (micros as f64 / 1_000_000.0 / total_seconds).clamp(0.0, 1.0);
                    progress.store(from + (done * span as f64) as u64, Ordering::Relaxed);
                }
            }
        });
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(errors.explain("Le rendu a échoué"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_plan_does_nothing() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(plan.is_empty());
        assert!(!plan.needs_reencode());
    }

    #[test]
    fn cutting_alone_stays_lossless() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: vec![TimeRange { start: 1.0, end: 5.0 }],
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(!plan.is_empty());
        // Une découpe seule se fait par copie de flux
        assert!(!plan.needs_reencode());
    }

    /// L'exactitude demandée sans découpe ne coûte rien : il n'y a rien à
    /// couper, donc rien à réencoder.
    #[test]
    fn precision_without_a_cut_costs_nothing() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: true,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(!plan.precise_cut());
        assert!(plan.is_empty());
    }

    #[test]
    fn a_precise_cut_gives_up_the_stream_copy() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: vec![TimeRange { start: 1.0, end: 5.0 }],
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: true,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(plan.precise_cut());
        // Le titre et les fondus restent seuls responsables du rendu d'effets
        assert!(!plan.needs_reencode());
    }

    #[test]
    fn a_title_forces_a_reencode() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: None,
            title: Some(TitleOptions {
                text: "Bonjour".into(),
                subtitle: String::new(),
                backdrop: "aurora".into(),
                duration: 3.0,
            }),
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(plan.needs_reencode());
    }

    #[test]
    fn fades_force_a_reencode() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: None,
            title: None,
            fade_in: 0.8,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(plan.needs_reencode());
    }

    /// La réduction de bruit ne touche pas à l'image : elle reste « sans perte »
    /// du point de vue vidéo.
    #[test]
    fn denoising_leaves_the_picture_untouched() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: Some(DenoiseOptions {
                sample: TimeRange { start: 0.0, end: 1.0 },
                strength: 25.0,
            }),
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };
        assert!(!plan.needs_reencode());
    }

    /// Sans découpe, un intervalle se retrouve tel quel dans le montage.
    #[test]
    fn remapping_without_a_cut_changes_nothing() {
        let range = TimeRange { start: 2.0, end: 4.0 };
        let mapped = remap(&[], range);

        assert_eq!(mapped.len(), 1);
        assert!((mapped[0].start - 2.0).abs() < 0.001);
        assert!((mapped[0].end - 4.0).abs() < 0.001);
    }

    /// Un passage retiré avant l'intervalle décale celui-ci d'autant.
    #[test]
    fn remapping_follows_the_cut() {
        // On garde 0–2 puis 5–10 : les deux secondes de 5 à 7 deviennent 2 à 4
        let keep = vec![
            TimeRange { start: 0.0, end: 2.0 },
            TimeRange { start: 5.0, end: 10.0 },
        ];

        let mapped = remap(&keep, TimeRange { start: 5.0, end: 7.0 });

        assert_eq!(mapped.len(), 1);
        assert!((mapped[0].start - 2.0).abs() < 0.001, "{:?}", mapped[0]);
        assert!((mapped[0].end - 4.0).abs() < 0.001, "{:?}", mapped[0]);
    }

    /// À cheval sur deux morceaux conservés, l'intervalle se scinde.
    #[test]
    fn remapping_splits_across_kept_segments() {
        let keep = vec![
            TimeRange { start: 0.0, end: 2.0 },
            TimeRange { start: 5.0, end: 10.0 },
        ];

        let mapped = remap(&keep, TimeRange { start: 1.0, end: 6.0 });

        assert_eq!(mapped.len(), 2);
        // 1–2 reste en place ; 5–6 atterrit juste après, à 2–3
        assert!((mapped[0].start - 1.0).abs() < 0.001);
        assert!((mapped[0].end - 2.0).abs() < 0.001);
        assert!((mapped[1].start - 2.0).abs() < 0.001);
        assert!((mapped[1].end - 3.0).abs() < 0.001);
    }

    /// Un intervalle entièrement dans un passage retiré disparaît.
    #[test]
    fn remapping_drops_what_was_cut_away() {
        let keep = vec![
            TimeRange { start: 0.0, end: 2.0 },
            TimeRange { start: 5.0, end: 10.0 },
        ];

        assert!(remap(&keep, TimeRange { start: 3.0, end: 4.0 }).is_empty());
    }

    /// `atempo` n'accepte qu'un facteur de 0,5 à 2 : au-delà, il se répète.
    #[test]
    fn atempo_splits_beyond_what_the_filter_accepts() {
        assert_eq!(atempo_chain(1.5), vec!["atempo=1.5000"]);
        assert_eq!(atempo_chain(4.0), vec!["atempo=2.0000", "atempo=2.0000"]);
        assert_eq!(atempo_chain(3.0), vec!["atempo=2.0000", "atempo=1.5000"]);
        assert_eq!(atempo_chain(0.25), vec!["atempo=0.5000", "atempo=0.5000"]);
    }

    /// Le produit des facteurs doit toujours redonner la vitesse demandée.
    #[test]
    fn atempo_preserves_the_requested_speed() {
        for speed in [0.25, 0.5, 0.75, 1.25, 2.0, 2.5, 4.0] {
            let product: f64 = atempo_chain(speed)
                .iter()
                .map(|filter| {
                    filter
                        .strip_prefix("atempo=")
                        .and_then(|value| value.parse::<f64>().ok())
                        .expect("facteur lisible")
                })
                .product();

            assert!((product - speed).abs() < 0.001, "{speed} donne {product}");
        }
    }

    /// Le cadre est ramené dans l'image, sur des dimensions paires.
    #[test]
    fn a_crop_is_brought_back_inside_the_picture() {
        let crop = CropOptions {
            x: 11,
            y: 7,
            width: 9999,
            height: 333,
        };

        let fixed = crop.sanitized(1920, 1080).expect("cadre exploitable");

        assert_eq!(fixed.x, 10);
        assert_eq!(fixed.y, 6);
        assert_eq!(fixed.width, 1910);
        assert_eq!(fixed.height, 332);
        assert!(fixed.x + fixed.width <= 1920);
        assert!(fixed.y + fixed.height <= 1080);
    }

    /// Un cadre minuscule est écarté plutôt que de produire un rendu illisible.
    #[test]
    fn a_tiny_crop_is_refused() {
        let crop = CropOptions {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };

        assert!(crop.sanitized(1920, 1080).is_none());
    }

    /// Le muet, le gain et la normalisation tiennent dans une seule chaîne.
    #[test]
    fn audio_filters_follow_a_sensible_order() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: Some(DenoiseOptions {
                sample: TimeRange { start: 0.0, end: 1.0 },
                strength: 20.0,
            }),
            audio: Some(AudioOptions {
                mute: vec![TimeRange { start: 1.0, end: 2.0 }],
                gain_db: 0.0,
                normalize: true,
            }),
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            crop: None,
            gif: None,
            precise: false,
        };

        let filters = audio_filters(&plan, Some(-45.0));

        assert_eq!(filters.len(), 3);
        assert!(filters[0].starts_with("volume=0:enable="));
        assert!(filters[1].starts_with("afftdn="));
        assert!(filters[2].starts_with("loudnorm="));
    }

    /// Normaliser fixe le niveau : un gain par-dessus n'aurait pas de sens.
    #[test]
    fn normalising_replaces_the_manual_gain() {
        let plan = EditPlan {
            source: "/tmp/x.mp4".into(),
            keep: Vec::new(),
            denoise: None,
            audio: Some(AudioOptions {
                mute: Vec::new(),
                gain_db: 6.0,
                normalize: true,
            }),
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            crop: None,
            gif: None,
            precise: false,
        };

        let filters = audio_filters(&plan, None);

        assert_eq!(filters.len(), 1);
        assert!(filters[0].starts_with("loudnorm="));
    }

    /// Prépare une vidéo de test : mire animée, sinus 440 Hz noyé dans du bruit
    #[cfg(test)]
    fn make_sample(directory: &Path, seconds: f64) -> PathBuf {
        std::fs::create_dir_all(directory).expect("dossier de test");
        let path = directory.join("source.mp4");

        let status = Command::new(FFMPEG)
            .args([
                "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", &format!("testsrc2=s=640x360:r=24:d={seconds}"),
                "-f", "lavfi", "-i", &format!("sine=f=440:d={seconds}"),
                "-f", "lavfi", "-i", &format!("anoisesrc=d={seconds}:c=pink:a=0.06"),
                "-filter_complex", "[1:a][2:a]amix=inputs=2:normalize=0[a]",
                "-map", "0:v", "-map", "[a]",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                // Une image-clé par seconde, comme nos enregistrements
                "-g", "24",
                "-c:a", "aac", "-shortest", "-y", &path.to_string_lossy(),
            ])
            .status()
            .expect("ffmpeg");
        assert!(status.success(), "vidéo de test non produite");
        path
    }

    fn workspace(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("fastcap-edit-test-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("dossier");
        path
    }

    /// Découpe seule : doit être sans réencodage, donc quasi instantanée, et
    /// produire exactement la durée demandée.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn cutting_is_lossless_and_precise() {
        let dir = workspace("cut");
        let source = make_sample(&dir, 8.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            // On garde 1s–3s et 5s–7s : quatre secondes au total
            keep: vec![
                TimeRange { start: 1.0, end: 3.0 },
                TimeRange { start: 5.0, end: 7.0 },
            ],
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };

        let began = std::time::Instant::now();
        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let elapsed = began.elapsed();

        eprintln!(
            "  → découpe : {:.2}s de vidéo, {} Ko, en {:?} (sans perte: {})",
            outcome.duration_ms as f64 / 1000.0,
            outcome.size_bytes / 1024,
            elapsed,
            outcome.lossless
        );

        assert!(outcome.lossless, "la découpe seule ne doit pas réencoder");
        let duration = outcome.duration_ms as f64 / 1000.0;
        assert!(
            (duration - 4.0).abs() < 0.5,
            "durée attendue ~4s, obtenue {duration:.2}s"
        );
        // Copie de flux : l'opération doit être très rapide
        assert!(elapsed.as_secs() < 10, "découpe trop lente: {elapsed:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Découpe exacte : elle réencode, et doit tomber bien plus près de la
    /// borne demandée que la copie de flux, qui glisse jusqu'à l'image-clé.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn a_precise_cut_lands_where_it_was_asked() {
        let dir = workspace("cut-precise");
        let source = make_sample(&dir, 8.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            // Des bornes volontairement décalées des images-clés
            keep: vec![TimeRange { start: 1.4, end: 3.7 }],
            denoise: None,
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: true,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let duration = outcome.duration_ms as f64 / 1000.0;

        eprintln!("  → découpe exacte : {duration:.3}s attendus 2.300s");

        assert!(!outcome.lossless, "la coupe exacte réencode");
        assert!(
            (duration - 2.3).abs() < 0.15,
            "durée attendue ~2,3s, obtenue {duration:.3}s"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Accélération : la durée doit être divisée par la vitesse demandée.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn speeding_up_shortens_the_montage() {
        let dir = workspace("speed");
        let source = make_sample(&dir, 8.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: None,
            audio: None,
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 2.0,
            crop: None,
            gif: None,
            precise: false,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let duration = outcome.duration_ms as f64 / 1000.0;

        eprintln!("  → vitesse ×2 : {duration:.2}s pour 8s d'origine");
        assert!(
            (duration - 4.0).abs() < 0.4,
            "durée attendue ~4s, obtenue {duration:.2}s"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recadrage : le fichier produit doit avoir exactement les dimensions
    /// demandées, ramenées sur des valeurs paires.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn cropping_changes_the_frame_size() {
        let dir = workspace("crop");
        let source = make_sample(&dir, 4.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: None,
            audio: None,
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            // La mire fait 640x360
            crop: Some(CropOptions {
                x: 40,
                y: 20,
                width: 320,
                height: 180,
            }),
            gif: None,
            precise: false,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let info = probe(&outcome.path).expect("analyse");

        eprintln!("  → recadrage : {}x{}", info.width, info.height);
        assert_eq!((info.width, info.height), (320, 180));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Carton de fin : la vidéo s'allonge de sa durée, comme le carton d'entrée.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn an_outro_lengthens_the_montage() {
        let dir = workspace("outro");
        let source = make_sample(&dir, 5.0);

        let card = |text: &str| TitleOptions {
            text: text.into(),
            subtitle: String::new(),
            backdrop: "slate".into(),
            duration: 2.0,
        };

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: None,
            audio: None,
            title: Some(card("Ouverture")),
            outro: Some(card("Merci")),
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            crop: None,
            gif: None,
            precise: false,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let duration = outcome.duration_ms as f64 / 1000.0;

        eprintln!("  → deux cartons de 2s : {duration:.2}s pour 5s d'origine");
        assert!(
            (duration - 9.0).abs() < 0.5,
            "durée attendue ~9s, obtenue {duration:.2}s"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Export GIF : un fichier d'image animée, à la largeur demandée.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn exporting_a_gif_produces_an_animation() {
        let dir = workspace("gif");
        let source = make_sample(&dir, 4.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: vec![TimeRange { start: 0.0, end: 2.0 }],
            denoise: None,
            audio: None,
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            crop: None,
            gif: Some(GifOptions { fps: 10, width: 240 }),
            precise: false,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");

        eprintln!(
            "  → GIF : {} ({} Ko)",
            outcome.filename,
            outcome.size_bytes / 1024
        );

        assert!(outcome.filename.ends_with(".gif"));
        assert!(!outcome.lossless, "un GIF est toujours réencodé");
        assert!(outcome.size_bytes > 0);

        let info = probe(&outcome.path).expect("analyse");
        assert_eq!(info.width, 240);
        assert!(!info.has_audio, "un GIF ne porte pas de son");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Passage muet : le son doit s'effondrer sur l'intervalle désigné et
    /// rester intact ailleurs.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn muting_silences_only_the_chosen_passage() {
        let dir = workspace("mute");
        let source = make_sample(&dir, 6.0);

        let level = |path: &str, from: f64, to: f64| -> f32 {
            measure_noise_floor(path, TimeRange { start: from, end: to }).expect("mesure")
        };

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: None,
            audio: Some(AudioOptions {
                mute: vec![TimeRange { start: 2.0, end: 4.0 }],
                gain_db: 0.0,
                normalize: false,
            }),
            title: None,
            outro: None,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            crop: None,
            gif: None,
            precise: false,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");

        let muted = level(&outcome.path, 2.5, 3.5);
        let kept = level(&outcome.path, 4.5, 5.5);

        eprintln!("  → muet : {muted:.1} dB ; hors du passage : {kept:.1} dB");
        assert!(muted < -60.0, "le passage n'est pas muet: {muted:.1} dB");
        assert!(kept > muted + 30.0, "le reste a été touché: {kept:.1} dB");
        // L'image n'est pas réencodée
        assert!(outcome.lossless);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Réduction de bruit : le souffle doit baisser nettement, sans toucher
    /// à l'image ni au signal utile.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn denoising_lowers_the_hiss_and_keeps_the_picture() {
        let dir = workspace("denoise");
        let source = make_sample(&dir, 6.0);

        let band = |path: &str| -> f32 {
            let output = Command::new(FFMPEG)
                .args([
                    "-hide_banner", "-i", path, "-af", "highpass=f=4000,volumedetect",
                    "-f", "null", "-",
                ])
                .output()
                .expect("mesure");
            let text = String::from_utf8_lossy(&output.stderr);
            text.lines()
                .find_map(|line| {
                    let p = line.find("mean_volume:")?;
                    line[p + 12..].trim().trim_end_matches(" dB").trim().parse().ok()
                })
                .unwrap_or(0.0)
        };

        let before = band(&source.to_string_lossy());

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: Some(DenoiseOptions {
                sample: TimeRange { start: 0.2, end: 1.2 },
                strength: 25.0,
            }),
            title: None,
            fade_in: 0.0,
            fade_out: 0.0,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &dir, progress).expect("montage");
        let after = band(&outcome.path);

        eprintln!("  → bruit >4 kHz : {before:.1} dB puis {after:.1} dB");
        assert!(
            after < before - 5.0,
            "réduction insuffisante : {before:.1} -> {after:.1} dB"
        );
        // L'image n'est pas réencodée
        assert!(outcome.lossless);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Titre et fondus : le fichier doit rester lisible et s'allonger de la
    /// durée du carton.
    #[test]
    #[ignore = "nécessite ffmpeg"]
    fn a_title_and_fades_produce_a_playable_file() {
        let dir = workspace("title");
        let source = make_sample(&dir, 5.0);

        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            keep: Vec::new(),
            denoise: None,
            title: Some(TitleOptions {
                text: "Démonstration FastCap".into(),
                subtitle: "Un sous-titre".into(),
                backdrop: "aurora".into(),
                duration: 2.0,
            }),
            fade_in: 0.5,
            fade_out: 0.5,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };

        let progress: Progress = Arc::new(AtomicU64::new(0));
        let began = std::time::Instant::now();
        let outcome = run(&plan, &dir, progress).expect("montage");

        eprintln!(
            "  → titre + fondus : {:.2}s de vidéo, {} Ko, rendu en {:?}",
            outcome.duration_ms as f64 / 1000.0,
            outcome.size_bytes / 1024,
            began.elapsed()
        );

        let duration = outcome.duration_ms as f64 / 1000.0;
        assert!(
            (duration - 7.0).abs() < 1.0,
            "attendu ~7s (5 + 2 de titre), obtenu {duration:.2}s"
        );
        assert!(!outcome.lossless, "un titre impose un réencodage");

        // Le fichier doit être lisible, avec ses deux pistes
        let probe = Command::new(FFPROBE)
            .args([
                "-v", "error", "-show_entries", "stream=codec_type,codec_name",
                "-of", "default=nw=1", &outcome.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe.stdout).to_string();
        assert!(report.contains("codec_name=h264"), "obtenu: {report}");
        assert!(report.contains("codec_type=audio"), "obtenu: {report}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Montage complet sur un **vrai** enregistrement FastCap : découpe,
    /// réduction de bruit, titre et fondus enchaînés.
    #[test]
    #[ignore = "nécessite un enregistrement existant"]
    fn a_full_edit_on_a_real_recording() {
        let directory = dirs::picture_dir()
            .map(|d| d.join("FastCap"))
            .unwrap_or_default();

        let Some(source) = std::fs::read_dir(&directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|e| e == "mp4").unwrap_or(false))
            .filter(|path| !path.to_string_lossy().contains("montage"))
            .max_by_key(|path| {
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
            })
        else {
            eprintln!("  (aucun enregistrement, test ignoré)");
            return;
        };

        let info = probe(&source.to_string_lossy()).expect("analyse");
        eprintln!(
            "  → source : {} ({:.1}s, {}x{}, son: {})",
            source.file_name().unwrap_or_default().to_string_lossy(),
            info.duration,
            info.width,
            info.height,
            info.has_audio
        );

        if info.duration < 6.0 {
            eprintln!("  (enregistrement trop court, test ignoré)");
            return;
        }

        let workspace = workspace("real");
        let plan = EditPlan {
            source: source.to_string_lossy().to_string(),
            // On retire un passage au milieu
            keep: vec![
                TimeRange { start: 0.0, end: 2.0 },
                TimeRange { start: 4.0, end: 6.0 },
            ],
            denoise: info.has_audio.then(|| DenoiseOptions {
                sample: TimeRange { start: 0.2, end: 1.2 },
                strength: 20.0,
            }),
            title: Some(TitleOptions {
                text: "Mon enregistrement".into(),
                subtitle: "FastCap".into(),
                backdrop: "aurora".into(),
                duration: 2.0,
            }),
            fade_in: 0.4,
            fade_out: 0.4,
            precise: false,
            audio: None,
            outro: None,
            speed: 1.0,
            crop: None,
            gif: None,
        };

        let began = std::time::Instant::now();
        let progress: Progress = Arc::new(AtomicU64::new(0));
        let outcome = run(&plan, &workspace, progress.clone()).expect("montage complet");

        eprintln!(
            "  → résultat : {:.2}s, {} Ko, en {:?}",
            outcome.duration_ms as f64 / 1000.0,
            outcome.size_bytes / 1024,
            began.elapsed()
        );

        // 4 s conservées + 2 s de titre
        let duration = outcome.duration_ms as f64 / 1000.0;
        assert!(
            (duration - 6.0).abs() < 1.2,
            "durée attendue ~6s, obtenue {duration:.2}s"
        );
        assert_eq!(progress.load(Ordering::Relaxed), 100);

        let probe_out = Command::new(FFPROBE)
            .args([
                "-v", "error", "-show_entries", "stream=codec_type,codec_name",
                "-of", "default=nw=1", &outcome.path,
            ])
            .output()
            .expect("ffprobe");
        let report = String::from_utf8_lossy(&probe_out.stdout).to_string();
        assert!(report.contains("codec_name=h264"), "obtenu: {report}");

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn ranges_must_have_a_real_duration() {
        assert!(TimeRange { start: 0.0, end: 2.0 }.is_valid());
        assert!(!TimeRange { start: 2.0, end: 2.0 }.is_valid());
        assert!(!TimeRange { start: 5.0, end: 1.0 }.is_valid());
        assert!(!TimeRange { start: -1.0, end: 3.0 }.is_valid());
    }
}
