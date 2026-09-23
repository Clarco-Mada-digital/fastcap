// FastCap - Composition écran + incrustation webcam, pour les aperçus
//
// Pendant l'enregistrement, c'est ffmpeg qui compose : son graphe de filtres
// reçoit les mêmes masques et les mêmes ombres pré-rendus (voir `visuals`).
// Ce module sert à produire l'**aperçu** affiché dans l'interface, avec une
// géométrie identique à celle du graphe — l'utilisateur voit donc exactement
// ce qui sera enregistré, sans avoir à lancer un encodage.
//
// Les masques et les ombres ne sont régénérés qu'au changement de style,
// jamais par image.

use image::{imageops, GrayImage, Rgba, RgbaImage};

use crate::visuals::{self, Presentation, Shape, SPRITE_PAD};

/// Disposition de l'incrustation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    PipTopLeft,
    PipTopRight,
    PipBottomLeft,
    PipBottomRight,
    SideLeft,
    SideRight,
    Full,
}

impl Layout {
    pub fn parse(value: &str) -> Self {
        match value {
            "pip-tl" => Layout::PipTopLeft,
            "pip-tr" => Layout::PipTopRight,
            "pip-bl" => Layout::PipBottomLeft,
            "side-left" => Layout::SideLeft,
            "side-right" => Layout::SideRight,
            "full" => Layout::Full,
            _ => Layout::PipBottomRight,
        }
    }


    pub fn is_pip(self) -> bool {
        matches!(
            self,
            Layout::PipTopLeft | Layout::PipTopRight | Layout::PipBottomLeft | Layout::PipBottomRight
        )
    }

}

/// Style courant de l'incrustation, ajustable en direct
#[derive(Debug, Clone)]
pub struct CamStyle {
    pub layout: Layout,
    pub shape: Shape,
    pub presentation: Presentation,
    /// Largeur de l'incrustation en pourcentage de la largeur de sortie
    pub size_percent: u32,
    pub margin: u32,
    pub offset_x: i32,
    pub offset_y: i32,
}

impl CamStyle {
    /// Dimensions de l'incrustation pour une sortie donnée
    fn cam_size(&self, output_width: u32, output_height: u32) -> (u32, u32) {
        match self.layout {
            Layout::Full => (output_width, output_height),
            Layout::SideLeft | Layout::SideRight => (output_width / 2, output_height),
            _ => {
                let width = ((output_width as f32) * (self.size_percent.clamp(8, 60) as f32)
                    / 100.0) as u32;
                let width = even(width.clamp(80, output_width));
                // Le cercle et la bulle sont carrés, le reste garde du 4:3
                let height = match (self.shape, self.presentation) {
                    (Shape::Circle, _) | (_, Presentation::Bubble) => width,
                    _ => even(width * 3 / 4),
                };
                (width, height.min(output_height))
            }
        }
    }
}

fn even(value: u32) -> u32 {
    value - (value % 2)
}

/// Éléments graphiques dépendant du style, régénérés seulement à son changement
struct CamAssets {
    width: u32,
    height: u32,
    shape: Shape,
    presentation: Presentation,
    mask: GrayImage,
    decoration: Option<RgbaImage>,
    border: Option<RgbaImage>,
}

impl CamAssets {
    fn build(width: u32, height: u32, shape: Shape, presentation: Presentation) -> Self {
        // La « bulle » impose un cercle quelle que soit la forme demandée
        let effective_shape = if presentation == Presentation::Bubble {
            Shape::Circle
        } else {
            shape
        };

        Self {
            width,
            height,
            shape,
            presentation,
            mask: visuals::shape_mask(width, height, effective_shape),
            decoration: presentation
                .is_decorated()
                .then(|| visuals::decoration_sprite(width, height, effective_shape, presentation)),
            border: visuals::border_sprite(width, height, effective_shape, presentation),
        }
    }

    fn matches(&self, width: u32, height: u32, shape: Shape, presentation: Presentation) -> bool {
        self.width == width
            && self.height == height
            && self.shape == shape
            && self.presentation == presentation
    }
}

/// Assemble les images finales de l'enregistrement
pub struct Compositor {
    output_width: u32,
    output_height: u32,
    assets: Option<CamAssets>,
    /// Webcam déjà redimensionnée, réutilisée tant qu'aucune nouvelle image n'arrive
    scaled_cam: Option<RgbaImage>,
    scaled_cam_key: Option<(u64, u32, u32)>,
}

impl Compositor {
    pub fn new(output_width: u32, output_height: u32) -> Self {
        Self {
            output_width,
            output_height,
            assets: None,
            scaled_cam: None,
            scaled_cam_key: None,
        }
    }

    /// Position de l'incrustation, décalages utilisateur compris
    fn pip_position(&self, style: &CamStyle, cam_w: u32, cam_h: u32) -> (i32, i32) {
        let screen_w = self.output_width as i32;
        let screen_h = self.output_height as i32;
        let margin = style.margin.min(300) as i32;
        let (cam_w, cam_h) = (cam_w as i32, cam_h as i32);

        let (base_x, base_y) = match style.layout {
            Layout::PipTopLeft => (margin, margin),
            Layout::PipTopRight => (screen_w - margin - cam_w, margin),
            Layout::PipBottomLeft => (margin, screen_h - margin - cam_h),
            _ => (screen_w - margin - cam_w, screen_h - margin - cam_h),
        };

        // On laisse l'incrustation dépasser un peu, mais jamais totalement
        let max_x = (screen_w - cam_w).max(0);
        let max_y = (screen_h - cam_h).max(0);

        (
            (base_x + style.offset_x).clamp(0, max_x),
            (base_y + style.offset_y).clamp(0, max_y),
        )
    }

    /// Redimensionne la webcam vers la taille voulue, en recadrant au centre
    /// pour remplir le cadre sans déformer.
    fn scale_cam(
        &mut self,
        cam: &RgbaImage,
        sequence: u64,
        target_w: u32,
        target_h: u32,
    ) -> &RgbaImage {
        let key = (sequence, target_w, target_h);
        if self.scaled_cam_key != Some(key) {
            let (src_w, src_h) = (cam.width(), cam.height());

            // Échelle « cover » : on couvre la cible puis on recadre
            let scale = (target_w as f32 / src_w as f32).max(target_h as f32 / src_h as f32);
            let fit_w = ((src_w as f32) * scale).ceil() as u32;
            let fit_h = ((src_h as f32) * scale).ceil() as u32;

            let resized = imageops::resize(cam, fit_w.max(1), fit_h.max(1), imageops::FilterType::Triangle);
            let crop_x = (fit_w.saturating_sub(target_w)) / 2;
            let crop_y = (fit_h.saturating_sub(target_h)) / 2;
            let cropped =
                imageops::crop_imm(&resized, crop_x, crop_y, target_w, target_h).to_image();

            self.scaled_cam = Some(cropped);
            self.scaled_cam_key = Some(key);
        }

        self.scaled_cam.as_ref().expect("webcam redimensionnée")
    }

    /// Compose l'image finale. `screen` est consommée et sert de toile.
    pub fn compose(
        &mut self,
        screen: RgbaImage,
        cam: Option<(&RgbaImage, u64)>,
        style: Option<&CamStyle>,
    ) -> RgbaImage {
        let (Some(style), Some((cam_image, sequence))) = (style, cam) else {
            return screen;
        };

        let (cam_w, cam_h) = style.cam_size(self.output_width, self.output_height);
        if cam_w == 0 || cam_h == 0 {
            return screen;
        }

        match style.layout {
            Layout::Full => {
                // La caméra remplit tout le cadre : l'écran n'est pas utilisé
                self.scale_cam(cam_image, sequence, self.output_width, self.output_height)
                    .clone()
            }
            Layout::SideLeft | Layout::SideRight => {
                self.compose_side(screen, cam_image, sequence, style, cam_w, cam_h)
            }
            _ => self.compose_pip(screen, cam_image, sequence, style, cam_w, cam_h),
        }
    }

    /// Incrustation en coin, avec ombre/halo puis liseré
    fn compose_pip(
        &mut self,
        mut canvas: RgbaImage,
        cam_image: &RgbaImage,
        sequence: u64,
        style: &CamStyle,
        cam_w: u32,
        cam_h: u32,
    ) -> RgbaImage {
        if !self
            .assets
            .as_ref()
            .map(|a| a.matches(cam_w, cam_h, style.shape, style.presentation))
            .unwrap_or(false)
        {
            self.assets = Some(CamAssets::build(
                cam_w,
                cam_h,
                style.shape,
                style.presentation,
            ));
        }

        let (x, y) = self.pip_position(style, cam_w, cam_h);

        // 1. Ombre portée / halo, derrière l'incrustation
        if let Some(assets) = &self.assets {
            if let Some(decoration) = &assets.decoration {
                blend_rgba(
                    &mut canvas,
                    decoration,
                    x - SPRITE_PAD as i32,
                    y - SPRITE_PAD as i32,
                );
            }
        }

        // 2. La caméra elle-même, découpée par son masque
        let scaled = self.scale_cam(cam_image, sequence, cam_w, cam_h).clone();
        if let Some(assets) = &self.assets {
            blend_masked(&mut canvas, &scaled, &assets.mask, x, y);

            // 3. Liseré par-dessus
            if let Some(border) = &assets.border {
                blend_rgba(&mut canvas, border, x, y);
            }
        }

        canvas
    }

    /// Écran et caméra côte à côte, chacun sur une moitié du cadre
    fn compose_side(
        &mut self,
        screen: RgbaImage,
        cam_image: &RgbaImage,
        sequence: u64,
        style: &CamStyle,
        cam_w: u32,
        cam_h: u32,
    ) -> RgbaImage {
        let mut canvas = RgbaImage::from_pixel(
            self.output_width,
            self.output_height,
            Rgba([12, 14, 20, 255]),
        );

        let half = self.output_width / 2;

        // L'écran garde ses proportions et est centré dans sa moitié
        let scale = (half as f32 / screen.width() as f32)
            .min(self.output_height as f32 / screen.height() as f32);
        let screen_w = ((screen.width() as f32) * scale).max(1.0) as u32;
        let screen_h = ((screen.height() as f32) * scale).max(1.0) as u32;
        let screen_scaled = imageops::resize(
            &screen,
            screen_w,
            screen_h,
            imageops::FilterType::Triangle,
        );

        let cam_scaled = self.scale_cam(cam_image, sequence, cam_w, cam_h).clone();

        let (screen_x, cam_x) = match style.layout {
            Layout::SideRight => (0i32, half as i32),
            _ => (half as i32, 0i32),
        };

        let screen_y = ((self.output_height.saturating_sub(screen_h)) / 2) as i32;
        let screen_x = screen_x + ((half.saturating_sub(screen_w)) / 2) as i32;

        blend_opaque(&mut canvas, &screen_scaled, screen_x, screen_y);
        blend_opaque(&mut canvas, &cam_scaled, cam_x, 0);

        canvas
    }
}

// --- Primitives de mélange ---

/// Copie une image opaque à la position donnée (sans lire son canal alpha)
fn blend_opaque(canvas: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) {
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);

    for sy in 0..src.height() as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= ch {
            continue;
        }
        for sx in 0..src.width() as i32 {
            let tx = x + sx;
            if tx < 0 || tx >= cw {
                continue;
            }
            let pixel = *src.get_pixel(sx as u32, sy as u32);
            canvas.put_pixel(tx as u32, ty as u32, pixel);
        }
    }
}

/// Mélange une image RGBA sur la toile (« source over »)
fn blend_rgba(canvas: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) {
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);

    for sy in 0..src.height() as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= ch {
            continue;
        }
        for sx in 0..src.width() as i32 {
            let tx = x + sx;
            if tx < 0 || tx >= cw {
                continue;
            }

            let source = *src.get_pixel(sx as u32, sy as u32);
            let alpha = source.0[3] as u32;
            if alpha == 0 {
                continue;
            }

            let target = canvas.get_pixel_mut(tx as u32, ty as u32);
            for channel in 0..3 {
                let s = source.0[channel] as u32;
                let d = target.0[channel] as u32;
                target.0[channel] = ((s * alpha + d * (255 - alpha)) / 255) as u8;
            }
            target.0[3] = 255;
        }
    }
}

/// Mélange une image en utilisant un masque en niveaux de gris comme alpha
fn blend_masked(canvas: &mut RgbaImage, src: &RgbaImage, mask: &GrayImage, x: i32, y: i32) {
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);

    for sy in 0..src.height() as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= ch {
            continue;
        }
        for sx in 0..src.width() as i32 {
            let tx = x + sx;
            if tx < 0 || tx >= cw {
                continue;
            }

            let alpha = mask.get_pixel(sx as u32, sy as u32).0[0] as u32;
            if alpha == 0 {
                continue;
            }

            let source = *src.get_pixel(sx as u32, sy as u32);
            let target = canvas.get_pixel_mut(tx as u32, ty as u32);

            if alpha == 255 {
                target.0[0] = source.0[0];
                target.0[1] = source.0[1];
                target.0[2] = source.0[2];
            } else {
                for channel in 0..3 {
                    let s = source.0[channel] as u32;
                    let d = target.0[channel] as u32;
                    target.0[channel] = ((s * alpha + d * (255 - alpha)) / 255) as u8;
                }
            }
            target.0[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(layout: Layout) -> CamStyle {
        CamStyle {
            layout,
            shape: Shape::Rounded,
            presentation: Presentation::Minimal,
            size_percent: 25,
            margin: 20,
            offset_x: 0,
            offset_y: 0,
        }
    }

    #[test]
    fn pip_sits_in_the_requested_corner() {
        let compositor = Compositor::new(1000, 600);
        let s = style(Layout::PipTopLeft);
        assert_eq!(compositor.pip_position(&s, 200, 150), (20, 20));

        let s = style(Layout::PipBottomRight);
        assert_eq!(compositor.pip_position(&s, 200, 150), (780, 430));
    }

    #[test]
    fn pip_never_leaves_the_frame() {
        let compositor = Compositor::new(1000, 600);
        let mut s = style(Layout::PipTopLeft);
        s.offset_x = -5000;
        s.offset_y = 5000;

        let (x, y) = compositor.pip_position(&s, 200, 150);
        assert_eq!(x, 0);
        assert_eq!(y, 450);
    }

    #[test]
    fn cam_size_is_even_and_proportional() {
        let s = style(Layout::PipBottomRight);
        let (w, h) = s.cam_size(1920, 1080);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
        assert_eq!(w, 480); // 25 % de 1920
    }

    #[test]
    fn circle_and_bubble_are_square() {
        let mut s = style(Layout::PipBottomRight);
        s.shape = Shape::Circle;
        let (w, h) = s.cam_size(1920, 1080);
        assert_eq!(w, h);

        let mut s = style(Layout::PipBottomRight);
        s.presentation = Presentation::Bubble;
        let (w, h) = s.cam_size(1920, 1080);
        assert_eq!(w, h);
    }

    #[test]
    fn full_layout_replaces_the_screen_entirely() {
        let mut compositor = Compositor::new(320, 240);
        let screen = RgbaImage::from_pixel(320, 240, Rgba([255, 0, 0, 255]));
        let cam = RgbaImage::from_pixel(64, 48, Rgba([0, 255, 0, 255]));

        let out = compositor.compose(screen, Some((&cam, 1)), Some(&style(Layout::Full)));
        assert_eq!(out.get_pixel(160, 120).0[1], 255, "la caméra doit remplir le cadre");
    }

    #[test]
    fn without_a_camera_the_screen_passes_through_untouched() {
        let mut compositor = Compositor::new(64, 48);
        let screen = RgbaImage::from_pixel(64, 48, Rgba([7, 9, 11, 255]));

        let out = compositor.compose(screen, None, Some(&style(Layout::PipBottomRight)));
        assert_eq!(out.get_pixel(0, 0).0, [7, 9, 11, 255]);
    }

    #[test]
    fn masked_blend_respects_transparency() {
        let mut canvas = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 255]));
        let src = RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 255]));
        let mut mask = GrayImage::new(4, 4);
        mask.put_pixel(0, 0, image::Luma([255]));

        blend_masked(&mut canvas, &src, &mask, 2, 2);
        assert_eq!(canvas.get_pixel(2, 2).0[0], 255, "pixel masqué à 255 : recopié");
        assert_eq!(canvas.get_pixel(3, 3).0[0], 0, "pixel masqué à 0 : inchangé");
    }

    #[test]
    fn blending_outside_the_canvas_is_clipped() {
        let mut canvas = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let src = RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 255]));

        // Débordements sur les quatre côtés : ne doit pas paniquer
        blend_rgba(&mut canvas, &src, -3, -3);
        blend_rgba(&mut canvas, &src, 6, 6);
        assert_eq!(canvas.get_pixel(0, 0).0[0], 255);
    }
}
