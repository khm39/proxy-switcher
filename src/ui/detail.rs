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
            let proxy_id = state.selected_proxy_id.clone();

            let Some(proxy_id) = proxy_id else {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Select a proxy to view details")
                            .size(16.0)
                            .color(super::TEXT_MUTED),
                    );
                });
                return;
            };

            let proxy = state.data.proxies.iter_mut().find(|p| p.id == proxy_id);

            let Some(proxy) = proxy else {
                ui.label(
                    RichText::new("Proxy not found")
                        .size(14.0)
                        .color(super::TEXT_MUTED),
                );
                return;
            };

            // Header: type badge + name + status
            ui.horizontal(|ui| {
                super::type_badge(ui, &proxy.proxy_type.to_string());
                ui.label(
                    RichText::new(&proxy.name)
                        .size(22.0)
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
                            .size(13.0)
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
                    let (bg, stroke, text_color) = if is_active {
                        (super::BG_ELEVATED, egui::Stroke::new(1.5, super::ACCENT), super::ACCENT)
                    } else {
                        (super::BG_DARK, egui::Stroke::new(1.0, super::BORDER), super::TEXT_SECONDARY)
                    };

                    let resp = egui::Frame::none()
                        .fill(bg)
                        .stroke(stroke)
                        .rounding(egui::Rounding::same(6.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                        .show(ui, |ui| {
                            let text = if is_active {
                                RichText::new(label).size(13.0).strong().color(text_color)
                            } else {
                                RichText::new(label).size(13.0).color(text_color)
                            };
                            ui.label(text);
                        });
                    if resp.response.interact(egui::Sense::click()).clicked() {
                        state.detail_tab = tab;
                    }
                }
            });

            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match state.detail_tab {
                        DetailTab::Basic => {
                            render_basic_tab(ui, state, &proxy_id);
                        }
                        DetailTab::PortFilter => {
                            render_port_filter_tab(ui, state, &proxy_id);
                        }
                        DetailTab::Note => {
                            render_note_tab(ui, state, &proxy_id);
                        }
                    }
                });
        });
}

/// Render a field label with a red asterisk indicating it is required.
fn required_label(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.label(
            RichText::new(text)
                .size(13.0)
                .color(super::TEXT_SECONDARY),
        );
        ui.label(
            RichText::new("*")
                .size(13.0)
                .color(super::COLOR_FAILED),
        );
    });
}

fn find_proxy_mut<'a>(state: &'a mut AppState, proxy_id: &str) -> Option<&'a mut crate::models::Proxy> {
    state.data.proxies.iter_mut().find(|p| p.id == proxy_id)
}

fn render_basic_tab(ui: &mut Ui, state: &mut AppState, proxy_id: &str) {
    let mut show_password = state.show_password;

    let Some(proxy) = find_proxy_mut(state, proxy_id) else {
        return;
    };

    egui::Frame::none()
        .fill(super::BG_DARK)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(16.0))
        .stroke(egui::Stroke::new(1.0, super::BORDER))
        .show(ui, |ui| {
            super::input_field_scope(ui, |ui| {
                let field_width = (ui.available_width() - 4.0).max(100.0);

                // Proxy Name *
                required_label(ui, "Proxy Name");
                ui.add(
                    egui::TextEdit::singleline(&mut proxy.name)
                        .desired_width(field_width)
                        .margin(egui::vec2(8.0, 4.0))
                        .hint_text("e.g. My Proxy"),
                );
                ui.add_space(8.0);

                // Type *
                required_label(ui, "Type");
                egui::ComboBox::from_id_source("proxy_type_combo")
                    .selected_text(
                        RichText::new(proxy.proxy_type.to_string())
                            .size(13.0)
                            .color(super::TEXT_PRIMARY),
                    )
                    .width(field_width)
                    .show_ui(ui, |ui| {
                        for pt in ProxyType::ALL {
                            ui.selectable_value(
                                &mut proxy.proxy_type,
                                pt,
                                pt.to_string(),
                            );
                        }
                    });
                ui.add_space(8.0);

                // Host *
                required_label(ui, "Host");
                ui.add(
                    egui::TextEdit::singleline(&mut proxy.host)
                        .desired_width(field_width)
                        .margin(egui::vec2(8.0, 4.0))
                        .hint_text("e.g. proxy.example.com"),
                );
                ui.add_space(8.0);

                // Port *
                required_label(ui, "Port");
                {
                    let mut port_str = proxy.port.to_string();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut port_str)
                            .desired_width(field_width)
                            .margin(egui::vec2(8.0, 4.0))
                            .hint_text("e.g. 8080"),
                    );
                    if resp.changed() {
                        if let Ok(p) = port_str.parse::<u16>() {
                            if p >= 1 {
                                proxy.port = p;
                            }
                        }
                    }
                }
                ui.add_space(8.0);

                // Username (optional)
                ui.label(
                    RichText::new("Username")
                        .size(13.0)
                        .color(super::TEXT_SECONDARY),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut proxy.username)
                        .desired_width(field_width)
                        .margin(egui::vec2(8.0, 4.0))
                        .hint_text("e.g. user01"),
                );
                ui.add_space(8.0);

                // Password (optional)
                ui.label(
                    RichText::new("Password")
                        .size(13.0)
                        .color(super::TEXT_SECONDARY),
                );
                ui.horizontal(|ui| {
                    let pw_width = ui.available_width() - 60.0;
                    if show_password {
                        ui.add(
                            egui::TextEdit::singleline(&mut proxy.password)
                                .desired_width(pw_width)
                                .margin(egui::vec2(8.0, 4.0))
                                .hint_text("e.g. password123"),
                        );
                    } else {
                        let mut masked = proxy.password.clone();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut masked)
                                .password(true)
                                .desired_width(pw_width)
                                .margin(egui::vec2(8.0, 4.0))
                                .hint_text("e.g. password123"),
                        );
                        if resp.changed() {
                            proxy.password = masked;
                        }
                    }
                    let toggle_text =
                        if show_password { "Hide" } else { "Show" };
                    if ui
                        .button(
                            RichText::new(toggle_text)
                                .size(13.0)
                                .color(super::ACCENT),
                        )
                        .clicked()
                    {
                        show_password = !show_password;
                    }
                });
            });
        });

    state.show_password = show_password;

    ui.add_space(12.0);

    // Action buttons
    let proxy_url = state
        .data
        .proxies
        .iter()
        .find(|p| p.id == proxy_id)
        .map(|p| p.url())
        .unwrap_or_default();

    let testing = state
        .data
        .proxies
        .iter()
        .find(|p| p.id == proxy_id)
        .map(|p| matches!(p.test_status, TestStatus::Testing))
        .unwrap_or(false);

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        if testing {
            let dots = animated_dots(ui);
            let label = format!("Testing{dots}");
            ui.add_enabled(false, egui::Button::new(
                RichText::new(label)
                    .size(13.0)
                    .color(super::COLOR_TESTING),
            ));
            ui.ctx().request_repaint();
        } else if ui.button(RichText::new("Test Connection").size(13.0)).clicked() {
            let status = Arc::new(Mutex::new(TestStatus::Testing));
            state.pending_test = Some((proxy_id.to_string(), status.clone()));
            crate::tester::run_test(&state.rt, proxy_url, status, ui.ctx().clone());
        }

        if ui.button(
            RichText::new("Set as Active")
                .size(13.0)
                .color(super::ACCENT),
        ).clicked() {
            state.data.active_proxy_id = Some(proxy_id.to_string());
            state.needs_save = true;
            state.apply_proxy();
        }

        if ui.button(RichText::new("Save").size(13.0)).clicked() {
            state.needs_save = true;
        }

        if ui.button(
            RichText::new("Delete")
                .size(13.0)
                .color(super::COLOR_FAILED),
        ).clicked() {
            let was_active = state.data.active_proxy_id.as_ref() == Some(&proxy_id.to_string());
            state.data.proxies.retain(|p| p.id != proxy_id);
            if was_active {
                state.data.active_proxy_id = None;
            }
            if state.selected_proxy_id.as_ref().map(|s| s.as_str()) == Some(proxy_id) {
                state.selected_proxy_id = None;
            }
            state.needs_save = true;
            if was_active {
                state.apply_proxy();
            }
        }
    });
}

fn render_port_filter_tab(ui: &mut Ui, state: &mut AppState, proxy_id: &str) {
    let Some(proxy) = find_proxy_mut(state, proxy_id) else {
        return;
    };

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
                        .size(14.0)
                        .color(super::TEXT_PRIMARY),
                );
                ui.add_space(8.0);

                if pf.enabled {
                    ui.label(
                        RichText::new("Allowed ports (comma-separated)")
                            .size(13.0)
                            .color(super::TEXT_SECONDARY),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut pf.raw_input)
                            .desired_width(ui.available_width())
                            .margin(egui::vec2(8.0, 4.0))
                            .hint_text("e.g. 80, 443, 8080"),
                    );
                    if resp.changed() {
                        pf.parse_raw_input();
                    }

                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Quick select")
                            .size(13.0)
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
                                RichText::new(label).size(13.0).color(super::ACCENT)
                            } else {
                                RichText::new(label)
                                    .size(13.0)
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
                                .size(13.0)
                                .color(super::TEXT_MUTED),
                        );
                    } else {
                        let ports_str: Vec<String> =
                            pf.ports.iter().map(|p| p.to_string()).collect();
                        ui.label(
                            RichText::new(format!("Allowed: {}", ports_str.join(", ")))
                                .size(13.0)
                                .color(super::COLOR_SUCCESS),
                        );
                    }
                } else {
                    ui.label(
                        RichText::new("Port filter disabled - all ports go through proxy")
                            .size(13.0)
                            .color(super::TEXT_MUTED),
                    );
                }
            });
        });

    ui.add_space(12.0);
    if ui.button(RichText::new("Save").size(13.0)).clicked() {
        state.needs_save = true;
    }
}

fn render_note_tab(ui: &mut Ui, state: &mut AppState, proxy_id: &str) {
    let Some(proxy) = find_proxy_mut(state, proxy_id) else {
        return;
    };

    ui.label(
        RichText::new("Notes")
            .size(13.0)
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
    if ui.button(RichText::new("Save").size(13.0)).clicked() {
        state.needs_save = true;
    }
}

// ---------------------------------------------------------------------------
// Animated testing indicator helpers
// ---------------------------------------------------------------------------

fn animated_dots(ui: &Ui) -> &'static str {
    let phase = (ui.input(|i| i.time) * 2.5) as usize % 4;
    match phase {
        0 => "",
        1 => ".",
        2 => "..",
        _ => "...",
    }
}

