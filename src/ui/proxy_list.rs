use crate::app::AppState;
use crate::models::{Proxy, TestStatus};
use egui::{self, RichText, Ui};

const LIST_WIDTH: f32 = 300.0;

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::SidePanel::left("proxy_list")
        .exact_width(LIST_WIDTH)
        .frame(
            egui::Frame::none()
                .fill(super::BG_DARKEST)
                .inner_margin(egui::Margin::symmetric(12.0, 12.0))
                .stroke(egui::Stroke::new(1.0, super::BORDER)),
        )
        .show_inside(ui, |ui| {
            let proxy_count = state.data.proxies.len();

            // Header
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("PROXIES")
                        .size(12.0)
                        .strong()
                        .color(super::TEXT_MUTED),
                );
                ui.label(
                    RichText::new(format!("{proxy_count}"))
                        .size(12.0)
                        .color(super::TEXT_MUTED)
                        .background_color(super::BG_ELEVATED),
                );
            });
            ui.add_space(8.0);

            // Collect proxy info before mutable borrow
            let proxy_infos: Vec<(String, String, String, TestStatus, bool)> = state
                .data
                .proxies
                .iter()
                .map(|p| {
                    let is_active = state.data.active_proxy_id.as_ref() == Some(&p.id);
                    (
                        p.id.clone(),
                        p.proxy_type.to_string(),
                        p.name.clone(),
                        p.test_status.clone(),
                        is_active,
                    )
                })
                .collect();

            let mut to_delete: Option<String> = None;

            egui::ScrollArea::vertical()
                .max_height((ui.available_height() - 100.0).max(60.0))
                .show(ui, |ui| {
                    if proxy_infos.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("No proxies yet")
                                    .size(14.0)
                                    .color(super::TEXT_MUTED),
                            );
                        });
                    }

                    for (id, ptype, name, test_status, is_active) in &proxy_infos {
                        let is_selected = state.selected_proxy_id.as_ref() == Some(id);

                        let card_bg = if is_selected {
                            super::BG_ELEVATED
                        } else {
                            super::BG_MID
                        };

                        let card_stroke = if is_selected {
                            egui::Stroke::new(1.5, super::ACCENT)
                        } else {
                            egui::Stroke::new(1.0, super::BORDER)
                        };

                        let resp = egui::Frame::none()
                            .fill(card_bg)
                            .stroke(card_stroke)
                            .rounding(egui::Rounding::same(8.0))
                            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    // Type badge with padding
                                    super::type_badge(ui, ptype);

                                    // Proxy name
                                    let name_text = if *is_active {
                                        RichText::new(name)
                                            .size(14.0)
                                            .strong()
                                            .color(super::TEXT_PRIMARY)
                                    } else {
                                        RichText::new(name)
                                            .size(14.0)
                                            .color(super::TEXT_PRIMARY)
                                    };
                                    ui.label(name_text);

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            // Delete button
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("x")
                                                            .size(13.0)
                                                            .color(super::TEXT_MUTED),
                                                    )
                                                    .frame(false),
                                                )
                                                .clicked()
                                            {
                                                to_delete = Some(id.clone());
                                            }

                                            // Status dot
                                            let (color, tip) = match test_status {
                                                TestStatus::Idle => {
                                                    (super::COLOR_IDLE, "Untested".to_string())
                                                }
                                                TestStatus::Testing => {
                                                    (super::COLOR_TESTING, "Testing...".to_string())
                                                }
                                                TestStatus::Success(ms) => {
                                                    (super::COLOR_SUCCESS, format!("{ms}ms"))
                                                }
                                                TestStatus::Failed(msg) => {
                                                    (super::COLOR_FAILED, msg.clone())
                                                }
                                            };
                                            ui.label(
                                                RichText::new("●").size(12.0).color(color),
                                            )
                                            .on_hover_text(tip);

                                            if *is_active {
                                                ui.label(
                                                    RichText::new("ACTIVE")
                                                        .size(11.0)
                                                        .strong()
                                                        .color(super::COLOR_SUCCESS),
                                                );
                                            }
                                        },
                                    );
                                });
                            });

                        if resp.response.interact(egui::Sense::click()).clicked() {
                            state.selected_proxy_id = Some(id.clone());
                        }

                        ui.add_space(4.0);
                    }
                });

            // Handle deletion
            if let Some(del_id) = to_delete {
                let was_active = state.data.active_proxy_id.as_ref() == Some(&del_id);
                state.data.proxies.retain(|p| p.id != del_id);
                if was_active {
                    state.data.active_proxy_id = None;
                }
                if state.selected_proxy_id.as_ref() == Some(&del_id) {
                    state.selected_proxy_id = None;
                }
                state.needs_save = true;
                if was_active {
                    state.apply_proxy();
                }
            }

            ui.add_space(8.0);

            // Add proxy button
            let btn = ui.add_sized(
                [ui.available_width(), 30.0],
                egui::Button::new(RichText::new("+ Add Proxy").size(14.0).color(super::ACCENT)),
            );
            if btn.clicked() {
                let new_proxy = Proxy::default();
                let new_id = new_proxy.id.clone();
                state.data.proxies.push(new_proxy);
                state.selected_proxy_id = Some(new_id);
                state.needs_save = true;
            }

            // TUN settings at bottom
            ui.add_space(8.0);
            ui.scope(|ui| {
                ui.visuals_mut().widgets.noninteractive.bg_stroke =
                    egui::Stroke::new(1.0, super::BORDER);
                ui.separator();
            });
            ui.add_space(4.0);

            ui.label(
                RichText::new("TUN SETTINGS")
                    .size(12.0)
                    .strong()
                    .color(super::TEXT_MUTED),
            );
            ui.add_space(4.0);

            ui.label(
                RichText::new("TUN IP")
                    .size(13.0)
                    .color(super::TEXT_SECONDARY),
            );
            super::input_field_scope(ui, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.data.tun_addr)
                        .desired_width(ui.available_width())
                        .margin(egui::vec2(8.0, 4.0))
                        .font(egui::TextStyle::Monospace),
                );
                if resp.changed() {
                    state.needs_save = true;
                }
            });
            ui.label(
                RichText::new("Restart proxy to apply")
                    .size(12.0)
                    .color(super::TEXT_MUTED),
            );
        });
}
