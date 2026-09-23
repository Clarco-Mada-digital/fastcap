// FastCap - OCR (reconnaissance optique de caractères)
//
// L'OCR s'appuie sur le binaire `tesseract` s'il est présent sur le système.
// Aucune bibliothèque native n'est liée : le build reste identique sur les
// trois plateformes, et l'OCR devient simplement indisponible si tesseract
// n'est pas installé.

use crate::image_utils::decode_data_url;
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Un mot reconnu dans l'image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub confidence: f32,
    /// (x, y, largeur, hauteur) en pixels
    pub bbox: Option<(i32, i32, i32, i32)>,
}

/// Résultat complet d'une reconnaissance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrOutcome {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub words: Vec<OcrWord>,
}

/// Indique si le moteur OCR est utilisable sur cette machine
pub fn is_available() -> bool {
    Command::new(tesseract_binary())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tesseract_binary() -> String {
    std::env::var("FASTCAP_TESSERACT").unwrap_or_else(|_| "tesseract".to_string())
}

/// Reconnaît le texte d'une image fournie sous forme de data URL
pub fn recognize(data_url: &str, language: Option<&str>) -> Result<OcrOutcome, String> {
    if !is_available() {
        return Err(
            "OCR indisponible : installez tesseract (https://tesseract-ocr.github.io) \
             et assurez-vous que la commande `tesseract` est dans le PATH."
                .to_string(),
        );
    }

    let image = decode_data_url(data_url)?;
    let png = crate::image_utils::to_png_data_url(&image)?;
    let bytes = crate::image_utils::decode_data_url_bytes(&png)?;

    // Fichier temporaire : tesseract travaille sur des fichiers
    let tmp_dir = std::env::temp_dir();
    let stamp = uuid::Uuid::new_v4();
    let input = tmp_dir.join(format!("fastcap_ocr_{stamp}.png"));

    std::fs::write(&input, &bytes).map_err(|e| format!("Écriture temporaire impossible: {e}"))?;

    // "eng" fonctionne toujours ; "fra+eng" si l'utilisateur le demande
    let lang = language.unwrap_or("fra+eng").to_string();

    let output = Command::new(tesseract_binary())
        .arg(&input)
        .arg("stdout")
        .arg("-l")
        .arg(&lang)
        .arg("tsv")
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&input);
            return Err(format!("Exécution de tesseract impossible: {e}"));
        }
    };

    // Nettoyage systématique du fichier temporaire
    let _ = std::fs::remove_file(&input);

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "tesseract a échoué (langue « {lang} » peut-être non installée): {}",
            err.trim()
        ));
    }

    let tsv = String::from_utf8_lossy(&output.stdout);
    Ok(parse_tsv(&tsv, &lang))
}

/// Analyse la sortie TSV de tesseract
fn parse_tsv(tsv: &str, language: &str) -> OcrOutcome {
    let mut words: Vec<OcrWord> = Vec::new();

    // Colonnes : level page block par line word left top width height conf text
    for line in tsv.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        if cols[0] != "5" {
            // 5 = niveau « mot »
            continue;
        }

        let text = cols[11].trim();
        if text.is_empty() {
            continue;
        }

        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        let left: i32 = cols[6].parse().unwrap_or(0);
        let top: i32 = cols[7].parse().unwrap_or(0);
        let width: i32 = cols[8].parse().unwrap_or(0);
        let height: i32 = cols[9].parse().unwrap_or(0);

        words.push(OcrWord {
            text: text.to_string(),
            confidence: if conf < 0.0 { 0.0 } else { conf },
            bbox: Some((left, top, width, height)),
        });
    }

    // Reconstituer le texte en insérant des retours à la ligne quand l'Y change
    let mut text = String::new();
    let mut last_top: Option<i32> = None;
    for word in &words {
        let top = word.bbox.map(|b| b.1).unwrap_or(0);
        if let Some(prev) = last_top {
            // Tolérance : une même ligne peut bouger de quelques pixels
            if (top - prev).abs() > 6 {
                text.push('\n');
            } else {
                text.push(' ');
            }
        }
        text.push_str(&word.text);
        last_top = Some(top);
    }

    let confidence = if words.is_empty() {
        0.0
    } else {
        words.iter().map(|w| w.confidence).sum::<f32>() / words.len() as f32
    };

    OcrOutcome {
        text: text.trim().to_string(),
        language: language.to_string(),
        confidence,
        words,
    }
}

/// Liste les langues installées pour tesseract
pub fn available_languages() -> Vec<String> {
    let output = match Command::new(tesseract_binary())
        .arg("--list-langs")
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1) // première ligne = en-tête
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}
