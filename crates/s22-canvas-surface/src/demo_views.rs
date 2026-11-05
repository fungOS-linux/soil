//! Navigation and composition only; canonical GFX marks use the existing painter.
use crate::{BatteryView, Key, ModelEstimateView, simple_jarvis::project_card};
use fpl_gfx::{
    Theme, ThemeRole,
    status_widgets::{self, Mark},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DemoView {
    #[default]
    Home,
    Battery,
    DeviceStatus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Navigation {
    pub view: DemoView,
    /// Home action index: 0 = battery, 1 = device status.
    pub focus: usize,
    pub notice: Option<NavigationNotice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationNotice {
    RecentsUnavailable,
    PttOnly,
}
impl Navigation {
    pub(crate) fn key(&mut self, key: Key) {
        use DemoView::*;
        match key {
            Key::Back | Key::Home => *self = Self::default(),
            Key::Recents => self.notice = Some(NavigationNotice::RecentsUnavailable),
            Key::Ptt => self.notice = Some(NavigationNotice::PttOnly),
            Key::Left => {
                self.view = match self.view {
                    Home | Battery => Home,
                    DeviceStatus => Battery,
                }
            }
            Key::Right => {
                self.view = match self.view {
                    Home => Battery,
                    Battery | DeviceStatus => DeviceStatus,
                }
            }
            Key::Up if self.view == Home => self.focus = self.focus.saturating_sub(1),
            Key::Down | Key::Tab if self.view == Home => self.focus = (self.focus + 1).min(1),
            Key::Enter if self.view == Home => {
                self.view = if self.focus == 0 {
                    Battery
                } else {
                    DeviceStatus
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    nav: Navigation,
    battery: BatteryView,
    estimate: Option<ModelEstimateView>,
    reactor_seconds: Option<f32>,
    aurora: bool,
) {
    let theme = Theme::jarvis_hud();
    let [r, g, b, a] = theme.color(ThemeRole::Background).to_srgb8();
    ui.painter().rect_filled(
        ui.max_rect(),
        0.0,
        egui::Color32::from_rgba_unmultiplied(r, g, b, a),
    );
    if aurora {
        crate::aurora::paint(ui, ui.max_rect(), reactor_seconds.unwrap_or(0.0));
    }
    let compact = ui.available_width() <= 160.0;
    let font = if compact { 12.0 } else { 18.0 };
    let line_height = if compact { 18.0 } else { 32.0 };
    let line = |ui: &mut egui::Ui, text: String, role| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), line_height),
            egui::Sense::hover(),
        );
        project_card(
            ui,
            rect,
            font,
            &theme,
            Mark::Text {
                at: [0.02, 0.0],
                text,
                role,
            },
        );
    };
    if let Some(notice) = nav.notice {
        line(
            ui,
            match notice {
                NavigationNotice::RecentsUnavailable => "Recents",
                NavigationNotice::PttOnly => "PTT action",
            }
            .into(),
            ThemeRole::Primary,
        );
        line(
            ui,
            match notice {
                NavigationNotice::RecentsUnavailable => "Unavailable",
                NavigationNotice::PttOnly => "No recording",
            }
            .into(),
            ThemeRole::Warning,
        );
        line(ui, "Back: Home".into(), ThemeRole::TextMuted);
        return;
    }
    if nav.view == DemoView::Home {
        // Main hardware-test surface, never a launcher. Canonical reactor stays
        // visible even without an animation grant (static zero frame).
        let battery_height = if compact { 44.0 } else { 116.0 };
        let hint_height = if compact { 16.0 } else { 28.0 };
        let spacing = ui.spacing().item_spacing.y;
        let reactor_height =
            (ui.available_height() - battery_height - hint_height - 2.0 * spacing).max(1.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), reactor_height),
            egui::Sense::hover(),
        );
        crate::simple_jarvis::project_reactor(ui, rect, reactor_seconds.unwrap_or(0.0));
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), battery_height),
            egui::Sense::hover(),
        );
        status_widgets::visit_battery(battery, compact, |mark| {
            project_card(ui, rect, font, &theme, mark)
        })
        .expect("validated battery");
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), hint_height),
            egui::Sense::hover(),
        );
        project_card(
            ui,
            rect,
            font,
            &theme,
            Mark::Text {
                at: [0.02, 0.0],
                text: if nav.focus == 0 {
                    "> Battery"
                } else {
                    "> Device test"
                }
                .into(),
                role: ThemeRole::TextMuted,
            },
        );
        return;
    }
    line(
        ui,
        match nav.view {
            DemoView::Home => "Home",
            DemoView::Battery => "Battery",
            DemoView::DeviceStatus => "Device status",
        }
        .into(),
        ThemeRole::Primary,
    );
    match nav.view {
        DemoView::Home => unreachable!("Home composed above"),
        DemoView::Battery => {
            // Use owner freshness filtering, measured SOC and voltage formatting.
            let height = if compact {
                if estimate.is_some() { 44.0 } else { 58.0 }
            } else {
                116.0
            };
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), height),
                egui::Sense::hover(),
            );
            status_widgets::visit_battery_composed(battery, compact, estimate.is_some(), |mark| {
                project_card(ui, rect, font, &theme, mark)
            })
            .expect("validated battery");
            if let Some(view) = estimate {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), if compact { 38.0 } else { 56.0 }),
                    egui::Sense::hover(),
                );
                status_widgets::model_estimate::visit(view, |mark| {
                    project_card(ui, rect, font, &theme, mark)
                })
                .expect("validated estimate");
            } else {
                line(
                    ui,
                    format!("Source: {}", battery.state_label()),
                    ThemeRole::TextMuted,
                );
            }
        }
        DemoView::DeviceStatus => {
            // No service-status provider is wired. Never infer readiness from pixels.
            line(ui, "Services".into(), ThemeRole::Text);
            line(ui, "Unavailable".into(), ThemeRole::Warning);
        }
    }
}

pub(crate) fn volume_overlay(ui: &egui::Ui, view: crate::VolumeView) {
    let bounds = ui.max_rect();
    let compact = bounds.width() <= 160.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            bounds.min.x,
            bounds.max.y - if compact { 54.0 } else { 96.0 },
        ),
        egui::vec2(bounds.width(), if compact { 54.0 } else { 96.0 }),
    );
    let theme = Theme::jarvis_hud();
    status_widgets::volume::visit(view, |mark| {
        project_card(ui, rect, if compact { 12.0 } else { 18.0 }, &theme, mark)
    })
    .expect("validated volume");
}
