use crate::app::AppState;
use egui::{self, RichText, Ui};

const SIDEBAR_WIDTH: f32 = 185.0;

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::SidePanel::left("sidebar")
        .exact_width(SIDEBAR_WIDTH)
        .show_inside(ui, |ui| {
            ui.vertical(|ui| {
                // Header
                ui.heading("PROFILES");
                ui.separator();

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

                    ui.horizontal(|ui| {
                        let label = if is_active {
                            RichText::new(format!("> {name}")).strong()
                        } else {
                            RichText::new(format!("  {name}"))
                        };

                        if ui
                            .selectable_label(is_selected, label)
                            .clicked()
                        {
                            state.selected_profile_id = Some(id.clone());
                            state.selected_proxy_id = None;
                        }
                    });

                    if is_active {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new("ACTIVE")
                                    .small()
                                    .color(super::COLOR_SUCCESS),
                            );
                        });
                    }
                }

                ui.separator();

                // Add / Remove profile buttons
                ui.horizontal(|ui| {
                    if ui.button("+ Add").clicked() {
                        let new_profile =
                            crate::models::Profile::new(format!("Profile {}", state.data.profiles.len() + 1));
                        let new_id = new_profile.id.clone();
                        state.data.profiles.push(new_profile);
                        state.selected_profile_id = Some(new_id);
                        state.selected_proxy_id = None;
                        state.needs_save = true;
                    }
                    if ui.button("Remove").clicked() {
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

                // Set active button
                if ui.button("Set Active").clicked() {
                    if let Some(sel_id) = state.selected_profile_id.clone() {
                        state.data.active_profile_id = Some(sel_id);
                        state.needs_save = true;
                        state.apply_proxy();
                    }
                }

                ui.separator();

                // Active proxy info
                ui.label(RichText::new("ACTIVE PROXY").strong().small());
                if let Some(profile) = state.data.active_profile() {
                    if let Some(proxy) = profile.active_proxy() {
                        ui.label(&proxy.name);
                        ui.label(
                            RichText::new(format!(":{}", proxy.port))
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    } else {
                        ui.label(
                            RichText::new("None")
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    }
                }
            });
        });
}
