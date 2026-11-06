//! Native Soil consent popup.
//!
//! This is the first surface for `soil-consent`: a host-owned, single-request
//! popup. It listens on a per-user Unix socket, blocks each caller until a
//! decision, and renders one request at a time. Canvas/Spore composition comes
//! later; this binary is deliberately small and native.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use serde::Serialize;
use soil_consent::{Broker, Decision, Effect, Pending, Preview, Request, Scope};

fn main() -> anyhow::Result<()> {
    let socket = std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(default_socket);
    let broker = Arc::new(Broker::new());
    start_server(broker.clone(), socket.clone())?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 680.0])
            .with_min_inner_size([420.0, 420.0])
            .with_title("Soil · Permission request")
            .with_visible(false),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "soil-consent",
        options,
        Box::new(move |cc| Ok(Box::new(ConsentApp::new(cc, broker)))),
    )?;
    Ok(())
}

fn default_socket() -> PathBuf {
    std::env::temp_dir().join("soil-consent.sock")
}

fn start_server(broker: Arc<Broker>, path: PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => anyhow::bail!("consent socket already in use: {}", path.display()),
            Err(_) => std::fs::remove_file(&path)?,
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("soil-consent-popup: listening on {}", path.display());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                break;
            };
            let broker = broker.clone();
            std::thread::spawn(move || {
                let _ = serve_one(stream, broker);
            });
        }
    });
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Response {
    Decision { decision: Decision },
    Error { error: String },
}

fn serve_one(stream: UnixStream, broker: Arc<Broker>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.len() > 1024 * 1024 {
        return write_response(
            stream,
            Response::Error {
                error: "request too large".into(),
            },
        );
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                stream,
                Response::Error {
                    error: format!("invalid request: {error}"),
                },
            );
        }
    };
    match broker.request(request) {
        Ok(decision) => write_response(stream, Response::Decision { decision }),
        Err(error) => write_response(
            stream,
            Response::Error {
                error: error.to_string(),
            },
        ),
    }
}

fn write_response(mut stream: UnixStream, response: Response) -> anyhow::Result<()> {
    let encoded = serde_json::to_string(&response)?;
    stream.write_all(encoded.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

struct ConsentApp {
    broker: Arc<Broker>,
    active: Option<Pending>,
    theme: Theme,
}

#[derive(Clone, Copy)]
struct Theme {
    bg: egui::Color32,
    panel: egui::Color32,
    text: egui::Color32,
    muted: egui::Color32,
    accent: egui::Color32,
    danger: egui::Color32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: egui::Color32::from_rgb(15, 18, 24),
            panel: egui::Color32::from_rgb(24, 29, 38),
            text: egui::Color32::from_rgb(232, 237, 245),
            muted: egui::Color32::from_rgb(146, 158, 176),
            accent: egui::Color32::from_rgb(103, 178, 255),
            danger: egui::Color32::from_rgb(255, 112, 112),
        }
    }
}

impl ConsentApp {
    fn new(cc: &eframe::CreationContext<'_>, broker: Arc<Broker>) -> Self {
        let theme = Theme::default();
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = theme.bg;
        visuals.window_fill = theme.panel;
        visuals.override_text_color = Some(theme.text);
        visuals.widgets.active.bg_fill = theme.accent;
        visuals.widgets.hovered.bg_fill = theme.accent.gamma_multiply(0.75);
        visuals.selection.bg_fill = theme.accent.gamma_multiply(0.45);
        cc.egui_ctx.set_visuals(visuals);
        Self {
            broker,
            active: None,
            theme,
        }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        if self.active.is_none() {
            self.active = self.broker.pending().into_iter().next();
            if self.active.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
    }

    fn decide(&mut self, ctx: &egui::Context, allowed: bool, scope: Scope) {
        let Some(pending) = self.active.take() else {
            return;
        };
        let decision = Decision {
            request_id: pending.request.id,
            allowed,
            scope,
            reason: if allowed {
                None
            } else {
                Some("denied by operator".into())
            },
            decided_by: "native-consent-popup".into(),
            decided_at_ms: now_ms(),
        };
        let _ = self.broker.resolve(decision);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
}

impl eframe::App for ConsentApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh(ctx);
        ctx.request_repaint_after(Duration::from_millis(200));
        let Some(pending) = self.active.as_ref() else {
            return;
        };
        let request = pending.request.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("SOIL · TRUSTED CONSENT")
                        .small()
                        .color(self.theme.accent),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("host-owned")
                            .small()
                            .color(self.theme.muted),
                    );
                });
            });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Permission request")
                    .size(25.0)
                    .strong()
                    .color(self.theme.text),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} wants to {}.",
                    request.requester, request.action
                ))
                .size(14.0)
                .color(self.theme.muted),
            );
            ui.add_space(14.0);
            facts(ui, &request, &self.theme);
            ui.add_space(12.0);
            preview(ui, &request, &self.theme);
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Deny").color(self.theme.danger))
                            .min_size(egui::vec2(96.0, 36.0)),
                    )
                    .clicked()
                {
                    self.decide(ctx, false, Scope::Once);
                }
                if ui
                    .add(egui::Button::new("Allow once").min_size(egui::vec2(120.0, 36.0)))
                    .clicked()
                {
                    self.decide(ctx, true, Scope::Once);
                }
                if request.requested_scope != Scope::Once
                    && ui
                        .add(
                            egui::Button::new("Allow for session")
                                .min_size(egui::vec2(150.0, 36.0)),
                        )
                        .clicked()
                {
                    self.decide(ctx, true, Scope::Session);
                }
                if request.requested_scope == Scope::Persistent
                    && ui
                        .add(egui::Button::new("Allow").min_size(egui::vec2(96.0, 36.0)))
                        .clicked()
                {
                    self.decide(ctx, true, Scope::Persistent);
                }
            });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Approval is bound to the payload digest. The preview is presentation only.",
                )
                .small()
                .color(self.theme.muted),
            );
        });
    }
}

fn facts(ui: &mut egui::Ui, request: &Request, theme: &Theme) {
    egui::Frame::new()
        .fill(theme.panel)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            row(ui, theme, "Requested by", &request.requester);
            row(ui, theme, "Principal", &request.principal);
            row(ui, theme, "Action", &request.action);
            row(
                ui,
                theme,
                "Effects",
                &request
                    .effects
                    .iter()
                    .map(|effect| match effect {
                        Effect::Read => "read",
                        Effect::Write => "write",
                        Effect::External => "external",
                        Effect::Destructive => "destructive",
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if !request.resources.is_empty() {
                row(ui, theme, "Resources", &request.resources.join(", "));
            }
            if !request.scopes.is_empty() {
                row(ui, theme, "Scopes", &request.scopes.join(", "));
            }
            row(
                ui,
                theme,
                "Lifetime",
                &format_scope(request.requested_scope),
            );
            row(ui, theme, "Payload", &truncate(&request.payload_digest, 44));
        });
}

fn preview(ui: &mut egui::Ui, request: &Request, theme: &Theme) {
    match &request.preview {
        Preview::None => {}
        Preview::SporeScene {
            spore,
            view,
            summary,
        } => preview_frame(
            ui,
            theme,
            "Spore preview",
            summary,
            &format!("{spore} · {view}"),
        ),
        Preview::Artifact {
            media_type,
            summary,
            digest,
        } => preview_frame(
            ui,
            theme,
            "Outbound preview",
            summary,
            &format!("{media_type} · {}", truncate(digest, 32)),
        ),
        Preview::Schema { schema, json } => {
            preview_frame(ui, theme, "Request details", schema, json);
        }
    }
}

fn preview_frame(ui: &mut egui::Ui, theme: &Theme, title: &str, body: &str, meta: &str) {
    egui::Frame::new()
        .fill(theme.panel)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).small().color(theme.accent));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(body).size(14.0).color(theme.text));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(meta).small().color(theme.muted));
        });
}

fn row(ui: &mut egui::Ui, theme: &Theme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().color(theme.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).small().color(theme.text));
        });
    });
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    format!("{}…", &value[..value.floor_char_boundary(max)])
}

fn format_scope(scope: Scope) -> String {
    match scope {
        Scope::Once => "once".into(),
        Scope::Session => "session".into(),
        Scope::Persistent => "persistent".into(),
        Scope::Until(until) => format!("until {until}"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
