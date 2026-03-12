use crate::local_proxy::{self, ProxyHandle, ProxyStatus, UpstreamConfig};
use crate::models::{AppData, TestStatus};
use crate::ui::detail::DetailTab;
use std::sync::{Arc, Mutex};

/// Application-wide mutable state shared across UI panels.
pub struct AppState {
    pub data: AppData,
    pub rt: Arc<tokio::runtime::Runtime>,

    // Selection state
    pub selected_profile_id: Option<String>,
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
}

impl AppState {
    pub fn new(rt: Arc<tokio::runtime::Runtime>) -> Self {
        let data = crate::storage::load();
        let selected_profile_id = data.active_profile_id.clone();
        Self {
            data,
            rt,
            selected_profile_id,
            selected_proxy_id: None,
            detail_tab: DetailTab::Basic,
            show_password: false,
            proxy_handle: None,
            proxy_status: Arc::new(Mutex::new(ProxyStatus::default())),
            pending_test: None,
            needs_save: false,
            save_error: None,
            egui_ctx: None,
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
                    for profile in &mut self.data.profiles {
                        if let Some(proxy) = profile.proxies.iter_mut().find(|p| p.id == pid) {
                            proxy.test_status = status;
                            break;
                        }
                    }
                    self.pending_test = None;
                }
            }
        }
    }

    /// Start or restart the transparent proxy with the active proxy config.
    /// If no active proxy, stops the proxy.
    pub fn apply_proxy(&mut self) {
        // Stop existing proxy
        if let Some(handle) = self.proxy_handle.take() {
            handle.stop();
        }

        let proxy = self
            .data
            .active_profile()
            .and_then(|p| p.active_proxy())
            .cloned();

        let Some(proxy) = proxy else {
            let mut s = self.proxy_status.lock().unwrap();
            s.running = false;
            s.error = None;
            s.tun_addr.clear();
            return;
        };

        if proxy.host.is_empty() {
            let mut s = self.proxy_status.lock().unwrap();
            s.running = false;
            s.error = Some("Proxy host is empty".to_string());
            return;
        }

        let config = UpstreamConfig::from_proxy(&proxy);
        let ctx = self.egui_ctx.clone().unwrap_or_else(|| egui::Context::default());

        match local_proxy::start(&self.rt, config, self.proxy_status.clone(), ctx) {
            Ok(handle) => {
                self.proxy_handle = Some(handle);
            }
            Err(e) => {
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
        // Store egui context for repaint requests
        if self.state.egui_ctx.is_none() {
            self.state.egui_ctx = Some(ctx.clone());
        }

        // Poll async test results
        self.state.poll_test_result();

        // Top panel: title bar + proxy status
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PROXY MANAGER");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status = self.state.proxy_status.lock().unwrap();
                    if status.running {
                        ui.label(
                            egui::RichText::new(format!("TUN PROXY [ON] {}", status.tun_addr))
                                .color(crate::ui::COLOR_SUCCESS),
                        );
                        ui.label(
                            egui::RichText::new(format!("({} conns)", status.connections))
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("TUN PROXY [OFF]")
                                .color(crate::ui::COLOR_IDLE),
                        );
                    }
                    if let Some(err) = &status.error {
                        ui.colored_label(crate::ui::COLOR_FAILED, err.as_str());
                    }
                });
            });

            // Show save error if any
            if let Some(err) = &self.state.save_error {
                ui.colored_label(
                    crate::ui::COLOR_FAILED,
                    format!("Save error: {err}"),
                );
            }
        });

        // Main area: 3-pane layout rendered via nested panels
        egui::CentralPanel::default().show(ctx, |ui| {
            crate::ui::sidebar::render(ui, &mut self.state);
            crate::ui::proxy_list::render(ui, &mut self.state);
            crate::ui::detail::render(ui, &mut self.state);
        });

        // Auto-save when dirty
        if self.state.needs_save {
            self.state.do_save();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Stop the transparent proxy
        if let Some(handle) = self.state.proxy_handle.take() {
            handle.stop();
        }
        let _ = crate::storage::save(&self.state.data);
    }
}
