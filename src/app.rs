use crate::tun_proxy::{self, ProxyHandle, ProxyStatus, UpstreamConfig};
use crate::models::{AppData, TestStatus};
use crate::ui::detail::DetailTab;
use std::sync::{Arc, Mutex};

/// Application-wide mutable state shared across UI panels.
pub struct AppState {
    pub data: AppData,
    pub rt: Arc<tokio::runtime::Runtime>,

    // Selection state
    pub selected_proxy_id: Option<String>,

    // Detail panel
    pub detail_tab: DetailTab,
    pub show_password: bool,

    // Local transparent proxy
    pub proxy_handle: Option<ProxyHandle>,
    pub proxy_status: Arc<Mutex<ProxyStatus>>,

    // Async test tracking
    pub pending_test: Option<(String, Arc<Mutex<TestStatus>>)>,

    // Dirty flag
    pub needs_save: bool,

    // Save error message
    pub save_error: Option<String>,

    // egui context for repaint requests
    pub egui_ctx: Option<egui::Context>,

    // Track whether theme has been applied
    pub theme_applied: bool,
}

impl AppState {
    pub fn new(rt: Arc<tokio::runtime::Runtime>) -> Self {
        let data = crate::storage::load();
        Self {
            data,
            rt,
            selected_proxy_id: None,
            detail_tab: DetailTab::Basic,
            show_password: false,
            proxy_handle: None,
            proxy_status: Arc::new(Mutex::new(ProxyStatus::default())),
            pending_test: None,
            needs_save: false,
            save_error: None,
            egui_ctx: None,
            theme_applied: false,
        }
    }

    /// Poll pending test result and apply it to the matching proxy.
    pub fn poll_test_result(&mut self) {
        if let Some((proxy_id, status_arc)) = &self.pending_test {
            let status = status_arc.lock().unwrap().clone();
            match &status {
                TestStatus::Testing => {} // still running
                _ => {
                    let pid = proxy_id.clone();
                    if let Some(proxy) = self.data.proxies.iter_mut().find(|p| p.id == pid) {
                        proxy.test_status = status;
                    }
                    self.pending_test = None;
                }
            }
        }
    }

    /// Start or restart the transparent proxy with the active proxy config.
    pub fn apply_proxy(&mut self) {
        log::info!("apply_proxy() called");

        // Stop existing proxy
        if let Some(handle) = self.proxy_handle.take() {
            log::info!("Stopping existing proxy");
            handle.stop();
        }

        let proxy = self.data.active_proxy().cloned();

        let Some(proxy) = proxy else {
            log::info!("No active proxy configured");
            let mut s = self.proxy_status.lock().unwrap();
            s.running = false;
            s.error = None;
            s.tun_addr.clear();
            return;
        };

        log::info!("Active proxy: {} ({}://{}:{})",
            proxy.name, proxy.proxy_type, proxy.host, proxy.port);

        if proxy.host.is_empty() {
            log::warn!("Proxy host is empty, skipping");
            let mut s = self.proxy_status.lock().unwrap();
            s.running = false;
            s.error = Some("Proxy host is empty".to_string());
            return;
        }

        let config = UpstreamConfig::from_proxy(&proxy);
        let ctx = self.egui_ctx.clone().unwrap_or_else(|| egui::Context::default());
        let tun_addr = self.data.tun_addr.clone();

        log::info!("Starting local proxy...");
        match tun_proxy::start(&self.rt, config, self.proxy_status.clone(), ctx, &tun_addr) {
            Ok(handle) => {
                log::info!("Local proxy started successfully");
                self.proxy_handle = Some(handle);
            }
            Err(e) => {
                log::error!("Local proxy start failed: {e}");
                let mut s = self.proxy_status.lock().unwrap();
                s.running = false;
                s.error = Some(e);
            }
        }
    }

    fn do_save(&mut self) {
        match crate::storage::save(&self.data) {
            Ok(()) => {
                self.save_error = None;
            }
            Err(e) => {
                self.save_error = Some(e);
            }
        }
        self.needs_save = false;
    }
}

// ---------------------------------------------------------------------------
// eframe App implementation
// ---------------------------------------------------------------------------

pub struct App {
    pub state: AppState,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, rt: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            state: AppState::new(rt),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Apply theme once
        if !self.state.theme_applied {
            crate::ui::apply_theme(ctx);
            self.state.theme_applied = true;
        }

        // Store egui context for repaint requests
        if self.state.egui_ctx.is_none() {
            self.state.egui_ctx = Some(ctx.clone());
        }

        // Poll async test results
        self.state.poll_test_result();

        // Top panel: title + proxy status
        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::none()
                .fill(crate::ui::BG_DARKEST)
                .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                .stroke(egui::Stroke::new(1.0, crate::ui::BORDER)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Proxy Manager")
                            .size(18.0)
                            .strong()
                            .color(crate::ui::TEXT_PRIMARY),
                    );

                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("v0.1")
                            .size(11.0)
                            .color(crate::ui::TEXT_MUTED),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status = self.state.proxy_status.lock().unwrap();
                        if status.running {
                            let pill = egui::RichText::new(format!("  ON  {}  ", status.tun_addr))
                                .size(11.0)
                                .strong()
                                .color(egui::Color32::from_rgb(18, 18, 18));
                            ui.label(pill.background_color(crate::ui::COLOR_SUCCESS));

                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!("{} conn", status.connections))
                                    .size(11.0)
                                    .color(crate::ui::TEXT_MUTED),
                            );
                        } else {
                            let pill = egui::RichText::new("  OFF  ")
                                .size(11.0)
                                .strong()
                                .color(crate::ui::TEXT_MUTED);
                            ui.label(pill.background_color(crate::ui::BG_ELEVATED));
                        }
                        if let Some(err) = &status.error {
                            ui.label(
                                egui::RichText::new(err.as_str())
                                    .size(11.0)
                                    .color(crate::ui::COLOR_FAILED),
                            );
                        }
                    });
                });

                if let Some(err) = &self.state.save_error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Save error: {err}"))
                            .size(11.0)
                            .color(crate::ui::COLOR_FAILED),
                    );
                }
            });

        // Main area: 2-pane layout (proxy list + detail)
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(crate::ui::BG_DARK))
            .show(ctx, |ui| {
                crate::ui::proxy_list::render(ui, &mut self.state);
                crate::ui::detail::render(ui, &mut self.state);
            });

        // Auto-save when dirty
        if self.state.needs_save {
            self.state.do_save();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(handle) = self.state.proxy_handle.take() {
            handle.stop();
        }
        let _ = crate::storage::save(&self.state.data);
    }
}
