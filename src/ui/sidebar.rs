use crate::app::AppState;
use egui::{self, RichText, Ui};

const SIDEBAR_WIDTH: f32 = 200.0;

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::SidePanel::left("sidebar")
        .exact_width(SIDEBAR_WIDTH)
        .frame(
            egui::Frame::none()
                .fill(super::BG_DARKEST)
                .inner_margin(egui::Margin::symmetric(12.0, 12.0))
                .stroke(egui::Stroke::new(1.0, super::BORDER)),
        )
        .show_inside(ui, |ui| {
            ui.vertical(|ui| {
                // Section header
                ui.label(
                    RichText::new("PROFILES")
                        .size(10.0)
                        .strong()
                        .color(super::TEXT_MUTED),
                );
                ui.add_space(6.0);

                // Profile list
                let profile_ids: Vec<(String, String)> = state
                    .data
                    .profiles
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect();

                for (id, name) in &profile_ids {
                    let is_active = state.data.active_profile_id.as_ref() == Some(id);
                    let is_selected = state.selected_profile_id.as_ref() == Some(id);

                    let bg = if is_selected {
                        super::ACCENT_DIM
                    } else {
                        egui::Color32::TRANSPARENT
                    };

                    let text_color = if is_selected {
                        egui::Color32::WHITE
                    } else if is_active {
                        super::ACCENT_HOVER
                    } else {
                        super::TEXT_PRIMARY
                    };

                    egui::Frame::none()
                        .fill(bg)
                        .rounding(egui::Rounding::same(6.0))
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let label = RichText::new(name)
                                    .color(text_color)
                                    .size(13.0);
                                let label = if is_active { label.strong() } else { label };

                                if ui.selectable_label(false, label).clicked() {
                                    state.selected_profile_id = Some(id.clone());
                                    state.selected_proxy_id = None;
                                }

                                if is_active {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new("ACTIVE")
                                                    .size(9.0)
                                                    .strong()
                                                    .color(super::COLOR_SUCCESS),
                                            );
                                        },
                                    );
                                }
                            });
                        });
                }

                ui.add_space(8.0);

                // Add / Remove profile buttons (same row)
                ui.horizontal(|ui| {
                    let half_w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    if ui
                        .add_sized(
                            [half_w, 24.0],
                            egui::Button::new(RichText::new("+ Add").size(12.0)),
                        )
                        .clicked()
                    {
                        let new_profile = crate::models::Profile::new(format!(
                            "Profile {}",
                            state.data.profiles.len() + 1
                        ));
                        let new_id = new_profile.id.clone();
                        state.data.profiles.push(new_profile);
                        state.selected_profile_id = Some(new_id);
                        state.selected_proxy_id = None;
                        state.needs_save = true;
                    }
                    if ui
                        .add_sized(
                            [half_w, 24.0],
                            egui::Button::new(
                                RichText::new("Remove")
                                    .size(12.0)
                                    .color(super::COLOR_FAILED),
                            ),
                        )
                        .clicked()
                    {
                        if let Some(sel_id) = state.selected_profile_id.clone() {
                            if state.data.profiles.len() > 1 {
                                let was_active =
                                    state.data.active_profile_id.as_ref() == Some(&sel_id);
                                state.data.profiles.retain(|p| p.id != sel_id);
                                if was_active {
                                    state.data.active_profile_id =
                                        state.data.profiles.first().map(|p| p.id.clone());
                                }
                                state.selected_profile_id =
                                    state.data.profiles.first().map(|p| p.id.clone());
                                state.selected_proxy_id = None;
                                state.needs_save = true;
                                if was_active {
                                    state.apply_proxy();
                                }
                            }
                        }
                    }
                });

                // Set active button (separate row, full width)
                if ui
                    .add_sized(
                        [ui.available_width(), 24.0],
                        egui::Button::new(RichText::new("Set Active").size(12.0)),
                    )
                    .clicked()
                {
                    if let Some(sel_id) = state.selected_profile_id.clone() {
                        state.data.active_profile_id = Some(sel_id);
                        state.needs_save = true;
                        state.apply_proxy();
                    }
                }

                ui.add_space(12.0);

                // Separator
                ui.scope(|ui| {
                    ui.visuals_mut().widgets.noninteractive.bg_stroke =
                        egui::Stroke::new(1.0, super::BORDER);
                    ui.separator();
                });

                ui.add_space(8.0);

                // Active proxy info
                ui.label(
                    RichText::new("ACTIVE PROXY")
                        .size(10.0)
                        .strong()
                        .color(super::TEXT_MUTED),
                );
                ui.add_space(4.0);

                if let Some(profile) = state.data.active_profile() {
                    if let Some(proxy) = profile.active_proxy() {
                        let card_width = ui.available_width();
                        egui::Frame::none()
                            .fill(super::BG_ELEVATED)
                            .rounding(egui::Rounding::same(6.0))
                            .inner_margin(egui::Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.set_min_width(card_width - 16.0);
                                ui.label(
                                    RichText::new(&proxy.name)
                                        .size(13.0)
                                        .color(super::TEXT_PRIMARY),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{} :{}",
                                        proxy.proxy_type, proxy.port
                                    ))
                                    .size(11.0)
                                    .color(super::TEXT_MUTED),
                                );
                            });
                    } else {
                        ui.label(
                            RichText::new("None")
                                .size(12.0)
                                .color(super::TEXT_MUTED),
                        );
                    }
                }

                ui.add_space(12.0);

                ui.scope(|ui| {
                    ui.visuals_mut().widgets.noninteractive.bg_stroke =
                        egui::Stroke::new(1.0, super::BORDER);
                    ui.separator();
                });

                ui.add_space(8.0);

                // TUN settings
                ui.label(
                    RichText::new("TUN SETTINGS")
                        .size(10.0)
                        .strong()
                        .color(super::TEXT_MUTED),
                );
                ui.add_space(4.0);

                ui.label(
                    RichText::new("TUN IP")
                        .size(11.0)
                        .color(super::TEXT_SECONDARY),
                );
                super::input_field_scope(ui, |ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.data.tun_addr)
                            .desired_width(160.0)
                            .margin(egui::vec2(8.0, 4.0))
                            .font(egui::TextStyle::Monospace),
                    );
                    if resp.changed() {
                        state.needs_save = true;
                    }
                });
                ui.label(
                    RichText::new("Restart proxy to apply")
                        .size(10.0)
                        .color(super::TEXT_MUTED),
                );
            });
        });
}
