pub mod detail;
pub mod proxy_list;
pub mod sidebar;

// Color constants for status indicators
pub const COLOR_IDLE: egui::Color32 = egui::Color32::from_rgb(71, 85, 105);    // #475569
pub const COLOR_TESTING: egui::Color32 = egui::Color32::from_rgb(251, 191, 36); // #fbbf24
pub const COLOR_SUCCESS: egui::Color32 = egui::Color32::from_rgb(74, 222, 128); // #4ade80
pub const COLOR_FAILED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113); // #f87171
