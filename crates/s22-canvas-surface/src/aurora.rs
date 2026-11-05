//! Opt-in aurora curtain background for the Jarvis surface.
//!
//! Deterministic: the only input is the host-supplied animation time. No
//! wall clock, random source, device state or guest authority. The effect is
//! intentionally cheap: a bounded set of vertical quads, no per-pixel work.

use egui::{Color32, Rect, pos2};

const BANDS: usize = 32;

/// Paint a dark aurora curtain over `rect`. `seconds` is the same host-owned
/// animation time already used by the reactor.
pub(crate) fn paint(ui: &egui::Ui, rect: Rect, seconds: f32) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }

    let painter = ui.painter().with_clip_rect(rect);
    // Base wash so the curtain has contrast even on a light theme.
    painter.rect_filled(rect, 0.0, Color32::from_rgb(2, 7, 12));

    let band_width = rect.width() / BANDS as f32;
    let seconds = seconds.max(0.0);
    for i in 0..BANDS {
        let x = i as f32 / BANDS as f32;
        // Two slow waves at different spatial frequencies make the curtain
        // drift instead of pulsing as one block.
        let phase = seconds * 0.55 + x * 9.0;
        let curtain = (phase.sin() * 0.5 + 0.5).powf(1.8);
        let drift = (seconds * 0.17 + x * 2.0).sin();
        let hue = 0.46 + 0.10 * drift + 0.06 * (x - 0.5);
        let [r, g, b] = hsv(hue, 0.72, 1.0);

        let x0 = rect.left() + i as f32 * band_width;
        let x1 = (x0 + band_width + 0.5).min(rect.right());
        let alpha = (10.0 + 55.0 * curtain) as u8;
        painter.rect_filled(
            Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom())),
            0.0,
            Color32::from_rgba_unmultiplied(r, g, b, alpha),
        );

        // Narrow bright core gives the "curtain" its vertical filament.
        let core_alpha = (6.0 + 28.0 * curtain) as u8;
        let cx0 = x0 + band_width * 0.38;
        let cx1 = x0 + band_width * 0.62;
        painter.rect_filled(
            Rect::from_min_max(
                pos2(cx0, rect.top()),
                pos2(cx1.min(rect.right()), rect.bottom()),
            ),
            0.0,
            Color32::from_rgba_unmultiplied(r, g, b, core_alpha),
        );
    }
}

/// HSV -> opaque RGB. Hue/saturation/value are clamped to `[0, 1]`.
fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_wraps_at_the_primary_boundaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(1.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(-1.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(0.5, 1.0, 1.0), [0, 255, 255]);
    }
}
