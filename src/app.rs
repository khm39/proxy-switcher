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

    // System proxy state
    pub system_proxy_on: bool,
    pub system_proxy_message: Option<(bool, String)>, // (success, message)

    // Async test tracking
    pub pending_test: Option<(String, Arc<Mutex<TestStatus>>)>,

    // Dirty flag
    pub needs_save: bool,

    // Save error message
    pub save_error: Option<String>,
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
            system_proxy_on: crate::system_proxy::read_current().is_some(),
            system_proxy_message: None,
            pending_test: None,
            needs_save: false,
            save_error: None,
        }
    }

    /// Poll pending test result and apply it to the matching proxy.
    pub fn poll_test_result(&mut self) {
        if let Some((proxy_id, status_arc)) = &self.pending_test {
            let status = status_arc.lock().unwrap().clone();
            match &status {
                TestStatus::Testing => {} // still running
                _ => {
                    // Apply result to the proxy
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
        // Poll async test results
        self.state.poll_test_result();

        // Top panel: title bar + system proxy toggle
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PROXY MANAGER");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.state.system_proxy_on {
                        "SYSTEM PROXY [ON]"
                    } else {
                        "SYSTEM PROXY [OFF]"
                    };
                    if ui.button(label).clicked() {
                        if self.state.system_proxy_on {
                            // Turn OFF: clear system proxy
                            let result = crate::system_proxy::clear_proxy();
                            self.state.system_proxy_on = !result.success;
                            self.state.system_proxy_message =
                                Some((result.success, result.message));
                        } else {
                            // Turn ON: apply active proxy from active profile
                            if let Some(profile) = self.state.data.active_profile() {
                                if let Some(proxy) = profile.active_proxy() {
                                    let result =
                                        crate::system_proxy::apply_proxy(proxy);
                                    self.state.system_proxy_on = result.success;
                                    self.state.system_proxy_message =
                                        Some((result.success, result.message));
                                } else {
                                    self.state.system_proxy_message = Some((
                                        false,
                                        "No active proxy selected in the active profile"
                                            .to_string(),
                                    ));
                                }
                            } else {
                                self.state.system_proxy_message = Some((
                                    false,
                                    "No active profile selected".to_string(),
                                ));
                            }
                        }
                    }
                });
            });

            // Show system proxy status message
            if let Some((success, msg)) = &self.state.system_proxy_message {
                let color = if *success {
                    crate::ui::COLOR_SUCCESS
                } else {
                    crate::ui::COLOR_FAILED
                };
                ui.colored_label(color, msg.as_str());
            }

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
        let _ = crate::storage::save(&self.state.data);
        // Optionally clear system proxy on exit to avoid leaving stale settings
        // Uncomment the line below if you want auto-clear on exit:
        // let _ = crate::system_proxy::clear_proxy();
    }
}
