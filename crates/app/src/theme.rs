//! 主题与视觉风格。
//!
//! 配色取自设计稿：米黄纸感背景 + 橄榄绿强调色 + 细边框卡片。

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

/// 页面背景（米黄纸感）。
pub const BG: Color32 = Color32::from_rgb(0xF2, 0xEE, 0xE3);
/// 卡片/面板背景。
pub const PANEL: Color32 = Color32::from_rgb(0xF7, 0xF4, 0xEC);
/// 输入框背景。
pub const FIELD: Color32 = Color32::from_rgb(0xFA, 0xF8, 0xF2);
/// 顶栏背景。
pub const TOPBAR: Color32 = Color32::from_rgb(0xEA, 0xE4, 0xD5);
/// 强调色（橄榄绿）。
pub const ACCENT: Color32 = Color32::from_rgb(0x7C, 0x8A, 0x5A);
/// 强调色悬停。
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x8C, 0x9B, 0x68);
/// 强调色的浅色底（选中标签）。
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xC8, 0xCF, 0xB0);
/// 主要文字。
pub const TEXT: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x32);
/// 次要文字。
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x7A, 0x76, 0x6A);
/// 边框线。
pub const BORDER: Color32 = Color32::from_rgb(0xD8, 0xD2, 0xC2);
/// 危险色（删除）。
pub const DANGER: Color32 = Color32::from_rgb(0xB0, 0x4A, 0x3C);

/// 警告色（自动保存中 / 未落盘提示）
pub const WARN: Color32 = Color32::from_rgb(0xC8, 0x8A, 0x2E);
/// 选中框（地图选区）。
pub const SELECT: Color32 = Color32::from_rgb(0x6E, 0x8B, 0x5E);

/// 应用主题。
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::light();

    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.selection.bg_fill = ACCENT_SOFT;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    let radius = CornerRadius::same(6);
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.corner_radius = radius;

    visuals.widgets.inactive.bg_fill = FIELD;
    visuals.widgets.inactive.weak_bg_fill = FIELD;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = radius;

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0xE8, 0xE3, 0xD4);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xE8, 0xE3, 0xD4);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.corner_radius = radius;

    visuals.widgets.active.bg_fill = ACCENT_SOFT;
    visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.active.corner_radius = radius;

    visuals.widgets.open.bg_fill = FIELD;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(12);
    ctx.set_style(style);
}

/// 装入支持中文的字体。
///
/// 优先从 Windows 字体目录加载（微软雅黑 / 黑体），失败则退回内置字体。
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // 候选字体（按优先级）
    let candidates = [
        ("C:/Windows/Fonts/msyh.ttc", 0u32),
        ("C:/Windows/Fonts/msyhl.ttc", 0u32),
        ("C:/Windows/Fonts/simhei.ttf", 0u32),
        ("C:/Windows/Fonts/simsun.ttc", 0u32),
    ];

    let mut loaded = false;
    for (path, index) in candidates {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut data = egui::FontData::from_owned(bytes);
        data.tweak.y_offset_factor = 0.0;
        data.index = index;
        fonts.font_data.insert("cjk".to_owned(), std::sync::Arc::new(data));

        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().insert(0, "cjk".to_owned());
        }
        loaded = true;
        break;
    }

    if !loaded {
        eprintln!("警告：未找到中文字体，界面可能显示为方块");
    }
    ctx.set_fonts(fonts);
}

/// 主按钮（强调色实心）。
pub fn accent_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(Color32::from_rgb(0xF7, 0xF4, 0xEC)))
        .fill(ACCENT)
}

/// 工具栏普通按钮。
pub fn tool_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(text.into()).fill(FIELD)
}

/// 危险操作按钮。
pub fn danger_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(Color32::WHITE)).fill(DANGER)
}

/// 小标题文字。
pub fn section_title(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(13.0).color(TEXT_DIM)
}

/// 数值标签。
pub fn value_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).size(14.0).color(TEXT)
}
