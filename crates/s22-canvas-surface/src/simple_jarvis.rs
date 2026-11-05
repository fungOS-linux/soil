//! Thin painter projection of canonical GFX reactor geometry; no widget logic.
use egui::{Color32, Pos2, Stroke};
use fpl_gfx::{
    Theme, ThemeRole,
    reactor::{self, Activity, Mark, Paint},
};

// Retained original clean reactor composition for callers migrating captures.
#[allow(dead_code)]
pub(crate) fn show(
    ui: &mut egui::Ui,
    battery: super::BatteryView,
    estimate: Option<super::ModelEstimateView>,
) {
    let theme = Theme::jarvis_hud();
    let color = |role, alpha: f32| {
        let [r, g, b, a] = theme.color(role).to_srgb8();
        Color32::from_rgba_unmultiplied(r, g, b, (a as f32 * alpha) as u8)
    };
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, color(ThemeRole::Background, 1.0));
    let compact = ui.available_width() <= 160.0;
    let font = if compact { 12.0 } else { 18.0 };

    let card_height = if compact {
        if estimate.is_some() { 48.0 } else { 64.0 }
    } else {
        116.0
    };
    let status_height = 0.0;
    let estimate_height = if estimate.is_some() {
        if compact { 44.0 } else { 56.0 }
    } else {
        0.0
    };
    let available = ui.available_size();
    let size = egui::vec2(
        available.x,
        (available.y
            - card_height
            - status_height
            - estimate_height
            - ui.spacing().item_spacing.y * if estimate.is_some() { 2.0 } else { 1.0 })
        .max(1.0),
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    project_reactor(ui, rect, 0.0);
    let (card, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), card_height),
        egui::Sense::hover(),
    );
    debug_assert!(card.max.y <= ui.max_rect().max.y + 0.1);
    fpl_gfx::status_widgets::visit_battery_composed(battery, compact, estimate.is_some(), |mark| {
        project_card(ui, card, font, &theme, mark)
    })
    .expect("validated owner battery status");
    if let Some(view) = estimate {
        let (card, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), estimate_height),
            egui::Sense::hover(),
        );
        debug_assert!(card.max.y <= ui.max_rect().max.y + 0.1);
        fpl_gfx::status_widgets::model_estimate::visit(view, |mark| {
            project_card(ui, card, font, &theme, mark)
        })
        .expect("validated model estimate");
    }
}

/// Project canonical bounded Resting marks; no activity or audio inference.
pub(crate) fn project_reactor(ui: &egui::Ui, rect: egui::Rect, seconds: f32) {
    let theme = Theme::jarvis_hud();
    let color = |role, alpha: f32| {
        let [r, g, b, a] = theme.color(role).to_srgb8();
        Color32::from_rgba_unmultiplied(r, g, b, (a as f32 * alpha) as u8)
    };
    let scale = (rect.width() / reactor::SIZE[0]).min(rect.height() / reactor::SIZE[1]);
    let center = rect.center();
    let painter = ui.painter().with_clip_rect(rect);
    reactor::visit(Activity::Resting, seconds, |mark| match mark {
        Mark::Disc {
            radius,
            paint,
            alpha,
        } => {
            let role = match paint {
                Paint::Surface => ThemeRole::Surface,
                Paint::Accent => ThemeRole::Primary,
                Paint::Ink => ThemeRole::Background,
            };
            painter.circle_filled(center, radius * scale, color(role, alpha));
        }
        Mark::Ring {
            radius,
            width,
            alpha,
        } => {
            painter.circle_stroke(
                center,
                radius * scale,
                Stroke::new(width * scale, color(ThemeRole::Primary, alpha)),
            );
        }
        Mark::Arc {
            radius,
            start,
            end,
            width,
            alpha,
        } => {
            let points: Vec<Pos2> = (0..=32)
                .map(|i| {
                    let a = start + (end - start) * i as f32 / 32.0;
                    center + egui::vec2(a.cos(), a.sin()) * radius * scale
                })
                .collect();
            painter.add(egui::Shape::line(
                points,
                Stroke::new(width * scale, color(ThemeRole::Primary, alpha)),
            ));
        }
        Mark::Bar {
            x,
            height,
            width,
            alpha,
        } => {
            let r = egui::Rect::from_min_size(
                center + egui::vec2(x, -height / 2.0) * scale,
                egui::vec2(width, height) * scale,
            );
            painter.rect_filled(r, 0.0, color(ThemeRole::Background, alpha));
        }
    });
}

pub(crate) fn project_card(
    ui: &egui::Ui,
    rect: egui::Rect,
    font: f32,
    theme: &Theme,
    mark: fpl_gfx::status_widgets::Mark,
) {
    let painter = ui.painter().with_clip_rect(rect);
    let point = |x, y| rect.min + egui::vec2(x * rect.width(), y * rect.height());
    match mark {
        fpl_gfx::status_widgets::Mark::Line { points, role } => {
            let [r, g, b, a] = theme.color(role).to_srgb8();
            painter.add(egui::Shape::line(
                points.into_iter().map(|[x, y]| point(x, y)).collect(),
                Stroke::new(
                    if rect.width() <= 160.0 {
                        1.5_f32
                    } else {
                        2.0_f32
                    },
                    Color32::from_rgba_unmultiplied(r, g, b, a),
                ),
            ));
        }
        fpl_gfx::status_widgets::Mark::Rect {
            bounds: [x, y, w, h],
            role,
        } => {
            let [r, g, b, a] = theme.color(role).to_srgb8();
            painter.rect_filled(
                egui::Rect::from_min_size(
                    point(x, y),
                    egui::vec2(w * rect.width(), h * rect.height()),
                ),
                0.0,
                Color32::from_rgba_unmultiplied(r, g, b, a),
            );
        }
        fpl_gfx::status_widgets::Mark::Text {
            at: [x, y],
            text,
            role,
        } => {
            let [r, g, b, a] = theme.color(role).to_srgb8();
            let mut job = egui::text::LayoutJob::simple(
                text,
                egui::FontId::proportional(font),
                Color32::from_rgba_unmultiplied(r, g, b, a),
                rect.width() * (1.0 - x),
            );
            job.wrap.max_rows = if job.text == "Estimated SOC unavailable" {
                2
            } else {
                1
            };
            job.wrap.break_anywhere = true;
            painter.galley(
                point(x, y),
                painter.layout_job(job),
                Color32::from_rgba_unmultiplied(r, g, b, a),
            );
        }
    }
}
