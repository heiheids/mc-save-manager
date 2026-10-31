//! Minecraft 存档管理器 —— 主界面。
//!
//! 布局对齐设计稿：
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ [存档标签…]                            [+ 打开新存档]        │
//! ├────────────────────────────────────────────────────────────┤
//! │ [保存] [另存为] [复制区域] [粘贴区域] [另存区域] [导入区域] [实体管理] │
//! ├──────────────┬─────────────────────────────────────────────┤
//! │ 存档名称      │                                             │
//! │ 游戏版本      │              地  图                          │
//! │ 种子          │        （区块俯视图 + 选区）                  │
//! │ …             │                                             │
//! └──────────────┴─────────────────────────────────────────────┘
//! ```

// 发布版（非 debug）以 Windows GUI 子系统链接，启动时不再弹出黑色命令行窗口。
// debug 构建保留控制台，方便看日志/panic。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod export;
mod jobs;
mod lang;
mod map_view;
mod panels;
mod theme;

use std::path::PathBuf;

use eframe::egui::{self, Color32, Vec2};
use mcworld_core::edit::{ChunkClipboard, CleanupPreset, DeleteStats, EntityFilter};
use mcworld_core::world::{ChunkMeta, Dimension, EntityMeta, LevelDat, RegionKind, World};

use jobs::{JobResult, Jobs};
use map_view::{ChunkSelection, MapCamera, TileCache};

/// 单个维度自己的视图状态（切走再切回来时原样恢复）。
pub struct DimView {
    /// 区块缩略图纹理缓存。
    pub map: TileCache,
    /// 相机位置与缩放。
    pub camera: MapCamera,
    /// 当前选区（单选模式下的活动选区）。
    pub selection: Option<ChunkSelection>,
    /// 多选模式下已确认的选区列表。
    pub selections: Vec<ChunkSelection>,
    /// 是否开启多位置选择。
    pub multi_select: bool,
    /// 已加载片区（按连通性分簇）。
    pub regions: Vec<mcworld_core::regions::Region>,
    /// 每个区块内的实体类型分布：区块坐标 → [(类型, 数量)]（按数量降序）。
    pub chunk_entity_types: std::collections::HashMap<(i32, i32), Vec<(String, usize)>>,
    /// 是否已完成地图采样。
    pub sampled: bool,
}

impl Default for DimView {
    fn default() -> Self {
        DimView {
            map: TileCache::default(),
            camera: MapCamera::default(),
            selection: None,
            selections: Vec::new(),
            multi_select: false,
            regions: Vec::new(),
            chunk_entity_types: Default::default(),
            sampled: false,
        }
    }
}

/// 一个已打开的存档标签页。
pub struct WorldTab {
    pub world: World,
    pub path: PathBuf,
    pub name: String,

    /// SwiftMap 小地图模组的数据（有则用它替代存档扫描来渲染地图）。
    pub swiftmap: Option<mcworld_core::swiftmap::SwiftMapData>,

    /// 每个维度的区块索引。
    pub chunks: std::collections::HashMap<String, Vec<ChunkMeta>>,
    /// 每个维度的实体索引。
    pub entities: std::collections::HashMap<String, Vec<EntityMeta>>,
    /// 方块实体统计。
    pub block_entities: std::collections::HashMap<String, (usize, Vec<(String, usize)>)>,

    /// 每个维度各自的视图状态。切换维度只是换一份，不会丢掉已加载的内容。
    pub views: std::collections::HashMap<String, DimView>,

    pub current_dim: usize,

    // 左侧表单的编辑缓冲
    pub edit_name: String,
    pub edit_seed: String,
    pub edit_spawn: (i32, i32, i32),
    pub edit_gametype: i32,
    pub edit_difficulty: i32,
    pub edit_cheats: bool,
    pub edit_hardcore: bool,
    pub dirty_level: bool,
    /// 有改动后等待多久自动写盘（秒）。用于把高频的表单编辑合并成一次写操作。
    pub spawn_buf: (i32, i32, i32),
    /// 玩家坐标输入框的缓冲（f64 三元组）。
    pub player_buf: (f64, f64, f64),
    /// 上一帧的玩家坐标缓冲，用于检测"用户改动"。
    pub player_prev: (f64, f64, f64),

    /// 实体管理筛选条件。
    pub filter_center: Option<(f64, f64, f64)>,
    pub filter_radius: f64,
    pub filter_types: String,
    pub filter_exclude: String,
    pub filter_text: String,
}

impl WorldTab {
    pub fn new(world: World, path: PathBuf) -> Self {
        // 先把需要的字段全部取出来，再移动 world（否则借用后无法 move）
        let name = world.level.level_name();
        let edit_name = name.clone();
        // SwiftMap 小地图模组的数据（如果有，地图直接用它，省掉全量扫描）
        let swiftmap = mcworld_core::swiftmap::SwiftMapData::open(&path).ok().flatten();
        let edit_seed = world.seed().map(|s| s.to_string()).unwrap_or_default();
        let spawn = world.level.spawn();
        let gametype = world.level.game_type().unwrap_or(0);
        let difficulty = world.level.difficulty().unwrap_or(2);
        let cheats = world.level.allow_commands();
        let hardcore = world.level.hardcore();
        // 玩家坐标先在移动 world 之前取出来（若没有则用出生点附近）
        let world_player = {
            let p = world.player_pos();
            match p {
                Some(v) => (v[0].round(), v[1].round(), v[2].round()),
                None => (spawn.0 as f64, spawn.1 as f64, spawn.2 as f64),
            }
        };

        WorldTab {
            world,
            path,
            name,
            swiftmap,
            chunks: Default::default(),
            entities: Default::default(),
            block_entities: Default::default(),
            current_dim: 0,
            views: Default::default(),
            edit_name,
            edit_seed,
            edit_spawn: spawn,
            edit_gametype: gametype,
            edit_difficulty: difficulty,
            edit_cheats: cheats,
            edit_hardcore: hardcore,
            dirty_level: false,
            spawn_buf: spawn,
            player_buf: world_player,
            player_prev: world_player,
            filter_center: None,
            filter_radius: 32.0,
            filter_types: String::new(),
            filter_exclude: String::new(),
            filter_text: String::new(),
        }
    }

    pub fn dim(&self) -> Option<&Dimension> {
        self.world.dimensions.get(self.current_dim)
    }

    pub fn dim_id(&self) -> String {
        self.dim().map(|d| d.id.clone()).unwrap_or_default()
    }

    /// 当前维度的视图状态（只读）。不存在时返回一个静态空值。
    pub fn view(&self) -> &DimView {
        static EMPTY: std::sync::OnceLock<DimView> = std::sync::OnceLock::new();
        self.views
            .get(&self.dim_id())
            .unwrap_or_else(|| EMPTY.get_or_init(DimView::default))
    }

    /// 当前维度的视图状态（可写，不存在则创建）。
    pub fn view_mut(&mut self) -> &mut DimView {
        let id = self.dim_id();
        self.views.entry(id).or_default()
    }

    /// 确保某个维度的视图状态存在。
    pub fn ensure_view(&mut self, dim_id: &str) -> &mut DimView {
        self.views.entry(dim_id.to_string()).or_default()
    }

    pub fn chunks_of_current(&self) -> &[ChunkMeta] {
        self.chunks
            .get(&self.dim_id())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn entities_of_current(&self) -> &[EntityMeta] {
        self.entities
            .get(&self.dim_id())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn is_scanned(&self) -> bool {
        self.chunks.contains_key(&self.dim_id())
    }

    /// 重置当前维度的扫描结果与视图缓存（写操作后调用）。
    pub fn invalidate(&mut self) {
        let id = self.dim_id();
        self.chunks.remove(&id);
        self.entities.remove(&id);
        self.block_entities.remove(&id);
        // 视图状态整体重建：地图纹理、片区、实体分布都要重新计算
        self.views.remove(&id);
    }

    /// 依据当前已扫描的区块重新计算「已加载片区」。
    pub fn recompute_regions(&mut self) {
        let mut ent_by_chunk: std::collections::HashMap<(i32, i32), usize> = Default::default();
        for e in self.entities_of_current() {
            *ent_by_chunk.entry((e.chunk_x, e.chunk_z)).or_default() += 1;
        }
        let chunks = self.chunks_of_current().to_vec();
        let regions = mcworld_core::regions::cluster_from_meta(&chunks, &ent_by_chunk);
        self.view_mut().regions = regions;
    }
}

/// 应用状态。
pub struct App {
    pub tabs: Vec<WorldTab>,
    pub active: usize,
    pub jobs: Jobs,
    pub status: String,
    pub status_kind: StatusKind,
    pub clipboard: Option<ChunkClipboard>,
    pub show_entities: bool,
    pub show_chunks: bool,
    pub show_gamerules: bool,
    /// 是否显示「已加载片区」列表窗口。
    pub show_regions: bool,
    /// 是否在地图上常驻显示区块网格。
    pub show_grid: bool,
    /// 跳转坐标输入框（方块坐标）。
    pub goto_x: String,
    pub goto_z: String,
    /// 区块列表的排序方式。
    pub chunk_sort: ChunkSort,
    /// 自定义渲染高度：None 表示按维度取默认值（主世界=地表，下界=Y65）。
    pub render_y: Option<i32>,
    /// 鼠标悬停在地图上时的信息（坐标 / 区块 / 群系）。
    pub hover_info: Option<HoverInfo>,
    /// 是否处于「粘贴定位」模式：鼠标处显示红色预览框，左键确认。
    pub paste_mode: bool,
    /// 粘贴预览的左上角区块坐标（由鼠标位置决定）。
    pub paste_anchor: Option<(i32, i32)>,
    pub last_delete: Option<DeleteStats>,
    /// 最近一次写操作的备份，用于「撤销」。
    pub last_backup: Option<mcworld_core::safety::LastBackup>,
    /// 「重载存档」的请求（下一帧执行）。
    pub reload_requested: bool,
    /// 地图顶部提示的剩余显示时间（秒）。
    pub hint_remaining: f32,
    /// 有改动后等待多久自动写盘（秒），合并高频表单编辑。
    pub autosave_delay: f32,
    pub pending_confirm: Option<ConfirmAction>,
    /// 是否显示「关于」窗口。
    pub show_about: bool,
    /// 上一帧的窗口尺寸；变化时强制重绘几帧，规避 glow 后端偶发白屏。
    pub last_size: egui::Vec2,
    /// 还需要强制重绘的帧数。
    pub repaint_frames: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

/// 区块列表的排序方式。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ChunkSort {
    /// 按实体数从多到少（默认：便于找出卡顿源）
    #[default]
    EntityCount,
    /// 按区块体积
    Size,
    /// 按坐标（Z 优先）
    Coord,
}

/// 鼠标在地图上的悬停信息。
#[derive(Clone, Debug, Default)]
pub struct HoverInfo {
    /// 方块坐标。
    pub x: i32,
    pub z: i32,
    /// 区块坐标。
    pub chunk_x: i32,
    pub chunk_z: i32,
    /// 该处采样到的方块名与高度。
    pub block: Option<String>,
    pub block_y: i32,
    /// 群系 id。
    pub biome: Option<String>,
}

impl HoverInfo {
    /// 状态栏显示文本（方块与群系都用游戏内中文名）。
    pub fn to_line(&self) -> String {
        let mut s = format!(
            "X {}  Z {}   |   区块 ({}, {})",
            self.x, self.z, self.chunk_x, self.chunk_z
        );
        if let Some(b) = &self.block {
            s.push_str(&format!("   |   {} @ Y{}", lang::block(b), self.block_y));
        }
        if let Some(bio) = &self.biome {
            s.push_str(&format!("   |   {}", lang::biome(bio)));
        }
        s
    }
}

/// 维度默认的渲染切片方式。
///
/// **所有维度都用 Surface**。
///
/// 早期下界用固定层 Y=65，理由是"顶部有基岩天花板，取最高方块会全是灰"。
/// 修正地表判定（"上方紧邻空气"）之后，Surface 会自然穿透顶盖继续往下找
/// 真正的岩面，实测覆盖率 100%，还能正确区分绯红菌岩 / 下界岩 / 灵魂沙，
/// 比固定层（57% 覆盖、几乎全是下界岩）好得多。
///
/// 固定层仍保留给用户手动指定（工具栏的"自定义渲染高度"）。
pub fn default_slice_mode(_dim_id: &str) -> mcworld_core::chunk::SliceMode {
    mcworld_core::chunk::SliceMode::Surface
}

/// 需要二次确认的危险操作。
pub enum ConfirmAction {
    DeleteChunks {
        count: usize,
        dim: String,
        /// 目标选区列表（每项是归一化的区块范围）。
        /// **必须在弹确认框时快照**，否则用户点击别处清空选区后，
        /// 确认执行时会静默失败。多选模式下会有多个。
        targets: Vec<(i32, i32, i32, i32)>,
    },
    DeleteEntities { count: usize, dim: String },
    /// 删除某个区块内的全部实体。
    DeleteChunkEntities {
        chunk_x: i32,
        chunk_z: i32,
        count: usize,
        dim: String,
    },
    PasteChunks {
        count: usize,
        /// 目标位置说明（展示用）。
        dim: String,
        /// 目标左上角区块坐标（粘贴定位模式确定）。
        target: (i32, i32),
    },
    SaveLevel,
}

impl Default for App {
    fn default() -> Self {
        App {
            tabs: Vec::new(),
            active: 0,
            jobs: Jobs::default(),
            status: "就绪".into(),
            status_kind: StatusKind::Info,
            clipboard: None,
            show_entities: false,
            show_chunks: false,
            show_gamerules: false,
            show_regions: false,
            show_grid: false,
            goto_x: String::new(),
            goto_z: String::new(),
            chunk_sort: ChunkSort::default(),
            render_y: None,
            hover_info: None,
            paste_mode: false,
            paste_anchor: None,
            last_delete: None,
            last_backup: None,
            reload_requested: false,
            hint_remaining: 0.0,
            autosave_delay: 0.0,
            pending_confirm: None,
            show_about: false,
            last_size: egui::Vec2::ZERO,
            repaint_frames: 3,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let mut app = App::default();

        // 若通过命令行传入了存档路径，直接打开
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(p) = args.first() {
            app.open_world(PathBuf::from(p));
        }
        app
    }

    pub fn open_world(&mut self, path: PathBuf) {
        // 从存档路径推测 .minecraft 目录，供中文名查表使用。
        // 官方语言文件版权属 Mojang，本程序不内置，只在运行时读取用户的副本。
        crate::lang::set_game_dir_from_save(&path);
        match World::open(&path) {
            Ok(world) => {
                let name = world.level.level_name();
                let dim_count = world.dimensions.len();
                self.tabs.push(WorldTab::new(world, path.clone()));
                self.active = self.tabs.len() - 1;
                // 存档刚加载：显示 5 秒的一次性提示
                self.hint_remaining = 5.0;
                self.set_status(
                    format!("已打开「{name}」({dim_count} 个维度)"),
                    StatusKind::Success,
                );
                self.request_scan();
            }
            Err(e) => {
                self.set_status(format!("打开失败: {e}"), StatusKind::Error);
            }
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>, kind: StatusKind) {
        self.status = msg.into();
        self.status_kind = kind;
    }

    /// 为当前标签页的当前维度启动扫描。
    pub fn request_scan(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.is_scanned() {
            return;
        }
        let Some(dim) = tab.dim().cloned() else { return };
        let root = tab.path.clone();
        self.jobs.spawn_scan(root, dim);
    }

    /// 重载存档：丢弃所有维度的扫描结果与地图缓存，重新从磁盘读取。
    ///
    /// 存档路径不变，只是把内存里的索引 / 纹理全部清掉再重扫，
    /// 用于「用外部工具改过存档后同步」或「怀疑内存数据过时」的情况。
    pub fn reload_world(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };

        // 重开存档：level.dat 与维度列表都可能被外部工具改动
        if let Ok(w) = mcworld_core::world::World::open(&tab.path) {
            tab.world = w;
        }
        let world = &tab.world;
        let name = world.level.level_name();
        let spawn = world.level.spawn();
        let gt = world.level.game_type().unwrap_or(0);
        let diff = world.level.difficulty().unwrap_or(2);
        let cheats = world.level.allow_commands();
        let hard = world.level.hardcore();
        tab.name = name.clone();
        tab.edit_name = name;
        tab.edit_spawn = spawn;
        tab.spawn_buf = spawn;
        tab.edit_gametype = gt;
        tab.edit_difficulty = diff;
        tab.edit_cheats = cheats;
        tab.edit_hardcore = hard;
        tab.dirty_level = false;

        tab.chunks.clear();
        tab.entities.clear();
        tab.block_entities.clear();
        tab.views.clear();
        if tab.current_dim >= tab.world.dimensions.len() {
            tab.current_dim = 0;
        }

        self.request_scan();
    }

    /// 为当前维度启动地图采样。
    ///
    /// 每个维度的采样状态独立记录在 `DimView::sampled` 里，
    /// 切走再切回来不会重复采样，也不会因为清空而丢失已渲染的地图。
    pub fn request_sample(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.view().sampled {
            return;
        }
        let chunks: Vec<(i32, i32)> = tab.chunks_of_current().iter().map(|c| (c.x, c.z)).collect();
        if chunks.is_empty() {
            return;
        }
        let Some(dim) = tab.dim().cloned() else { return };
        let root = tab.path.clone();
        let mode = self.slice_mode();
        self.jobs.spawn_sample(root, dim, chunks, mode);
    }

    /// 当前有效的切片方式（自定义高度优先，否则按维度默认）。
    pub fn slice_mode(&self) -> mcworld_core::chunk::SliceMode {
        match self.render_y {
            Some(y) => mcworld_core::chunk::SliceMode::Fixed(y),
            None => self
                .tab()
                .map(|t| default_slice_mode(&t.dim_id()))
                .unwrap_or_default(),
        }
    }

    /// 强制用当前切片方式重新采样当前维度的地图。
    pub fn resample_current(&mut self) {
        if let Some(tab) = self.tab_mut() {
            let id = tab.dim_id();
            let v = tab.ensure_view(&id);
            v.sampled = false;
            v.map.clear();
        }
        self.request_sample();
    }

    fn tab(&self) -> Option<&WorldTab> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut WorldTab> {
        self.tabs.get_mut(self.active)
    }

    /// 处理后台任务返回。
    fn handle_results(&mut self, ctx: &egui::Context, results: Vec<JobResult>) {
        for r in results {
            match r {
                JobResult::Scanned {
                    dimension,
                    chunks,
                    entities,
                    block_entity_count,
                    block_entity_types,
                } => {
                    let (n, full) = (chunks.len(), chunks.iter().filter(|c| c.is_full()).count());
                    // 先按区块坐标统计每块实体数，供「已加载片区」展示
                    let mut ent_by_chunk: std::collections::HashMap<(i32, i32), usize> =
                        Default::default();
                    for e in &entities {
                        *ent_by_chunk.entry((e.chunk_x, e.chunk_z)).or_default() += 1;
                    }
                    let regions =
                        mcworld_core::regions::cluster_from_meta(&chunks, &ent_by_chunk);
                    let region_count = regions.len();

                    // 每个区块的实体类型分布（区块列表要用）
                    let mut by_chunk_types: std::collections::HashMap<
                        (i32, i32),
                        std::collections::HashMap<String, usize>,
                    > = Default::default();
                    for e in &entities {
                        *by_chunk_types
                            .entry((e.chunk_x, e.chunk_z))
                            .or_default()
                            .entry(e.id.clone())
                            .or_default() += 1;
                    }
                    let chunk_entity_types: std::collections::HashMap<
                        (i32, i32),
                        Vec<(String, usize)>,
                    > = by_chunk_types
                        .into_iter()
                        .map(|(k, m)| {
                            let mut v: Vec<(String, usize)> = m.into_iter().collect();
                            v.sort_by(|a, b| b.1.cmp(&a.1));
                            (k, v)
                        })
                        .collect();

                    if let Some(tab) = self.tab_mut() {
                        tab.chunks.insert(dimension.clone(), chunks);
                        tab.entities.insert(dimension.clone(), entities);
                        tab.block_entities
                            .insert(dimension.clone(), (block_entity_count, block_entity_types));
                        // 视图状态按维度隔离保存，切换维度不会互相覆盖
                        let v = tab.ensure_view(&dimension);
                        v.regions = regions;
                        v.chunk_entity_types = chunk_entity_types;
                    }
                    self.set_status(
                        format!(
                            "扫描完成：{n} 个区块（{full} 个完整，{} 个未生成），{region_count} 个已加载片区",
                            n - full
                        ),
                        StatusKind::Success,
                    );
                    self.request_sample();
                }
                JobResult::Sampled { dimension, surfaces, complete } => {
                    if let Some(tab) = self.tab_mut() {
                        // 纹理写进该维度自己的缓存；即使当前已切到别的维度也能正常保存
                        let v = tab.ensure_view(&dimension);
                        for s in &surfaces {
                            v.map.insert(ctx, s);
                        }
                        if complete {
                            v.sampled = true;
                        }
                    }
                }
                JobResult::EntitiesDeleted { stats, dimension } => {
                    self.set_status(
                        format!(
                            "已删除 {} 个实体（{dimension}），清空 {} 个区块",
                            stats.removed, stats.chunks_emptied
                        ),
                        StatusKind::Success,
                    );
                    self.last_delete = Some(stats);
                    self.refresh_current();
                }
                JobResult::ChunksDeleted {
                    count,
                    message,
                    backup_dir,
                } => {
                    self.last_backup = Some(mcworld_core::safety::LastBackup {
                        dir: backup_dir,
                        label: format!("删除 {count} 个区块"),
                    });
                    self.set_status(message, StatusKind::Success);
                    self.refresh_current();
                }
                JobResult::Copied(cb) => {
                    let (n, be, en) = (cb.chunk_count(), cb.block_entities, cb.entities);
                    self.clipboard = Some(cb);
                    self.set_status(
                        format!("已复制 {n} 个区块（{be} 方块实体 / {en} 实体）"),
                        StatusKind::Success,
                    );
                }
                JobResult::Pasted { message } => {
                    self.set_status(message, StatusKind::Success);
                    self.refresh_current();
                }
                JobResult::SmclipExported { path, bytes, chunks } => {
                    self.set_status(
                        format!(
                            "已导出 {chunks} 个区块 → {}（{:.1} KB）",
                            path.display(),
                            bytes as f64 / 1024.0
                        ),
                        StatusKind::Success,
                    );
                }
                JobResult::SmclipImported {
                    clipboard,
                    message,
                } => {
                    self.clipboard = Some(clipboard);
                    self.set_status(
                        format!("{message}；点「粘贴区域」放到目标位置"),
                        StatusKind::Success,
                    );
                }
                JobResult::Failed(e) => {
                    self.set_status(format!("操作失败: {e}"), StatusKind::Error);
                }
                JobResult::Info(m) => self.set_status(m, StatusKind::Info),
            }
        }
    }

    /// 重新扫描当前维度（写操作后）。
    fn refresh_current(&mut self) {
        if let Some(tab) = self.tab_mut() {
            tab.invalidate();
        }
        self.request_scan();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let results = self.jobs.poll();
        if !results.is_empty() {
            self.handle_results(ctx, results);
        }

        // 持续低频重绘：glow 后端在完全空闲时容易丢失内容（白屏）。
        // 0.25s 一次的开销可以忽略，但能保证画面始终有效。
        // 顶部提示的倒计时（5 秒后消失）
        if self.hint_remaining > 0.0 {
            self.hint_remaining -= ctx.input(|i| i.stable_dt).min(0.1);
            if self.hint_remaining < 0.0 {
                self.hint_remaining = 0.0;
            }
            ctx.request_repaint();
        }

        // 启动 / 窗口尺寸变化时强制多画几帧。
        // glow 后端在某些机器上首帧会得到空白后台缓冲，导致整个窗口全白。
        // 连续重绘几帧即可让内容真正上屏（之前这个字段只声明未使用，机制形同虚设）。
        let size = ctx.screen_rect().size();
        if (self.repaint_frames > 0 || (size != self.last_size && size.x > 1.0)) {
            if size != self.last_size {
                self.last_size = size;
                self.repaint_frames = 3;
            }
            if self.repaint_frames > 0 {
                self.repaint_frames -= 1;
            }
            ctx.request_repaint();
        }

        // 重载存档请求
        if self.reload_requested {
            self.reload_requested = false;
            self.reload_world();
            self.hint_remaining = 5.0;
            self.set_status("已重载存档", StatusKind::Info);
        }

        // ---- 自动保存：表单改动后等 0.8 秒写盘（合并高频编辑）----
        // 统一「立即执行」模式：不再需要手动点保存。
        let has_dirty = self.tabs.get(self.active).map(|t| t.dirty_level).unwrap_or(false);
        if has_dirty {
            self.autosave_delay += ctx.input(|i| i.stable_dt).min(0.1);
            if self.autosave_delay >= 0.8 {
                self.autosave_delay = 0.0;
                panels::autosave_level(self);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        } else {
            self.autosave_delay = 0.0;
        }

        // 顶栏：存档标签 + 打开新存档
        panels::top_bar(self, ctx);

        // 工具栏
        panels::toolbar(self, ctx);

        // 左侧属性栏
        panels::sidebar(self, ctx);

        // 中央地图
        panels::map_area(self, ctx);

        // 底部状态栏
        panels::status_bar(self, ctx);

        // 弹窗
        panels::windows(self, ctx);

        // 有后台任务时持续重绘
        if self.jobs.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}

/// 窗口图标（标题栏 / 任务栏）。
///
/// 内嵌一份 64×64 的预乘无关 RGBA 原始数据（由 `.build/dump_rgba.exe`
/// 从 `assets/icon_source.png` 生成），避免为解码 PNG 引入 image crate。
fn window_icon() -> egui::IconData {
    // 64x64 原始 RGBA（由 .build/dump_rgba.exe 从 icon_source.png 生成），
    // 比 32x32 在高 DPI 下更清晰。
    const N: usize = 64;
    let raw = include_bytes!("../assets/icon_64.rgba");
    if raw.len() == N * N * 4 {
        egui::IconData {
            rgba: raw.to_vec(),
            width: N as u32,
            height: N as u32,
        }
    } else {
        // 兜底：纯色方块，保证不 panic
        egui::IconData {
            rgba: vec![0x62, 0xA0, 0x48, 0xFF].repeat(N * N),
            width: N as u32,
            height: N as u32,
        }
    }
}

fn main() -> eframe::Result<()> {
    let title = concat!("Minecraft 存档管理器 v", env!("CARGO_PKG_VERSION"));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([980.0, 600.0])
            .with_title(title)
            .with_icon(window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        title,
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
