//! 地图视图：区块俯视图渲染、平移缩放、矩形选区。
//!
//! 渲染策略：
//! - 每个已加载区块生成一张 16×16 的缩略图纹理（缓存，按需生成）
//! - 视口内只绘制可见区块
//! - 未生成/未采样的区块画成深色棋盘格，和已生成地形区分开

use std::collections::HashMap;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use mcworld_core::chunk::Column;
use mcworld_core::world::ChunkSurface;

use crate::theme;

/// 一张区块缩略图纹理。
pub struct ChunkTile {
    pub texture: TextureHandle,
    /// 保留 256 列采样结果，供鼠标悬停查询方块与高度。
    pub columns: Vec<Column>,
}

/// 区块缩略图缓存。
#[derive(Default)]
pub struct TileCache {
    tiles: HashMap<(i32, i32), ChunkTile>,
    /// 已确认"无数据"的区块，避免每帧重复请求。
    missing: std::collections::HashSet<(i32, i32)>,
}

impl TileCache {
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn contains(&self, key: (i32, i32)) -> bool {
        self.tiles.contains_key(&key) || self.missing.contains(&key)
    }

    pub fn mark_missing(&mut self, key: (i32, i32)) {
        self.missing.insert(key);
    }

    /// 用采样数据生成纹理。
    pub fn insert(&mut self, ctx: &egui::Context, surface: &ChunkSurface) {
        let mut img = ColorImage::filled([16, 16], Color32::TRANSPARENT);
        for (i, col) in surface.columns.iter().enumerate().take(256) {
            let x = i % 16;
            let z = i / 16;
            img.pixels[z * 16 + x] = column_color(
                col,
                surface.x * 16 + x as i32,
                surface.z * 16 + z as i32,
            );
        }
        let tex = ctx.load_texture(
            format!("chunk-{}-{}", surface.x, surface.z),
            img,
            TextureOptions::NEAREST,
        );
        self.tiles.insert(
            (surface.x, surface.z),
            ChunkTile {
                texture: tex,
                columns: surface.columns.clone(),
            },
        );
    }

    pub fn get(&self, key: (i32, i32)) -> Option<&ChunkTile> {
        self.tiles.get(&key)
    }

    /// 取某块某列采样到的方块名与高度（用于鼠标悬停提示）。
    pub fn surface_block(
        &self,
        cx: i32,
        cz: i32,
        lx: usize,
        lz: usize,
    ) -> Option<(String, i32)> {
        let t = self.tiles.get(&(cx, cz))?;
        let col = t.columns.get(lz * 16 + lx)?;
        if col.y < 0 {
            return None;
        }
        Some((col.block.clone(), col.y))
    }

    /// 取某块某列采样到的高度。
    pub fn surface_y(&self, cx: i32, cz: i32, lx: usize, lz: usize) -> Option<i32> {
        let t = self.tiles.get(&(cx, cz))?;
        let col = t.columns.get(lz * 16 + lx)?;
        if col.y < 0 {
            None
        } else {
            Some(col.y)
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.missing.clear();
    }
}

/// 把采样列转成像素颜色。
///
/// `wx` / `wz` 是该列的世界坐标，用来做**确定性的轻微明暗扰动**：
/// 同一方块在不同位置会有 ±6 的亮度差，大面积同色地形（比如成片的石头、
/// 沙子、雪）才不会糊成一整块死色。扰动只依赖坐标，所以同一位置每次都一样，
/// 不会闪烁。
fn column_color(c: &Column, wx: i32, wz: i32) -> Color32 {
    if c.y < 0 {
        // 无地形（未生成 / 该列全空气）：用带一点蓝的深灰，
        // 而不是纯黑——纯黑在地图上像"洞"，容易误以为渲染坏了。
        return Color32::from_rgb(0x2A, 0x2E, 0x38);
    }

    // 坐标哈希 → [-6, +6] 的亮度偏移
    let h = (wx.wrapping_mul(0x27d4_eb2d) ^ wz.wrapping_mul(0x1656_67b1)) as u32;
    let jitter = ((h >> 13) & 0x0F) as i32 - 8; // -8..+7
    let jitter = jitter / 2; // -4..+3，够看出纹理又不脏

    let f = |v: u8| -> u8 { (v as i32 + jitter).clamp(0, 255) as u8 };
    Color32::from_rgb(f(c.color[0]), f(c.color[1]), f(c.color[2]))
}

/// 地图的相机状态。
#[derive(Clone, Debug)]
pub struct MapCamera {
    /// 视图中心（方块坐标）。
    pub center: Vec2,
    /// 每方块像素数。
    pub scale: f32,
}

impl Default for MapCamera {
    fn default() -> Self {
        MapCamera { center: Vec2::ZERO, scale: 1.0 }
    }
}

impl MapCamera {
    pub const MIN_SCALE: f32 = 0.02;
    pub const MAX_SCALE: f32 = 24.0;

    pub fn zoom_by(&mut self, factor: f32, anchor: Pos2, viewport: Rect) {
        let before = self.screen_to_world(anchor, viewport);
        self.scale = (self.scale * factor).clamp(Self::MIN_SCALE, Self::MAX_SCALE);
        let after = self.screen_to_world(anchor, viewport);
        // 保持鼠标下的世界坐标不动
        self.center += before - after;
    }

    pub fn screen_to_world(&self, p: Pos2, viewport: Rect) -> Vec2 {
        let rel = p - viewport.center();
        Vec2::new(rel.x / self.scale, rel.y / self.scale) + self.center
    }

    pub fn world_to_screen(&self, w: Vec2, viewport: Rect) -> Pos2 {
        let rel = (w - self.center) * self.scale;
        viewport.center() + rel
    }

    /// 当前视口覆盖的世界矩形（方块坐标）。
    pub fn visible_world_rect(&self, viewport: Rect) -> Rect {
        let half = Vec2::new(
            viewport.width() / 2.0 / self.scale,
            viewport.height() / 2.0 / self.scale,
        );
        Rect::from_center_size(
            Pos2::new(self.center.x, self.center.y),
            egui::vec2(half.x * 2.0, half.y * 2.0),
        )
    }
}

/// 地图交互结果。
#[derive(Clone, Debug, Default)]
pub struct MapResponse {
    /// 本帧点击的世界坐标（方块）。
    pub clicked_world: Option<(f64, f64)>,
    /// 鼠标当前所在的世界坐标（方块），用于显示坐标/区块/群系。
    pub hover_world: Option<(f64, f64)>,
    /// 本帧是否正在拖拽调整选区。
    pub selecting: bool,
    /// 本帧是否**刚刚结束**一次框选拖拽（用于多选模式下确认选区）。
    pub selecting_ended: bool,
    /// 本帧单击用的哪个键（用于「点击取消选区」的判定）。
    pub clicked_button: Option<egui::PointerButton>,
    /// 粘贴模式下光标所指的左上角区块坐标。
    pub paste_anchor: Option<(i32, i32)>,
    /// 粘贴模式下用户左键确认的位置（区块坐标）。
    pub paste_confirm: Option<(i32, i32)>,
    /// 粘贴模式下用户右键取消。
    pub paste_cancelled: bool,
}

/// 地块选区（区块坐标，闭区间）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkSelection {
    pub x0: i32,
    pub z0: i32,
    pub x1: i32,
    pub z1: i32,
}

impl ChunkSelection {
    pub fn normalized(&self) -> (i32, i32, i32, i32) {
        (
            self.x0.min(self.x1),
            self.z0.min(self.z1),
            self.x0.max(self.x1),
            self.z0.max(self.z1),
        )
    }

    pub fn chunk_count(&self) -> usize {
        let (a, b, c, d) = self.normalized();
        ((c - a + 1) as usize) * ((d - b + 1) as usize)
    }

    /// 以方块坐标表示的边界。
    pub fn block_bounds(&self) -> (f64, f64, f64, f64) {
        let (a, b, c, d) = self.normalized();
        (
            a as f64 * 16.0,
            b as f64 * 16.0,
            (c as f64 + 1.0) * 16.0,
            (d as f64 + 1.0) * 16.0,
        )
    }
}

/// 地图绘制选项。
#[derive(Clone, Debug, Default)]
pub struct MapOptions {
    /// 是否强制显示区块网格（不勾选时仅在放大到一定程度才显示）。
    pub show_grid: bool,
    /// 粘贴预览框的尺寸（区块数，宽 × 高）。Some 时进入粘贴定位模式。
    pub paste_size: Option<(i32, i32)>,
    /// 多选模式下**已确认**的选区（会画成半透明占位块 + 序号）。
    pub confirmed: Vec<ChunkSelection>,
}

/// 绘制地图。返回交互信息。
pub fn draw(
    ui: &mut egui::Ui,
    cache: &TileCache,
    cam: &mut MapCamera,
    selection: &mut Option<ChunkSelection>,
    player_pos: Option<[f64; 3]>,
    spawn_pos: Option<(i32, i32, i32)>,
    opts: MapOptions,
) -> MapResponse {
    let (outer, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let painter = ui.painter_at(outer);

    // 画面区域由缩放决定：1 个区块 = 16 方块 * scale 像素。
    // 地图内容始终是正方形网格，这里把绘制原点对齐到区块边界，
    // 避免出现半截区块或像素抖动。
    painter.rect_filled(outer, 0.0, Color32::from_rgb(0x1A, 0x1C, 0x20));
    let rect = outer;

    // ---- 平移 ----
    // 右键拖拽是主要的拖动手势；中键与 Shift+左键作为备用。
    if response.dragged_by(egui::PointerButton::Secondary)
        || response.dragged_by(egui::PointerButton::Middle)
        || (response.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.modifiers.shift))
    {
        let d = response.drag_delta();
        cam.center -= Vec2::new(d.x, d.y) / cam.scale;
    }

    // ---- 缩放 ----
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let factor = if scroll > 0.0 { 1.15 } else { 1.0 / 1.15 };
            let anchor = response.hover_pos().unwrap_or(rect.center());
            cam.zoom_by(factor, anchor, rect);
        }
        // 以光标为中心缩放后，滚轮也用于平移（Shift+滚轮横向）
        if ui.input(|i| i.modifiers.ctrl) {
            // 保留 Ctrl 作为精调
        }
    }

    // ---- 绘制区块 ----
    let visible = cam.visible_world_rect(rect);
    let cx0 = (visible.min.x / 16.0).floor() as i32 - 1;
    let cx1 = (visible.max.x / 16.0).ceil() as i32 + 1;
    let cz0 = (visible.min.y / 16.0).floor() as i32 - 1;
    let cz1 = (visible.max.y / 16.0).ceil() as i32 + 1;

    let chunk_px = 16.0 * cam.scale;
    let mut drawn = 0usize;

    for cx in cx0..=cx1 {
        for cz in cz0..=cz1 {
            let world_min = Vec2::new(cx as f32 * 16.0, cz as f32 * 16.0);
            let p0 = cam.world_to_screen(world_min, rect);
            let cell = Rect::from_min_size(p0, Vec2::splat(chunk_px));

            match cache.get((cx, cz)) {
                Some(tile) => {
                    painter.image(
                        tile.texture.id(),
                        cell,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    drawn += 1;
                }
                None => {
                    // 未采样：按是否有数据画不同底色（由缓存标记）
                    if !cache.contains((cx, cz)) {
                        // 尚未采样，画占位
                        painter.rect_filled(cell, 0.0, Color32::from_rgb(0x26, 0x29, 0x2E));
                    } else {
                        painter.rect_filled(cell, 0.0, Color32::from_rgb(0x1E, 0x20, 0x24));
                    }
                }
            }
        }
    }

    // 区块网格：勾选后始终显示；未勾选时只在放大到一定程度才显示（避免缩略图太乱）
    if opts.show_grid || cam.scale > 3.0 {
        // 勾选时用对比更强的半透明白线，否则用淡黑线做背景参考
        let color = if opts.show_grid {
            Color32::from_rgba_unmultiplied(255, 255, 255, 110)
        } else {
            Color32::from_black_alpha(45)
        };
        let grid = Stroke::new(1.0, color);
        for cx in cx0..=cx1 {
            let x = cam.world_to_screen(Vec2::new(cx as f32 * 16.0, 0.0), rect).x;
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], grid);
        }
        for cz in cz0..=cz1 {
            let y = cam.world_to_screen(Vec2::new(0.0, cz as f32 * 16.0), rect).y;
            painter.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)], grid);
        }
    }

    // ---- 选区交互 ----
    let mut out = MapResponse::default();

    // 悬停坐标（供状态栏显示方块/区块/群系）
    if let Some(p) = response.hover_pos() {
        let w = cam.screen_to_world(p, rect);
        out.hover_world = Some((w.x as f64, w.y as f64));
    }

    // 右键用于平移，不参与选区
    let panning = response.dragged_by(egui::PointerButton::Secondary);

    // ---- 粘贴定位模式 ----
    // 鼠标移动时在光标处显示红色预览框；左键确认位置；右键取消。
    let mut paste_confirm: Option<(i32, i32)> = None;
    if let Some((pw, ph)) = opts.paste_size {
        if let Some(p) = response.hover_pos() {
            let w = cam.screen_to_world(p, rect);
            // 以光标所指区块为左上角
            let (ax, az) = ((w.x / 16.0).floor() as i32, (w.y / 16.0).floor() as i32);
            out.paste_anchor = Some((ax, az));

            // 红色预览框
            let r = Rect::from_two_pos(
                cam.world_to_screen(Vec2::new(ax as f32 * 16.0, az as f32 * 16.0), rect),
                cam.world_to_screen(
                    Vec2::new((ax + pw) as f32 * 16.0, (az + ph) as f32 * 16.0),
                    rect,
                ),
            );
            painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(200, 60, 60, 46));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(2.0, Color32::from_rgb(0xE0, 0x3A, 0x3A)),
                egui::StrokeKind::Middle,
            );

            let label = format!("粘贴 {pw}×{ph} 区块 → ({ax}, {az})  左键确认 / 右键取消");
            let galley = painter.layout_no_wrap(
                label,
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            let anchor = Pos2::new(r.left_top().x, r.left_top().y - 22.0);
            let bg = Rect::from_min_size(anchor, galley.size() + Vec2::new(10.0, 6.0));
            painter.rect_filled(bg, 4.0, Color32::from_rgb(0xC8, 0x30, 0x30));
            painter.galley(anchor + Vec2::new(5.0, 3.0), galley, Color32::WHITE);

            if response.clicked_by(egui::PointerButton::Primary) {
                paste_confirm = Some((ax, az));
            }
        }
        out.paste_confirm = paste_confirm;
        out.paste_cancelled = response.clicked_by(egui::PointerButton::Secondary);
        // 粘贴模式下不处理选区
        out.selecting = false;
        if let Some(sel) = selection {
            let _ = sel;
        }
        return out;
    }

    if response.drag_started_by(egui::PointerButton::Primary)
        && !ui.input(|i| i.modifiers.shift)
        && !ui.input(|i| i.modifiers.alt)
        && !panning
    {
        if let Some(p) = response.interact_pointer_pos() {
            let w = cam.screen_to_world(p, rect);
            let (cx, cz) = (
                (w.x / 16.0).floor() as i32,
                (w.y / 16.0).floor() as i32,
            );
            *selection = Some(ChunkSelection { x0: cx, z0: cz, x1: cx, z1: cz });
            out.selecting = true;
        }
    }

    if response.dragged_by(egui::PointerButton::Primary)
        && !ui.input(|i| i.modifiers.shift)
        && !panning
    {
        if let (Some(p), Some(sel)) = (response.interact_pointer_pos(), selection.as_mut()) {
            let w = cam.screen_to_world(p, rect);
            sel.x1 = (w.x / 16.0).floor() as i32;
            sel.z1 = (w.y / 16.0).floor() as i32;
            out.selecting = true;
        }
    }

    // 本帧是否刚刚结束一次框选：左键在这一帧被释放，且当前有选区。
    // （多选模式据此把选区「确认」进列表）
    out.selecting_ended = response.drag_stopped_by(egui::PointerButton::Primary)
        && selection.is_some()
        && !panning;

    // 单击（左键或右键）：只记录坐标；选区是否清除由调用方决定。
    // 这里把左右键都算作"单击"——右键平时用于平移，但**没有拖动**的右键
    // 是一次干净的点击，应当按用户的预期清除选区。
    let left_click = response.clicked_by(egui::PointerButton::Primary);
    let right_click = response.clicked_by(egui::PointerButton::Secondary);
    if (left_click || right_click) && !ui.input(|i| i.modifiers.shift) {
        if let Some(p) = response.interact_pointer_pos() {
            let w = cam.screen_to_world(p, rect);
            out.clicked_world = Some((w.x as f64, w.y as f64));
            out.clicked_button = Some(if left_click {
                egui::PointerButton::Primary
            } else {
                egui::PointerButton::Secondary
            });
        }
    }

    // ---- 画已确认的多选区（淡一些，带序号）----
    for (i, sel) in opts.confirmed.iter().enumerate() {
        let (bx0, bz0, bx1, bz1) = sel.block_bounds();
        let p0 = cam.world_to_screen(Vec2::new(bx0 as f32, bz0 as f32), rect);
        let p1 = cam.world_to_screen(Vec2::new(bx1 as f32, bz1 as f32), rect);
        let r = Rect::from_two_pos(p0, p1);

        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0x6E, 0x8B, 0x5E, 46));
        // 白框 + 黑色外发光。与活动选区的「黑框 + 白晕」正好反过来，
        // 一眼就能分清哪些是已确认的、哪个是正在拖拽的。
        for (w, a) in [(8.0, 40u8), (6.0, 75), (4.5, 115)] {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(w, Color32::from_black_alpha(a)),
                egui::StrokeKind::Middle,
            );
        }
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(2.0, Color32::from_white_alpha(240)),
            egui::StrokeKind::Middle,
        );
        // 序号角标
        let badge = r.left_top() + Vec2::new(11.0, 11.0);
        painter.circle_filled(badge, 9.0, Color32::from_rgba_unmultiplied(0x2A, 0x33, 0x22, 235));
        painter.text(
            badge,
            egui::Align2::CENTER_CENTER,
            format!("{}", i + 1),
            egui::FontId::proportional(11.0),
            Color32::from_rgb(0xD8, 0xE6, 0xC4),
        );
    }

    // ---- 画活动选区 ----
    if let Some(sel) = selection {
        let (bx0, bz0, bx1, bz1) = sel.block_bounds();
        let p0 = cam.world_to_screen(Vec2::new(bx0 as f32, bz0 as f32), rect);
        let p1 = cam.world_to_screen(Vec2::new(bx1 as f32, bz1 as f32), rect);
        let r = Rect::from_two_pos(p0, p1);

        // 黑色框 + 白色发光：先画几层渐隐的白晕，再压上黑边，
        // 这样在深色（洞穴）和浅色（雪原）地形上都能看清。
        for (w, a) in [(7.0, 22u8), (5.0, 38), (3.5, 60)] {
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(w, Color32::from_white_alpha(a)),
                egui::StrokeKind::Middle,
            );
        }
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(110, 139, 94, 26));
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(2.0, Color32::from_black_alpha(235)),
            egui::StrokeKind::Middle,
        );

        // 四角手柄：黑底白心
        for corner in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()] {
            painter.rect_filled(
                Rect::from_center_size(corner, Vec2::splat(10.0)),
                0.0,
                Color32::from_black_alpha(230),
            );
            painter.rect_filled(
                Rect::from_center_size(corner, Vec2::splat(6.0)),
                0.0,
                Color32::WHITE,
            );
        }

        // 选区信息
        let (a, b, c, d) = sel.normalized();
        let mut label = format!(
            "区块 ({a},{b}) → ({c},{d})  ·  {} 区块",
            sel.chunk_count()
        );
        if !opts.confirmed.is_empty() {
            let total: usize = opts.confirmed.iter().map(|x| x.chunk_count()).sum::<usize>()
                + sel.chunk_count();
            label.push_str(&format!("  ·  多选合计 {total}"));
        }
        let anchor = r.left_top() + Vec2::new(0.0, -22.0);
        let galley = painter.layout_no_wrap(
            label,
            egui::FontId::proportional(12.0),
            Color32::from_rgb(0xF7, 0xF4, 0xEC),
        );
        let bg = Rect::from_min_size(anchor, galley.size() + Vec2::new(10.0, 6.0));
        painter.rect_filled(bg, 4.0, Color32::from_black_alpha(190));
        painter.galley(anchor + Vec2::new(5.0, 3.0), galley, Color32::WHITE);
    }

    // ---- 玩家标记 ----
    if let Some(p) = player_pos {
        let sp = cam.world_to_screen(Vec2::new(p[0] as f32, p[2] as f32), rect);
        if rect.contains(sp) {
            painter.circle_filled(sp, 6.0, Color32::from_rgb(0x3A, 0x3A, 0x32));
            painter.circle_filled(sp, 4.0, Color32::from_rgb(0xF7, 0xF4, 0xEC));
            painter.text(
                sp + Vec2::new(0.0, -14.0),
                egui::Align2::CENTER_BOTTOM,
                "玩家",
                egui::FontId::proportional(12.0),
                Color32::from_rgb(0xF7, 0xF4, 0xEC),
            );
        }
    }

    // ---- 出生点标记 ----
    if let Some((sx, sy, sz)) = spawn_pos {
        let _ = sy;
        let sp = cam.world_to_screen(Vec2::new(sx as f32, sz as f32), rect);
        if rect.contains(sp) {
            painter.circle_stroke(sp, 7.0, Stroke::new(2.0, Color32::from_rgb(0xC8, 0x8A, 0x3A)));
            painter.text(
                sp + Vec2::new(0.0, 12.0),
                egui::Align2::CENTER_TOP,
                "出生点",
                egui::FontId::proportional(11.0),
                Color32::from_rgb(0xC8, 0x8A, 0x3A),
            );
        }
    }

    // ---- 坐标提示 ----
    if let Some(hover) = response.hover_pos() {
        let w = cam.screen_to_world(hover, rect);
        let text = format!(
            "X {}  Z {}   缩放 {:.2}×   已加载 {} 区块",
            w.x.round() as i64,
            w.y.round() as i64,
            cam.scale,
            drawn
        );
        let galley = painter.layout_no_wrap(
            text,
            egui::FontId::proportional(12.0),
            Color32::from_rgb(0xE8, 0xE4, 0xD8),
        );
        let pos = rect.right_bottom() - galley.size() - Vec2::new(12.0, 8.0);
        let bg = Rect::from_min_size(pos, galley.size() + Vec2::new(12.0, 6.0));
        painter.rect_filled(bg, 4.0, Color32::from_black_alpha(170));
        painter.galley(pos + Vec2::new(6.0, 3.0), galley, Color32::WHITE);
    }

    out
}
