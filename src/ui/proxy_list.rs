use crate::app::AppState;
use crate::models::{Proxy, TestStatus};
use egui::{self, RichText, Ui};

const LIST_WIDTH: f32 = 285.0;

pub fn render(ui: &mut Ui, state: &mut AppState) {
    egui::SidePanel::left("proxy_list")
        .exact_width(LIST_WIDTH)
        .show_inside(ui, |ui| {
            let profile_id = state.selected_profile_id.clone();
            let profile = profile_id
                .as_ref()
                .and_then(|id| state.data.profiles.iter().find(|p| &p.id == id));

            let proxy_count = profile.map_or(0, |p| p.proxies.len());

            ui.heading(format!("PROXIES ({proxy_count})"));
            ui.separator();

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
                for (id, ptype, name, test_status, is_active) in &proxy_infos {
                    let is_selected = state.selected_proxy_id.as_ref() == Some(id);

                    ui.horizontal(|ui| {
                        // Type badge
                        ui.label(
                            RichText::new(ptype)
                                .small()
                                .strong()
                                .color(egui::Color32::LIGHT_BLUE),
                        );

                        // Proxy name (clickable)
                        let label_text = if *is_active {
                            RichText::new(name).strong()
                        } else {
                            RichText::new(name)
                        };
                        if ui.selectable_label(is_selected, label_text).clicked() {
                            state.selected_proxy_id = Some(id.clone());
                        }

                        // Status dot
                        let (color, tip) = match test_status {
                            TestStatus::Idle => (super::COLOR_IDLE, "Untested".to_string()),
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
                        let dot = RichText::new("●").color(color);
                        ui.label(dot).on_hover_text(tip);

                        // Delete button
                        if ui.small_button("x").clicked() {
                            to_delete = Some(id.clone());
                        }
                    });
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

            ui.separator();

            // Add proxy button
            if ui.button("+ Add Proxy").clicked() {
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
