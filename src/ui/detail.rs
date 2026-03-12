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
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(super::BG_MID)
                .inner_margin(egui::Margin::symmetric(20.0, 16.0)),
        )
        .show_inside(ui, |ui| {
            let profile_id = state.selected_profile_id.clone();
            let proxy_id = state.selected_proxy_id.clone();

            let Some(proxy_id) = proxy_id else {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Select a proxy to view details")
                            .size(14.0)
                            .color(super::TEXT_MUTED),
                    );
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
                ui.label(
                    RichText::new("Proxy not found")
                        .size(13.0)
                        .color(super::TEXT_MUTED),
                );
                return;
            };

            // Header: type badge + name + status
            ui.horizontal(|ui| {
                let badge_color = super::badge_color(&proxy.proxy_type.to_string());
                ui.label(
                    RichText::new(proxy.proxy_type.to_string())
                        .size(11.0)
                        .strong()
                        .color(badge_color)
                        .background_color(badge_color.linear_multiply(0.15)),
                );
                ui.label(
                    RichText::new(&proxy.name)
                        .size(20.0)
                        .strong()
                        .color(super::TEXT_PRIMARY),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (color, text) = match &proxy.test_status {
                        TestStatus::Idle => (super::COLOR_IDLE, "Untested".to_string()),
                        TestStatus::Testing => (super::COLOR_TESTING, "Testing...".to_string()),
                        TestStatus::Success(ms) => (super::COLOR_SUCCESS, format!("{ms}ms")),
                        TestStatus::Failed(msg) => (super::COLOR_FAILED, msg.clone()),
                    };
                    ui.label(
                        RichText::new(format!("● {text}"))
                            .size(12.0)
                            .color(color),
                    );
                });
            });

            ui.add_space(12.0);

            // Tab bar
            ui.horizontal(|ui| {
                let tabs = [
                    (DetailTab::Basic, "Basic Settings"),
                    (DetailTab::PortFilter, "Port Filter"),
                    (DetailTab::Note, "Note"),
                ];

                for (tab, label) in tabs {
                    let is_active = state.detail_tab == tab;
                    let text = if is_active {
                        RichText::new(label)
                            .size(12.0)
                            .strong()
                            .color(super::ACCENT)
                    } else {
                        RichText::new(label)
                            .size(12.0)
                            .color(super::TEXT_SECONDARY)
                    };

                    if ui.selectable_label(is_active, text).clicked() {
                        state.detail_tab = tab;
                    }
                    ui.add_space(4.0);
                }
            });

            // Accent underline
            ui.scope(|ui| {
                ui.visuals_mut().widgets.noninteractive.bg_stroke =
                    egui::Stroke::new(1.0, super::BORDER);
                ui.separator();
            });

            ui.add_space(8.0);

            match state.detail_tab {
                DetailTab::Basic => {
                    render_basic_tab(ui, state, &proxy_id, &profile_id);
                }
                DetailTab::PortFilter => {
                    render_port_filter_tab(ui, state, &proxy_id, &profile_id);
                }
                DetailTab::Note => {
                    render_note_tab(ui, state, &proxy_id, &profile_id);
                }
            }
        });
}

fn render_basic_tab(
    ui: &mut Ui,
    state: &mut AppState,
    proxy_id: &str,
    profile_id: &Option<String>,
) {
    // Re-find proxy for this tab
    let proxy = profile_id.as_ref().and_then(|pid| {
        state
            .data
            .profiles
            .iter_mut()
            .find(|p| &p.id == pid)
            .and_then(|profile| profile.proxies.iter_mut().find(|p| p.id == proxy_id))
    });

    let Some(proxy) = proxy else {
        return;
    };

    // Form in a card
    egui::Frame::none()
        .fill(super::BG_DARK)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(16.0))
        .stroke(egui::Stroke::new(1.0, super::BORDER))
        .show(ui, |ui| {
            super::input_field_scope(ui, |ui| {
                egui::Grid::new("proxy_basic_grid")
                    .num_columns(2)
                    .spacing([16.0, 12.0])
                    .show(ui, |ui| {
                        // Name
                        ui.label(
                            RichText::new("Proxy Name")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut proxy.name)
                                .desired_width(250.0)
                                .margin(egui::vec2(8.0, 4.0)),
                        );
                        ui.end_row();

                        // Type
                        ui.label(
                            RichText::new("Type")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        egui::ComboBox::from_id_source("proxy_type_combo")
                            .selected_text(
                                RichText::new(proxy.proxy_type.to_string())
                                    .size(12.0)
                                    .color(super::TEXT_PRIMARY),
                            )
                            .width(250.0)
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
                        ui.label(
                            RichText::new("Host")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut proxy.host)
                                .desired_width(250.0)
                                .margin(egui::vec2(8.0, 4.0))
                                .hint_text("e.g. proxy.example.com"),
                        );
                        ui.end_row();

                        // Port
                        ui.label(
                            RichText::new("Port")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        ui.add(
                            egui::DragValue::new(&mut proxy.port)
                                .clamp_range(1..=65535)
                                .speed(1),
                        );
                        ui.end_row();

                        // Username
                        ui.label(
                            RichText::new("Username")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut proxy.username)
                                .desired_width(250.0)
                                .margin(egui::vec2(8.0, 4.0))
                                .hint_text("optional"),
                        );
                        ui.end_row();

                        // Password
                        ui.label(
                            RichText::new("Password")
                                .size(12.0)
                                .color(super::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            if state.show_password {
                                ui.add(
                                    egui::TextEdit::singleline(&mut proxy.password)
                                        .desired_width(200.0)
                                        .margin(egui::vec2(8.0, 4.0))
                                        .hint_text("optional"),
                                );
                            } else {
                                let mut masked = proxy.password.clone();
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut masked)
                                        .password(true)
                                        .desired_width(200.0)
                                        .margin(egui::vec2(8.0, 4.0)),
                                );
                                if resp.changed() {
                                    proxy.password = masked;
                                }
                            }
                            let toggle_text =
                                if state.show_password { "Hide" } else { "Show" };
                            if ui
                                .button(
                                    RichText::new(toggle_text)
                                        .size(11.0)
                                        .color(super::ACCENT),
                                )
                                .clicked()
                            {
                                state.show_password = !state.show_password;
                            }
                        });
                        ui.end_row();
                    });
            });
        });

    ui.add_space(16.0);

    // Action buttons
    let proxy_url = {
        // Re-find proxy to get url
        let proxy = profile_id.as_ref().and_then(|pid| {
            state
                .data
                .profiles
                .iter()
                .find(|p| &p.id == pid)
                .and_then(|profile| profile.proxies.iter().find(|p| p.id == proxy_id))
        });
        proxy.map(|p| p.url()).unwrap_or_default()
    };

    let testing = profile_id.as_ref().and_then(|pid| {
        state
            .data
            .profiles
            .iter()
            .find(|p| &p.id == pid)
            .and_then(|profile| profile.proxies.iter().find(|p| p.id == proxy_id))
            .map(|p| matches!(p.test_status, TestStatus::Testing))
    }).unwrap_or(false);

    ui.horizontal(|ui| {
        // Test connection button
        let test_btn = ui.add_enabled(
            !testing,
            egui::Button::new(
                RichText::new(if testing { "Testing..." } else { "Test Connection" })
                    .size(12.0),
            ),
        );
        if test_btn.clicked() {
            let status = Arc::new(Mutex::new(TestStatus::Testing));
            state.pending_test = Some((proxy_id.to_string(), status.clone()));
            crate::tester::run_test(&state.rt, proxy_url, status, ui.ctx().clone());
        }

        // Set as active button
        let active_btn = ui.add(egui::Button::new(
            RichText::new("Set as Active")
                .size(12.0)
                .color(super::ACCENT),
        ));
        if active_btn.clicked() {
            if let Some(profile) = profile_id.as_ref().and_then(|pid| {
                state.data.profiles.iter_mut().find(|p| &p.id == pid)
            }) {
                profile.active_proxy_id = Some(proxy_id.to_string());
                state.needs_save = true;
                state.apply_proxy();
            }
        }

        ui.add_space(8.0);

        // Save button
        let save_btn = ui.add(egui::Button::new(
            RichText::new("Save").size(12.0),
        ));
        if save_btn.clicked() {
            state.needs_save = true;
        }
    });
}

fn render_port_filter_tab(
    ui: &mut Ui,
    state: &mut AppState,
    proxy_id: &str,
    profile_id: &Option<String>,
) {
    let proxy = profile_id.as_ref().and_then(|pid| {
        state
            .data
            .profiles
            .iter_mut()
            .find(|p| &p.id == pid)
            .and_then(|profile| profile.proxies.iter_mut().find(|p| p.id == proxy_id))
    });
    let Some(proxy) = proxy else { return };

    egui::Frame::none()
        .fill(super::BG_DARK)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(16.0))
        .stroke(egui::Stroke::new(1.0, super::BORDER))
        .show(ui, |ui| {
            super::input_field_scope(ui, |ui| {
                let pf = &mut proxy.port_filter;

                ui.checkbox(
                    &mut pf.enabled,
                    RichText::new("Enable Port Filter")
                        .size(13.0)
                        .color(super::TEXT_PRIMARY),
                );
                ui.add_space(8.0);

                if pf.enabled {
                    ui.label(
                        RichText::new("Allowed ports (comma-separated)")
                            .size(12.0)
                            .color(super::TEXT_SECONDARY),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut pf.raw_input)
                            .desired_width(300.0)
                            .margin(egui::vec2(8.0, 4.0))
                            .hint_text("e.g. 80, 443, 8080"),
                    );
                    if resp.changed() {
                        pf.parse_raw_input();
                    }

                    ui.add_space(12.0);

                    ui.label(
                        RichText::new("Quick select")
                            .size(11.0)
                            .color(super::TEXT_MUTED),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let quick_ports = [
                            (80, "HTTP"),
                            (443, "HTTPS"),
                            (22, "SSH"),
                            (8080, "8080"),
                            (3128, "3128"),
                        ];
                        for (port, label) in quick_ports {
                            let active = pf.ports.contains(&port);
                            let text = if active {
                                RichText::new(label).size(11.0).color(super::ACCENT)
                            } else {
                                RichText::new(label)
                                    .size(11.0)
                                    .color(super::TEXT_SECONDARY)
                            };
                            if ui.selectable_label(active, text).clicked() {
                                pf.toggle_port(port);
                            }
                        }
                    });

                    ui.add_space(8.0);
                    if pf.ports.is_empty() {
                        ui.label(
                            RichText::new("All ports allowed (no filter)")
                                .size(12.0)
                                .color(super::TEXT_MUTED),
                        );
                    } else {
                        let ports_str: Vec<String> =
                            pf.ports.iter().map(|p| p.to_string()).collect();
                        ui.label(
                            RichText::new(format!("Allowed: {}", ports_str.join(", ")))
                                .size(12.0)
                                .color(super::COLOR_SUCCESS),
                        );
                    }
                } else {
                    ui.label(
                        RichText::new("Port filter disabled - all ports go through proxy")
                            .size(12.0)
                            .color(super::TEXT_MUTED),
                    );
                }
            });
        });

    ui.add_space(12.0);
    if ui
        .button(RichText::new("Save").size(12.0))
        .clicked()
    {
        state.needs_save = true;
    }
}

fn render_note_tab(
    ui: &mut Ui,
    state: &mut AppState,
    proxy_id: &str,
    profile_id: &Option<String>,
) {
    let proxy = profile_id.as_ref().and_then(|pid| {
        state
            .data
            .profiles
            .iter_mut()
            .find(|p| &p.id == pid)
            .and_then(|profile| profile.proxies.iter_mut().find(|p| p.id == proxy_id))
    });
    let Some(proxy) = proxy else { return };
    ui.label(
        RichText::new("Notes")
            .size(12.0)
            .color(super::TEXT_SECONDARY),
    );
    ui.add_space(4.0);

    egui::Frame::none()
        .fill(super::BG_DARK)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(egui::Stroke::new(1.0, super::BORDER))
        .show(ui, |ui| {
            super::input_field_scope(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut proxy.note)
                        .desired_rows(10)
                        .desired_width(f32::INFINITY)
                        .margin(egui::vec2(8.0, 6.0))
                        .hint_text("Add notes about this proxy..."),
                );
            });
        });

    ui.add_space(12.0);
    if ui
        .button(RichText::new("Save").size(12.0))
        .clicked()
    {
        state.needs_save = true;
    }
}
