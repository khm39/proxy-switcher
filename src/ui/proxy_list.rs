use crate::app::AppState;
use crate::models::{Proxy, TestStatus};
use egui::{self, RichText, Ui};

const LIST_WIDTH: f32 = 300.0;

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::SidePanel::left("proxy_list")
        .exact_width(LIST_WIDTH)
        .frame(
            egui::Frame::none()
                .fill(super::BG_DARK)
                .inner_margin(egui::Margin::symmetric(12.0, 12.0))
                .stroke(egui::Stroke::new(1.0, super::BORDER)),
        )
        .show_inside(ui, |ui| {
            let profile_id = state.selected_profile_id.clone();
            let profile = profile_id
                .as_ref()
                .and_then(|id| state.data.profiles.iter().find(|p| &p.id == id));

            let proxy_count = profile.map_or(0, |p| p.proxies.len());

            // Header
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("PROXIES")
                        .size(10.0)
                        .strong()
                        .color(super::TEXT_MUTED),
                );
                ui.label(
                    RichText::new(format!("{proxy_count}"))
                        .size(10.0)
                        .color(super::TEXT_MUTED)
                        .background_color(super::BG_ELEVATED),
                );
            });
            ui.add_space(8.0);

            // Collect proxy info before mutable borrow
            let proxy_infos: Vec<(String, String, String, TestStatus, bool)> =
                if let Some(profile) = profile {
                    profile
                        .proxies
                        .iter()
                        .map(|p| {
                            let is_active = profile.active_proxy_id.as_ref() == Some(&p.id);
                            (
                                p.id.clone(),
                                p.proxy_type.to_string(),
                                p.name.clone(),
                                p.test_status.clone(),
                                is_active,
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };

            let mut to_delete: Option<String> = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                if proxy_infos.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("No proxies yet")
                                .size(12.0)
                                .color(super::TEXT_MUTED),
                        );
                        ui.label(
                            RichText::new("Add one to get started")
                                .size(11.0)
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
                        egui::Stroke::new(1.0, super::ACCENT)
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
                                // Type badge
                                let badge_color = super::badge_color(ptype);
                                ui.label(
                                    RichText::new(ptype)
                                        .size(9.0)
                                        .strong()
                                        .color(badge_color)
                                        .background_color(badge_color.linear_multiply(0.15)),
                                );

                                // Proxy name
                                let name_text = if *is_active {
                                    RichText::new(name)
                                        .size(13.0)
                                        .strong()
                                        .color(super::TEXT_PRIMARY)
                                } else {
                                    RichText::new(name)
                                        .size(13.0)
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
                                                        .size(11.0)
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
                                        ui.label(RichText::new("●").size(10.0).color(color))
                                            .on_hover_text(tip);

                                        if *is_active {
                                            ui.label(
                                                RichText::new("ACTIVE")
                                                    .size(9.0)
                                                    .strong()
                                                    .color(super::COLOR_SUCCESS),
                                            );
                                        }
                                    },
                                );
                            });
                        });

                    // Click on the card to select
                    if resp.response.interact(egui::Sense::click()).clicked() {
                        state.selected_proxy_id = Some(id.clone());
                    }

                    ui.add_space(4.0);
                }
            });

            // Handle deletion
            if let Some(del_id) = to_delete {
                let mut was_active_proxy = false;
                if let Some(profile) = profile_id
                    .as_ref()
                    .and_then(|pid| state.data.profiles.iter_mut().find(|p| &p.id == pid))
                {
                    was_active_proxy = profile.active_proxy_id.as_ref() == Some(&del_id);
                    profile.proxies.retain(|p| p.id != del_id);
                    if was_active_proxy {
                        profile.active_proxy_id = None;
                    }
                    if state.selected_proxy_id.as_ref() == Some(&del_id) {
                        state.selected_proxy_id = None;
                    }
                    state.needs_save = true;
                }
                if was_active_proxy {
                    state.apply_proxy();
                }
            }

            ui.add_space(8.0);

            // Add proxy button
            let btn = ui.add_sized(
                [ui.available_width(), 32.0],
                egui::Button::new(RichText::new("+ Add Proxy").size(12.0).color(super::ACCENT)),
            );
            if btn.clicked() {
                if let Some(profile) = profile_id
                    .as_ref()
                    .and_then(|pid| state.data.profiles.iter_mut().find(|p| &p.id == pid))
                {
                    let new_proxy = Proxy::default();
                    let new_id = new_proxy.id.clone();
                    profile.proxies.push(new_proxy);
                    state.selected_proxy_id = Some(new_id);
                    state.needs_save = true;
                }
            }
        });
}
