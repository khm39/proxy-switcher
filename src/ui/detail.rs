use crate::app::AppState;
use crate::models::{ProxyType, TestStatus};
use egui::{self, RichText, Ui};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Basic,
    PortFilter,
    Note,
}

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::CentralPanel::default().show_inside(ui, |ui| {
        let profile_id = state.selected_profile_id.clone();
        let proxy_id = state.selected_proxy_id.clone();

        let Some(proxy_id) = proxy_id else {
            ui.centered_and_justified(|ui| {
                ui.label("Select a proxy to view details");
            });
            return;
        };

        // Find the proxy
        let proxy = profile_id.as_ref().and_then(|pid| {
            state
                .data
                .profiles
                .iter_mut()
                .find(|p| &p.id == pid)
                .and_then(|profile| profile.proxies.iter_mut().find(|p| p.id == proxy_id))
        });

        let Some(proxy) = proxy else {
            ui.label("Proxy not found");
            return;
        };

        // Header: type badge + name + status
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(proxy.proxy_type.to_string())
                    .strong()
                    .color(egui::Color32::LIGHT_BLUE),
            );
            ui.heading(&proxy.name);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, text) = match &proxy.test_status {
                    TestStatus::Idle => (super::COLOR_IDLE, "Untested".to_string()),
                    TestStatus::Testing => (super::COLOR_TESTING, "Testing...".to_string()),
                    TestStatus::Success(ms) => (super::COLOR_SUCCESS, format!("{ms}ms")),
                    TestStatus::Failed(msg) => (super::COLOR_FAILED, msg.clone()),
                };
                ui.label(RichText::new(format!("● {text}")).color(color));
            });
        });

        ui.separator();

        // Tab bar
        ui.horizontal(|ui| {
            if ui
                .selectable_label(state.detail_tab == DetailTab::Basic, "Basic Settings")
                .clicked()
            {
                state.detail_tab = DetailTab::Basic;
            }
            if ui
                .selectable_label(state.detail_tab == DetailTab::PortFilter, "Port Filter")
                .clicked()
            {
                state.detail_tab = DetailTab::PortFilter;
            }
            if ui
                .selectable_label(state.detail_tab == DetailTab::Note, "Note")
                .clicked()
            {
                state.detail_tab = DetailTab::Note;
            }
        });

        ui.separator();

        match state.detail_tab {
            DetailTab::Basic => {
                egui::Grid::new("proxy_basic_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        // Name
                        ui.label("Proxy Name");
                        ui.text_edit_singleline(&mut proxy.name);
                        ui.end_row();

                        // Type
                        ui.label("Type");
                        egui::ComboBox::from_id_source("proxy_type_combo")
                            .selected_text(proxy.proxy_type.to_string())
                            .show_ui(ui, |ui| {
                                for pt in ProxyType::ALL {
                                    ui.selectable_value(
                                        &mut proxy.proxy_type,
                                        pt,
                                        pt.to_string(),
                                    );
                                }
                            });
                        ui.end_row();

                        // Host
                        ui.label("Host");
                        let host_resp = ui.text_edit_singleline(&mut proxy.host);
                        if proxy.host.is_empty() && host_resp.lost_focus() {
                            // Visual hint: red frame handled by stroke in the future
                        }
                        ui.end_row();

                        // Port
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut proxy.port).clamp_range(1..=65535));
                        ui.end_row();

                        // Username
                        ui.label("Username");
                        ui.text_edit_singleline(&mut proxy.username);
                        ui.end_row();

                        // Password
                        ui.label("Password");
                        ui.horizontal(|ui| {
                            if state.show_password {
                                ui.text_edit_singleline(&mut proxy.password);
                            } else {
                                let mut masked = proxy.password.clone();
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut masked).password(true),
                                );
                                if resp.changed() {
                                    proxy.password = masked;
                                }
                            }
                            if ui
                                .button(if state.show_password { "Hide" } else { "Show" })
                                .clicked()
                            {
                                state.show_password = !state.show_password;
                            }
                        });
                        ui.end_row();
                    });

                ui.add_space(12.0);

                // Connection test button
                let proxy_url = proxy.url();
                let testing = matches!(proxy.test_status, TestStatus::Testing);

                ui.horizontal(|ui| {
                    let test_btn = ui.add_enabled(!testing, egui::Button::new("Test Connection"));
                    if test_btn.clicked() {
                        let status = Arc::new(Mutex::new(TestStatus::Testing));
                        state.pending_test = Some((proxy_id.clone(), status.clone()));
                        crate::tester::run_test(
                            &state.rt,
                            proxy_url,
                            status,
                            ui.ctx().clone(),
                        );
                    }

                    // Set as active proxy
                    if ui.button("Set as Active").clicked() {
                        if let Some(profile) = profile_id.as_ref().and_then(|pid| {
                            state.data.profiles.iter_mut().find(|p| &p.id == pid)
                        }) {
                            profile.active_proxy_id = Some(proxy_id.clone());
                            state.needs_save = true;
                        }
                    }
                });

                ui.add_space(8.0);

                // Save button
                if ui.button("Save").clicked() {
                    state.needs_save = true;
                }
            }

            DetailTab::PortFilter => {
                let pf = &mut proxy.port_filter;

                ui.checkbox(&mut pf.enabled, "Enable Port Filter");
                ui.add_space(8.0);

                if pf.enabled {
                    ui.label("Allowed ports (comma-separated):");
                    let resp = ui.text_edit_singleline(&mut pf.raw_input);
                    if resp.changed() {
                        pf.parse_raw_input();
                    }

                    ui.add_space(8.0);
                    ui.label("Quick select:");
                    ui.horizontal(|ui| {
                        let quick_ports = [(80, "HTTP"), (443, "HTTPS"), (22, "SSH"), (8080, "8080"), (3128, "3128")];
                        for (port, label) in quick_ports {
                            let active = pf.ports.contains(&port);
                            if ui.selectable_label(active, label).clicked() {
                                pf.toggle_port(port);
                            }
                        }
                    });

                    ui.add_space(8.0);
                    if pf.ports.is_empty() {
                        ui.label(
                            RichText::new("All ports allowed (no filter)")
                                .color(egui::Color32::GRAY),
                        );
                    } else {
                        let ports_str: Vec<String> =
                            pf.ports.iter().map(|p| p.to_string()).collect();
                        ui.label(format!("Allowed: {}", ports_str.join(", ")));
                    }
                } else {
                    ui.label(
                        RichText::new("Port filter disabled - all ports go through proxy")
                            .color(egui::Color32::GRAY),
                    );
                }

                ui.add_space(8.0);
                if ui.button("Save").clicked() {
                    state.needs_save = true;
                }
            }

            DetailTab::Note => {
                ui.label("Notes:");
                ui.add(
                    egui::TextEdit::multiline(&mut proxy.note)
                        .desired_rows(10)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                if ui.button("Save").clicked() {
                    state.needs_save = true;
                }
            }
        }
    });
}
