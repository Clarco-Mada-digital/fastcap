// FastCap - Traitement d'image : annotations, flou, encodage
//
// Toutes les opérations travaillent sur une `RgbaImage` (crate `image` uniquement),
// ce qui évite toute dépendance à une bibliothèque de vision native.

use base64::Engine;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

/// Une annotation dessinée par l'utilisateur
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// "arrow" | "rectangle" | "ellipse" | "text" | "line" | "blur"
    pub tool: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Couleur hexadécimale, ex. "#00d4ff"
    pub color: String,
    pub thickness: u32,
    pub text: Option<String>,
}

/// Applique une liste d'annotations sur une image
pub fn apply_annotations(img: &mut RgbaImage, annotations: &[Annotation]) -> Result<(), String> {
    for ann in annotations {
        match ann.tool.as_str() {
            "rectangle" => draw_rectangle(img, ann),
            "ellipse" => draw_ellipse(img, ann),
            "line" => draw_line(img, ann),
            "arrow" => draw_arrow(img, ann),
            "blur" => blur_region(img, ann),
            // Le texte est rendu par le canvas du frontend (voir AnnotationCanvas) :
            // le backend ne peut pas rasteriser de police sans dépendance externe.
            "text" => Ok(()),
            other => Err(format!("Outil d'annotation inconnu: {other}")),
        }?;
    }
    Ok(())
}

/// Convertit une couleur hexadécimale ("#rrggbb" ou "rrggbb") en Rgba
pub fn parse_color(hex: &str) -> Rgba<u8> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() < 6 {
        return Rgba([255, 0, 0, 255]);
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(255);
    Rgba([channel(0), channel(2), channel(4), 255])
}

/// Écrit un pixel en ignorant ceux qui sortent de l'image
fn put_px(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x < 0 || y < 0 {
        return;
    }
    let (w, h) = img.dimensions();
    if (x as u32) < w && (y as u32) < h {
        img.put_pixel(x as u32, y as u32, color);
    }
}

/// Dessine un disque plein (utilisé pour l'épaisseur des traits)
fn fill_disc(img: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Rgba<u8>) {
    if radius <= 0 {
        put_px(img, cx, cy, color);
        return;
    }
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                put_px(img, cx + dx, cy + dy, color);
            }
        }
    }
}

/// Rectangle : contour si épaisseur > 0, plein si épaisseur == 0
fn draw_rectangle(img: &mut RgbaImage, ann: &Annotation) -> Result<(), String> {
    let color = parse_color(&ann.color);
    let x0 = ann.x.min(ann.x + ann.width) as i32;
    let y0 = ann.y.min(ann.y + ann.height) as i32;
    let x1 = ann.x.max(ann.x + ann.width) as i32;
    let y1 = ann.y.max(ann.y + ann.height) as i32;

    if ann.thickness == 0 {
        for y in y0..=y1 {
            for x in x0..=x1 {
                put_px(img, x, y, color);
            }
        }
        return Ok(());
    }

    let half = (ann.thickness / 2) as i32;
    for x in x0..=x1 {
        fill_disc(img, x, y0, half, color);
        fill_disc(img, x, y1, half, color);
    }
    for y in y0..=y1 {
        fill_disc(img, x0, y, half, color);
        fill_disc(img, x1, y, half, color);
    }
    Ok(())
}

/// Ligne entre deux coins du rectangle englobant
fn draw_line(img: &mut RgbaImage, ann: &Annotation) -> Result<(), String> {
    let color = parse_color(&ann.color);
    let start = (ann.x as i32, ann.y as i32);
    let end = ((ann.x + ann.width) as i32, (ann.y + ann.height) as i32);
    stroke_line(img, start, end, color, ann.thickness.max(1) as i32);
    Ok(())
}

/// Flèche : ligne + tête
fn draw_arrow(img: &mut RgbaImage, ann: &Annotation) -> Result<(), String> {
    let color = parse_color(&ann.color);
    let start = (ann.x as i32, ann.y as i32);
    let end = ((ann.x + ann.width) as i32, (ann.y + ann.height) as i32);
    let thickness = ann.thickness.max(1) as i32;

    stroke_line(img, start, end, color, thickness);

    // Tête de flèche : deux segments orientés à 30° du sens de la flèche
    let angle = ((end.1 - start.1) as f32).atan2((end.0 - start.0) as f32);
    let head = (20.0 + thickness as f32 * 3.0).max(12.0);

    for offset in [std::f32::consts::PI * 5.0 / 6.0, -std::f32::consts::PI * 5.0 / 6.0] {
        let a = angle + offset;
        let tip = (
            (end.0 as f32 + head * a.cos()) as i32,
            (end.1 as f32 + head * a.sin()) as i32,
        );
        stroke_line(img, end, tip, color, thickness);
    }
    Ok(())
}

/// Ellipse : contour si épaisseur > 0, pleine si épaisseur == 0
fn draw_ellipse(img: &mut RgbaImage, ann: &Annotation) -> Result<(), String> {
    let color = parse_color(&ann.color);
    let rx = (ann.width.abs() / 2.0).max(1.0);
    let ry = (ann.height.abs() / 2.0).max(1.0);
    let cx = ann.x + ann.width / 2.0;
    let cy = ann.y + ann.height / 2.0;
    let filled = ann.thickness == 0;
    let half = (ann.thickness / 2) as i32;

    // Pas angulaire assez fin pour ne pas laisser de trous
    let steps = ((rx.max(ry) * 4.0) as i32).clamp(64, 4096);
    for i in 0..steps {
        let t = i as f32 / steps as f32 * std::f32::consts::TAU;
        let (mut px, mut py) = (cx + rx * t.cos(), cy + ry * t.sin());

        if filled {
            // Remplissage : rejoindre le centre depuis le bord de l'ellipse
            let steps_radial = (rx.min(ry) as i32).clamp(1, 2048);
            for j in 0..=steps_radial {
                let f = j as f32 / steps_radial as f32;
                put_px(img, (cx + (px - cx) * f) as i32, (cy + (py - cy) * f) as i32, color);
            }
        } else {
            px = px.round();
            py = py.round();
            fill_disc(img, px as i32, py as i32, half, color);
        }
    }
    Ok(())
}

/// Floute une région rectangulaire (masquage de données sensibles)
fn blur_region(img: &mut RgbaImage, ann: &Annotation) -> Result<(), String> {
    let (w, h) = img.dimensions();
    let x0 = (ann.x.max(0.0) as u32).min(w.saturating_sub(1));
    let y0 = (ann.y.max(0.0) as u32).min(h.saturating_sub(1));
    let rw = (ann.width.abs() as u32).min(w - x0);
    let rh = (ann.height.abs() as u32).min(h - y0);

    if rw < 2 || rh < 2 {
        return Ok(());
    }

    // `blur` de image = flou gaussien ; un rayon plus grand masque mieux le contenu
    let sigma = (rw.min(rh) as f32 / 12.0).clamp(3.0, 24.0);
    let region = image::imageops::crop_imm(img, x0, y0, rw, rh).to_image();
    let blurred = image::imageops::blur(&region, sigma);
    image::imageops::replace(img, &blurred, x0 as i64, y0 as i64);
    Ok(())
}

/// Trace un trait épais (Bresenham + disques)
fn stroke_line(img: &mut RgbaImage, start: (i32, i32), end: (i32, i32), color: Rgba<u8>, thickness: i32) {
    let half = (thickness / 2).max(0);
    let (mut x, mut y) = start;
    let dx = (end.0 - x).abs();
    let dy = -(end.1 - y).abs();
    let sx = if x < end.0 { 1 } else { -1 };
    let sy = if y < end.1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        fill_disc(img, x, y, half, color);
        if x == end.0 && y == end.1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
        // Sécurité : éviter une boucle infinie sur des coordonnées démesurées
        if x.abs() > 100_000 || y.abs() > 100_000 {
            break;
        }
    }
}

/// Encode une image en PNG et la retourne sous forme de data URL
pub fn to_png_data_url(img: &RgbaImage) -> Result<String, String> {
    let mut buffer = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut buffer, ImageFormat::Png)
        .map_err(|e| format!("Erreur encodage PNG: {e}"))?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
    ))
}

/// Encode une image en JPEG (qualité 0-100) et retourne les octets bruts.
///
/// Utilisé pour les aperçus, où l'on veut éviter le surcoût du base64 quand
/// l'appelant n'a pas besoin d'une data URL.
pub fn to_jpeg_bytes(img: &RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buffer = Vec::new();
    let rgb = DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, quality.clamp(1, 100));
    DynamicImage::ImageRgb8(rgb)
        .write_with_encoder(encoder)
        .map_err(|e| format!("Erreur encodage JPEG: {e}"))?;
    Ok(buffer)
}

/// Encode une image en JPEG (qualité 0-100) et la retourne sous forme de data URL
pub fn to_jpeg_data_url(img: &RgbaImage, quality: u8) -> Result<String, String> {
    let buffer = to_jpeg_bytes(img, quality)?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&buffer)
    ))
}

/// Décode une data URL ("data:image/png;base64,....") en image
pub fn decode_data_url(data_url: &str) -> Result<RgbaImage, String> {
    let bytes = decode_data_url_bytes(data_url)?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Image invalide: {e}"))?;
    Ok(img.to_rgba8())
}

/// Décode uniquement la partie binaire d'une data URL
pub fn decode_data_url_bytes(data_url: &str) -> Result<Vec<u8>, String> {
    let comma = data_url
        .find(',')
        .ok_or_else(|| "Data URL invalide (séparateur manquant)".to_string())?;
    base64::engine::general_purpose::STANDARD
        .decode(data_url[comma + 1..].trim())
        .map_err(|e| format!("Décodage base64 impossible: {e}"))
}
