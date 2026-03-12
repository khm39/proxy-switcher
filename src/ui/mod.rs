pub mod detail;
pub mod proxy_list;
pub mod sidebar;

// ---------------------------------------------------------------------------
// Modern color palette (dark theme)
// ---------------------------------------------------------------------------

// Background layers
pub const BG_DARKEST: egui::Color32 = egui::Color32::from_rgb(15, 17, 23);      // #0f1117
pub const BG_DARK: egui::Color32 = egui::Color32::from_rgb(22, 25, 35);         // #161923
pub const BG_MID: egui::Color32 = egui::Color32::from_rgb(30, 34, 46);          // #1e222e
pub const BG_ELEVATED: egui::Color32 = egui::Color32::from_rgb(40, 45, 60);     // #282d3c

// Accent
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(99, 102, 241);        // #6366f1 indigo
pub const ACCENT_HOVER: egui::Color32 = egui::Color32::from_rgb(129, 131, 248); // #8183f8
pub const ACCENT_DIM: egui::Color32 = egui::Color32::from_rgb(67, 69, 170);     // #4345aa

// Text
pub const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(226, 232, 240);  // #e2e8f0
pub const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);// #94a3b8
pub const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(100, 116, 139);    // #64748b

// Status colors
pub const COLOR_IDLE: egui::Color32 = egui::Color32::from_rgb(100, 116, 139);   // #64748b
pub const COLOR_TESTING: egui::Color32 = egui::Color32::from_rgb(250, 204, 21); // #facc15
pub const COLOR_SUCCESS: egui::Color32 = egui::Color32::from_rgb(52, 211, 153); // #34d399
pub const COLOR_FAILED: egui::Color32 = egui::Color32::from_rgb(251, 113, 133); // #fb7185

// Badge colors
pub const BADGE_HTTP: egui::Color32 = egui::Color32::from_rgb(56, 189, 248);    // #38bdf8
pub const BADGE_SOCKS: egui::Color32 = egui::Color32::from_rgb(168, 85, 247);   // #a855f7

// Border / separator
pub const BORDER: egui::Color32 = egui::Color32::from_rgb(45, 50, 65);          // #2d3241

/// Apply modern dark theme to egui visuals.
pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    // Window / panel backgrounds
    visuals.panel_fill = BG_DARK;
    visuals.window_fill = BG_MID;
    visuals.extreme_bg_color = BG_DARKEST;
    visuals.faint_bg_color = BG_ELEVATED;

    // Widgets
    let rounding = egui::Rounding::same(6.0);
    let small_rounding = egui::Rounding::same(4.0);

    // Inactive widgets
    visuals.widgets.inactive.bg_fill = BG_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = BG_MID;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.inactive.rounding = rounding;

    // Hovered widgets
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(50, 55, 75);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(45, 50, 68);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = rounding;

    // Active widgets
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT_DIM;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT_HOVER);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.active.rounding = rounding;

    // Open widgets (combo boxes, etc.)
    visuals.widgets.open.bg_fill = BG_ELEVATED;
    visuals.widgets.open.weak_bg_fill = BG_MID;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.open.rounding = rounding;

    // Non-interactive
    visuals.widgets.noninteractive.bg_fill = BG_DARK;
    visuals.widgets.noninteractive.weak_bg_fill = BG_DARK;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = small_rounding;

    // Selection
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);

    // Misc
    visuals.window_rounding = egui::Rounding::same(8.0);
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0.0, 4.0].into(),
        blur: 12.0,
        spread: 0.0,
        color: egui::Color32::from_black_alpha(60),
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals.resize_corner_size = 8.0;
    visuals.striped = true;
    visuals.slider_trailing_fill = true;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

    ctx.set_visuals(visuals);

    // Spacing / style
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(12.0);
    style.spacing.indent = 16.0;
    ctx.set_style(style);
}

/// Return badge color for a proxy type string.
pub fn badge_color(proxy_type: &str) -> egui::Color32 {
    match proxy_type {
        "HTTP" | "HTTPS" => BADGE_HTTP,
        _ => BADGE_SOCKS,
    }
}
