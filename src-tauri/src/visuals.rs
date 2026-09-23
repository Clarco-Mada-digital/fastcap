// FastCap - Visuels pré-calculés (masques, ombres, halos, arrière-plans)
//
// Tout ce qui est décoratif est rendu **une seule fois** au démarrage de
// l'enregistrement, puis fourni à ffmpeg sous forme de PNG. Le graphe de
// filtres se limite alors à un `alphamerge` et quelques `overlay`, dont le
// coût est négligeable.
//
// La version précédente calculait ces effets par image avec `geq` (évaluateur
// d'expression par pixel) et `boxblur` : mesuré à ~10x le temps réel pour de
// la 1080p30, ce qui rendait tout enregistrement fluide impossible.

use image::{GrayImage, Luma, Rgba, RgbaImage};
use std::path::{Path, PathBuf};

/// Forme de l'incrustation webcam
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Square,
    Rounded,
    Circle,
}

impl Shape {
    pub fn parse(value: &str) -> Self {
        match value {
            "circle" => Shape::Circle,
            "rounded" => Shape::Rounded,
            _ => Shape::Square,
        }
    }
}

/// Style de présentation de l'incrustation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    /// Aucune décoration
    Minimal,
    /// Cadre clair + ombre portée nette
    Classic,
    /// Ombre douce et large
    Studio,
    /// Cercle + halo coloré
    Bubble,
}

impl Presentation {
    pub fn parse(value: &str) -> Self {
        match value {
            "classic" => Presentation::Classic,
            "studio" => Presentation::Studio,
            "bubble" => Presentation::Bubble,
            _ => Presentation::Minimal,
        }
    }

    pub fn is_decorated(self) -> bool {
        !matches!(self, Presentation::Minimal)
    }
}

/// Arrière-plan « façon Tella » : la capture est réduite, arrondie et posée
/// sur un fond dégradé avec une ombre portée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    None,
    Aurora,
    Sunset,
    Mint,
    Slate,
    Cream,
}

impl Backdrop {
    pub fn parse(value: &str) -> Self {
        match value {
            "aurora" => Backdrop::Aurora,
            "sunset" => Backdrop::Sunset,
            "mint" => Backdrop::Mint,
            "slate" => Backdrop::Slate,
            "cream" => Backdrop::Cream,
            _ => Backdrop::None,
        }
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, Backdrop::None)
    }

    /// Couleurs du dégradé (haut-gauche, bas-droite)
    fn colors(self) -> ([f32; 3], [f32; 3]) {
        match self {
            Backdrop::Aurora => ([99.0, 102.0, 241.0], [168.0, 85.0, 247.0]),
            Backdrop::Sunset => ([251.0, 146.0, 60.0], [244.0, 63.0, 94.0]),
            Backdrop::Mint => ([16.0, 185.0, 129.0], [56.0, 189.0, 248.0]),
            Backdrop::Slate => ([30.0, 41.0, 59.0], [15.0, 23.0, 42.0]),
            Backdrop::Cream => ([254.0, 243.0, 199.0], [253.0, 186.0, 116.0]),
            Backdrop::None => ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
        }
    }
}

// --- Masques de forme ---

/// Masque en niveaux de gris : blanc à l'intérieur de la forme, noir dehors.
/// Les bords sont anti-aliasés, ce que `geq` ne faisait pas.
pub fn shape_mask(width: u32, height: u32, shape: Shape) -> GrayImage {
    let width = width.max(1);
    let height = height.max(1);

    // Un carré est uniformément opaque : inutile de calculer quoi que ce soit
    if shape == Shape::Square {
        return GrayImage::from_pixel(width, height, Luma([255]));
    }

    let w = width as f32;
    let h = height as f32;
    let (cx, cy) = (w / 2.0, h / 2.0);

    // Rayon des coins : proportionnel au petit côté, plafonné pour rester élégant
    let radius = match shape {
        Shape::Circle => w.min(h) / 2.0,
        _ => (w.min(h) * 0.12).clamp(8.0, 48.0),
    };

    // Accès direct au tampon : `put_pixel` recalcule un indice et vérifie les
    // bornes à chaque pixel, ce qui domine le coût sur des images de cette
    // taille — surtout en build debug, où ces vérifications ne sont pas élidées.
    let mut buffer = vec![0u8; (width as usize) * (height as usize)];
    let inner_w = w / 2.0 - radius;
    let inner_h = h / 2.0 - radius;

    for y in 0..height {
        let py = y as f32 + 0.5;
        let dy_base = (py - cy).abs();
        let row = (y as usize) * (width as usize);

        for x in 0..width {
            let px = x as f32 + 0.5;

            let distance = match shape {
                Shape::Circle => ((px - cx).powi(2) + (py - cy).powi(2)).sqrt(),
                _ => {
                    // Distance signée à un rectangle aux coins arrondis
                    let dx = ((px - cx).abs() - inner_w).max(0.0);
                    let dy = (dy_base - inner_h).max(0.0);
                    (dx * dx + dy * dy).sqrt()
                }
            };

            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
            buffer[row + x as usize] = (coverage * 255.0) as u8;
        }
    }

    GrayImage::from_raw(width, height, buffer).expect("masque de taille cohérente")
}

/// Côté d'une tuile de l'atlas des formes
pub const ATLAS_TILE: u32 = 512;

/// Ordre des formes dans l'atlas, utilisé comme index de découpe
pub const ATLAS_ORDER: [Shape; 3] = [Shape::Square, Shape::Rounded, Shape::Circle];

/// Position d'une forme dans l'atlas
pub fn atlas_offset(shape: Shape) -> u32 {
    ATLAS_ORDER
        .iter()
        .position(|candidate| *candidate == shape)
        .unwrap_or(0) as u32
        * ATLAS_TILE
}

/// Atlas des trois masques, empilés verticalement.
///
/// Changer la forme de l'incrustation **pendant** l'enregistrement revient
/// alors à déplacer une découpe (`crop`), opération que ffmpeg accepte en
/// direct — là où regénérer un masque imposerait de relancer l'encodage.
pub fn shape_atlas() -> GrayImage {
    let mut atlas = GrayImage::new(ATLAS_TILE, ATLAS_TILE * ATLAS_ORDER.len() as u32);
    let atlas_width = ATLAS_TILE as usize;

    for (index, shape) in ATLAS_ORDER.iter().enumerate() {
        let tile = shape_mask(ATLAS_TILE, ATLAS_TILE, *shape);
        let tile_raw = tile.as_raw();
        let base = index * (ATLAS_TILE as usize) * atlas_width;

        let target = atlas.as_mut();
        target[base..base + tile_raw.len()].copy_from_slice(tile_raw);
    }

    atlas
}

// --- Flou séparable (approximation gaussienne par 3 passes de moyenne) ---

fn box_blur_gray(src: &GrayImage, radius: u32) -> GrayImage {
    if radius == 0 {
        return src.clone();
    }

    let mut buffer = src.clone();
    for _ in 0..3 {
        buffer = blur_pass_horizontal(&buffer, radius);
        buffer = blur_pass_vertical(&buffer, radius);
    }
    buffer
}

/// Moyenne glissante horizontale (coût linéaire, indépendant du rayon).
///
/// Comme pour les masques, tout passe par les tampons bruts : les accesseurs
/// pixel par pixel coûtaient ici l'essentiel du temps de calcul.
fn blur_pass_horizontal(src: &GrayImage, radius: u32) -> GrayImage {
    let (w, h) = (src.width(), src.height());
    let (wu, hu) = (w as usize, h as usize);
    let span = radius as usize;
    let window = (radius * 2 + 1) as u32;

    let source = src.as_raw();
    let mut out = vec![0u8; wu * hu];

    for y in 0..hu {
        let row = y * wu;
        let line = &source[row..row + wu];

        // Amorçage de la fenêtre, bords répliqués
        let mut sum: u32 = line[0] as u32 * radius;
        for i in 0..=span {
            sum += line[i.min(wu - 1)] as u32;
        }

        for x in 0..wu {
            out[row + x] = (sum / window) as u8;

            let leaving = line[x.saturating_sub(span)] as u32;
            let entering = line[(x + span + 1).min(wu - 1)] as u32;
            sum = sum + entering - leaving;
        }
    }

    GrayImage::from_raw(w, h, out).expect("flou horizontal cohérent")
}

fn blur_pass_vertical(src: &GrayImage, radius: u32) -> GrayImage {
    let (w, h) = (src.width(), src.height());
    let (wu, hu) = (w as usize, h as usize);
    let span = radius as usize;
    let window = (radius * 2 + 1) as u32;

    let source = src.as_raw();
    let mut out = vec![0u8; wu * hu];

    for x in 0..wu {
        let mut sum: u32 = source[x] as u32 * radius;
        for i in 0..=span {
            sum += source[i.min(hu - 1) * wu + x] as u32;
        }

        for y in 0..hu {
            out[y * wu + x] = (sum / window) as u8;

            let leaving = source[y.saturating_sub(span) * wu + x] as u32;
            let entering = source[(y + span + 1).min(hu - 1) * wu + x] as u32;
            sum = sum + entering - leaving;
        }
    }

    GrayImage::from_raw(w, h, out).expect("flou vertical cohérent")
}

// --- Ombres et halos ---

/// Marge ajoutée autour de l'incrustation pour loger l'ombre / le halo.
pub const SPRITE_PAD: u32 = 56;

/// Sprite décoratif placé **derrière** l'incrustation : ombre portée pour
/// « classique » / « studio », halo coloré pour « bulle ».
///
/// Le canevas fait `width + 2*PAD` par `height + 2*PAD` ; l'incrustation
/// vient donc se poser à l'offset (PAD, PAD) du sprite.
pub fn decoration_sprite(
    width: u32,
    height: u32,
    shape: Shape,
    presentation: Presentation,
) -> RgbaImage {
    let pad = SPRITE_PAD;
    let canvas_w = width + pad * 2;
    let canvas_h = height + pad * 2;

    // Silhouette de l'incrustation, décalée dans le canevas
    let silhouette = shape_mask(width, height, shape);

    let (offset_y, blur_radius, opacity, tint) = match presentation {
        Presentation::Classic => (10i32, 10u32, 0.55f32, [0u8, 0, 0]),
        Presentation::Studio => (16, 20, 0.45, [0, 0, 0]),
        Presentation::Bubble => (0, 22, 0.75, [129, 140, 248]),
        Presentation::Minimal => (0, 0, 0.0, [0, 0, 0]),
    };

    let mut field = GrayImage::new(canvas_w, canvas_h);
    for y in 0..height {
        for x in 0..width {
            let ty = y as i32 + pad as i32 + offset_y;
            let tx = x as i32 + pad as i32;
            if ty >= 0 && ty < canvas_h as i32 && tx >= 0 && tx < canvas_w as i32 {
                field.put_pixel(tx as u32, ty as u32, *silhouette.get_pixel(x, y));
            }
        }
    }

    // Le halo de la « bulle » déborde davantage : on dilate avant de flouter
    let field = box_blur_gray(&field, blur_radius);

    let mut sprite = RgbaImage::new(canvas_w, canvas_h);
    for y in 0..canvas_h {
        for x in 0..canvas_w {
            let a = field.get_pixel(x, y).0[0] as f32 / 255.0;
            let alpha = (a * opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            sprite.put_pixel(x, y, Rgba([tint[0], tint[1], tint[2], alpha]));
        }
    }

    sprite
}

/// Fin liseré clair posé **par-dessus** l'incrustation (style « classique »).
/// Renvoie `None` si la présentation n'en demande pas.
pub fn border_sprite(
    width: u32,
    height: u32,
    shape: Shape,
    presentation: Presentation,
) -> Option<RgbaImage> {
    if presentation != Presentation::Classic {
        return None;
    }

    let thickness = ((width.min(height) as f32) * 0.012).clamp(2.0, 6.0);
    let outer = shape_mask(width, height, shape);

    // Le liseré est la différence entre la forme et une version rétrécie
    let inner_w = (width as f32 - thickness * 2.0).max(1.0) as u32;
    let inner_h = (height as f32 - thickness * 2.0).max(1.0) as u32;
    let inner_small = shape_mask(inner_w, inner_h, shape);
    let inner = image::imageops::resize(
        &inner_small,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );

    let mut sprite = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let o = outer.get_pixel(x, y).0[0] as f32 / 255.0;
            let i = inner.get_pixel(x, y).0[0] as f32 / 255.0;
            let ring = (o - i).clamp(0.0, 1.0);
            let alpha = (ring * 0.85 * 255.0).round() as u8;
            sprite.put_pixel(x, y, Rgba([255, 255, 255, alpha]));
        }
    }

    Some(sprite)
}

// --- Arrière-plan « façon Tella » ---

/// Proportion de la largeur occupée par la capture posée sur l'arrière-plan
pub const BACKDROP_INSET: f32 = 0.86;

/// Géométrie de la capture une fois posée sur l'arrière-plan
#[derive(Debug, Clone, Copy)]
pub struct InsetRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Calcule la position/taille de la capture réduite, centrée, en dimensions paires.
pub fn inset_rect(width: u32, height: u32) -> InsetRect {
    let iw = even_floor(((width as f32) * BACKDROP_INSET) as u32).max(2);
    let ih = even_floor(((height as f32) * BACKDROP_INSET) as u32).max(2);
    InsetRect {
        x: (width.saturating_sub(iw)) / 2,
        y: (height.saturating_sub(ih)) / 2,
        width: iw,
        height: ih,
    }
}

fn even_floor(value: u32) -> u32 {
    value - (value % 2)
}

/// Largeur maximale de rendu de l'arrière-plan.
///
/// Un dégradé et une ombre floue ne contiennent aucun détail fin : les rendre
/// en pleine résolution puis les flouter coûtait plus de deux secondes en
/// build debug. On les calcule en petit et c'est ffmpeg qui les agrandit, sans
/// différence visible.
const BACKDROP_RENDER_WIDTH: u32 = 900;

/// Arrière-plan complet : dégradé diagonal + ombre portée de la capture
/// déjà incrustée. Rendu une seule fois, il ne coûte ensuite qu'un `overlay`.
///
/// L'image renvoyée peut être **plus petite** que `width` x `height` : elle
/// est destinée à être mise à l'échelle par le graphe de filtres.
pub fn backdrop_image(width: u32, height: u32, backdrop: Backdrop) -> RgbaImage {
    // Rendu dans un gabarit réduit, en conservant les proportions
    let (width, height) = if width > BACKDROP_RENDER_WIDTH {
        let ratio = BACKDROP_RENDER_WIDTH as f32 / width as f32;
        (
            BACKDROP_RENDER_WIDTH,
            (((height as f32) * ratio) as u32).max(2),
        )
    } else {
        (width.max(2), height.max(2))
    };

    let (from, to) = backdrop.colors();
    let (wu, hu) = (width as usize, height as usize);
    let mut pixels = vec![0u8; wu * hu * 4];

    let max_distance = (width + height) as f32;
    for y in 0..hu {
        let row = y * wu * 4;
        for x in 0..wu {
            let t = ((x + y) as f32 / max_distance).clamp(0.0, 1.0);
            // Interpolation douce (ease in-out) : dégradé moins « plat »
            let t = t * t * (3.0 - 2.0 * t);
            let index = row + x * 4;
            pixels[index] = (from[0] + (to[0] - from[0]) * t) as u8;
            pixels[index + 1] = (from[1] + (to[1] - from[1]) * t) as u8;
            pixels[index + 2] = (from[2] + (to[2] - from[2]) * t) as u8;
            pixels[index + 3] = 255;
        }
    }

    // Ombre portée de la capture, cuite dans l'arrière-plan
    let rect = inset_rect(width, height);
    let corner = shape_mask(rect.width, rect.height, Shape::Rounded);
    let corner_raw = corner.as_raw();

    let shadow_offset = ((height as f32) * 0.012) as i32;
    let mut field = vec![0u8; wu * hu];
    for y in 0..rect.height as usize {
        let ty = y as i32 + rect.y as i32 + shadow_offset;
        if ty < 0 || ty as usize >= hu {
            continue;
        }
        let source = y * rect.width as usize;
        let target = (ty as usize) * wu;

        for x in 0..rect.width as usize {
            let tx = x + rect.x as usize;
            if tx < wu {
                field[target + tx] = corner_raw[source + x];
            }
        }
    }

    let blur = ((height as f32) * 0.018).clamp(4.0, 28.0) as u32;
    let field = box_blur_gray(
        &GrayImage::from_raw(width, height, field).expect("champ d'ombre cohérent"),
        blur,
    );
    let field = field.as_raw();

    for y in 0..hu {
        let row = y * wu;
        for x in 0..wu {
            let shade = field[row + x];
            if shade == 0 {
                continue;
            }

            // L'ombre assombrit le dégradé, au plus de 45 %
            let alpha = (shade as u32) * 45 / 100;
            let index = (row + x) * 4;
            for channel in 0..3 {
                let base = pixels[index + channel] as u32;
                pixels[index + channel] = ((base * (255 - alpha)) / 255) as u8;
            }
        }
    }

    RgbaImage::from_raw(width, height, pixels).expect("arrière-plan cohérent")
}

// --- Écriture sur disque ---

/// Écrit un masque en niveaux de gris et renvoie son chemin
pub fn write_gray(path: &Path, image: &GrayImage) -> Result<PathBuf, String> {
    image
        .save(path)
        .map_err(|e| format!("Écriture du masque impossible: {e}"))?;
    Ok(path.to_path_buf())
}

/// Écrit un sprite RGBA et renvoie son chemin
pub fn write_rgba(path: &Path, image: &RgbaImage) -> Result<PathBuf, String> {
    image
        .save(path)
        .map_err(|e| format!("Écriture du sprite impossible: {e}"))?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Coût réel de la génération des visuels : c'est fait au démarrage de
    /// l'enregistrement, donc directement perceptible par l'utilisateur.
    #[test]
    #[ignore = "diagnostic"]
    fn measure_visual_generation_cost() {
        let t = Instant::now();
        let _ = backdrop_image(1712, 962, Backdrop::Aurora);
        eprintln!("  → arrière-plan 1712x962 : {:?}", t.elapsed());

        let t = Instant::now();
        let _ = decoration_sprite(480, 480, Shape::Circle, Presentation::Bubble);
        eprintln!("  → sprite caméra 480x480 : {:?}", t.elapsed());

        let t = Instant::now();
        let _ = shape_mask(480, 360, Shape::Rounded);
        eprintln!("  → masque 480x360 : {:?}", t.elapsed());
    }

    #[test]
    fn circle_mask_is_opaque_at_center_and_clear_at_corner() {
        let mask = shape_mask(100, 100, Shape::Circle);
        assert_eq!(mask.get_pixel(50, 50).0[0], 255);
        assert_eq!(mask.get_pixel(0, 0).0[0], 0);
    }

    #[test]
    fn rounded_mask_keeps_edges_and_cuts_corners() {
        let mask = shape_mask(200, 120, Shape::Rounded);
        // Centre des bords : plein
        assert_eq!(mask.get_pixel(100, 2).0[0], 255);
        assert_eq!(mask.get_pixel(2, 60).0[0], 255);
        // Coin : coupé
        assert_eq!(mask.get_pixel(0, 0).0[0], 0);
    }

    #[test]
    fn square_mask_is_fully_opaque() {
        let mask = shape_mask(40, 30, Shape::Square);
        assert!(mask.pixels().all(|p| p.0[0] == 255));
    }

    #[test]
    fn inset_rect_is_centred_and_even() {
        let rect = inset_rect(1920, 1080);
        assert_eq!(rect.width % 2, 0);
        assert_eq!(rect.height % 2, 0);
        assert_eq!(rect.x * 2 + rect.width, 1920);
    }

    #[test]
    fn decoration_sprite_has_padding_on_every_side() {
        let sprite = decoration_sprite(100, 80, Shape::Rounded, Presentation::Studio);
        assert_eq!(sprite.width(), 100 + SPRITE_PAD * 2);
        assert_eq!(sprite.height(), 80 + SPRITE_PAD * 2);
    }
}
