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

mod cache;
mod export;
mod html_export;
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
    /// 是否已完成**初始**采样（出生点附近那一圈）。
    pub sampled: bool,

    // ── 按视口增量加载 ──
    //
    // 早期实现是"打开存档就把所有区块采样一遍"，上万区块的存档要等十几秒。
    // 现在改成：
    //   - 打开时只采样出生点周围 INITIAL_RADIUS 圈
    //   - 相机移动后，把**当前视野**内还没采样的区块排进队列
    //   - 队列由后台线程逐批处理，UI 上显示"加载中"
    //   - 已经采样过的区块留下纹理（缩略图），下次不再读盘
    /// 已排队或正在加载的区块，防止重复提交。
    pub pending: std::collections::HashSet<(i32, i32)>,
    /// 上一次触发加载时的视口中心（区块坐标），用于判断"是否需要重新排队列"。
    pub last_view_center: Option<(i32, i32)>,
    /// 当前是否还有区块在加载（用于显示"加载中"）。
    pub loading: bool,
    /// 是否已完成一次全图（或大范围）加载。
    pub full_loaded: bool,
    /// 初始加载的展开阶段：
    ///   0 = 还没开始
    ///   1 = 首屏（FIRST_SCREEN_RADIUS）已完成，等待补齐到 INITIAL_RADIUS
    ///   2 = 已补齐，之后完全交给视口加载
    pub expand_stage: u8,
    /// 地图画布最近的像素尺寸（宽, 高）。
    ///
    /// 「全图」要按真实画布比例算缩放，否则横竖会有一边溢出。
    /// 这个值在每次 `map_view::draw` 时更新。
    pub canvas_size: Option<(f32, f32)>,
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
            pending: Default::default(),
            last_view_center: None,
            loading: false,
            full_loaded: false,
            expand_stage: 0,
            canvas_size: None,
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
    /// 悬停查询用的列缓存（按需从磁盘读，上限 64 个区块）。
    pub column_cache: std::collections::HashMap<(i32, i32), Vec<mcworld_core::chunk::Column>>,

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
        column_cache: Default::default(),
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
    /// 按需取某区块某列的方块名与高度（鼠标悬停用）。
    ///
    /// 缩略图里不再保留 256 个 Column（那是原始区块数据，很占内存），
    /// 所以悬停时要临时从磁盘读这一个区块。读到的结果放进一个小 LRU，
    /// 避免鼠标在同一区块上移动时反复读盘。
    pub fn column_on_demand(
        &mut self,
        cx: i32,
        cz: i32,
        lx: usize,
        lz: usize,
    ) -> Option<(String, i32)> {
        // 缓存命中
        if let Some(cols) = self.column_cache.get(&(cx, cz)) {
            let col = cols.get(lz * 16 + lx)?;
            if col.y < 0 {
                return None;
            }
            return Some((col.block.clone(), col.y));
        }

        // 未命中：读这一个区块
        let dim = self.dim().cloned()?;
        let mode = mcworld_core::chunk::SliceMode::Surface;
        let mut cache = mcworld_core::world::RegionCache::new(2);
        let surf = self.world.chunk_surface(&dim, cx, cz, &mut cache, mode)?;

        // 放进缓存（容量上限 64 个区块，约 0.5 MB）
        if self.column_cache.len() >= 64 {
            self.column_cache.clear();
        }
        self.column_cache.insert((cx, cz), surf.columns.clone());

        let col = surf.columns.get(lz * 16 + lx)?;
        if col.y < 0 {
            return None;
        }
        Some((col.block.clone(), col.y))
    }

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

    /// 当前维度的区块总数。
    pub fn chunk_count(&self) -> usize {
        self.chunks_of_current().len()
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
    /// 「导出全图」对话框是否打开。
    pub show_export_html: bool,
    /// 导出 HTML 时勾选的维度 id。
    pub export_dims: Vec<String>,
    /// 「保存缓存」确认提示是否显示（区块数 > 阈值时弹出）。
    pub show_cache_prompt: bool,
    /// 提示里显示的区块数。
    pub cache_prompt_chunks: usize,
    /// 本次会话是否已经就"是否保存缓存"问过用户。
    pub cache_prompt_done: bool,
    /// 本次会话是否已经尝试读过缓存。
    pub cache_load_tried: bool,
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
            show_export_html: false,
            export_dims: Vec::new(),
            show_cache_prompt: false,
            cache_prompt_chunks: 0,
            cache_prompt_done: false,
            cache_load_tried: false,
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
                // 出生点要在打开时就被记录：相机默认停在 (0,0)，
                // 而出生点往往离原点很远（实测有 (-303, 127) 的存档），
                // 不居中就会看到一片空白，以为地图没加载。
                let spawn = world.level.spawn();

                self.tabs.push(WorldTab::new(world, path.clone()));
                self.active = self.tabs.len() - 1;

                // 把相机对准出生点（方块坐标）。
                // 缩放保持默认 1.0×，用户滚轮即可调整。
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    let v = tab.view_mut();
                    v.camera.center =
                        egui::Vec2::new(spawn.0 as f32, spawn.2 as f32);
                    v.last_view_center = None;
                }

                // 换了存档：缓存相关的会话状态要重置
                self.cache_prompt_done = false;
                self.cache_load_tried = false;
                self.show_cache_prompt = false;

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

    /// 打开存档时的初始加载半径（单位：区块）。
    ///
    /// 实测（1.21.1 存档，10317 区块）单区块采样约 5.7 ms，
    /// 半径 20 就是 1681 个区块 ≈ 9.6 秒 —— 用户明确要求"默认只加载
    /// 出生点半径 20 区块位置"，所以这里保持 20，但**分批提交**，
    /// 首屏先用小的 R首屏 半径快速出图，剩下的在后台补齐。
    pub const INITIAL_RADIUS: i32 = 20;

    /// 首屏优先半径：小于 INITIAL_RADIUS。
    ///
    /// 打开存档后先只排这一圈（约 121 个区块 ≈ 0.7 秒），
    /// 让地图立刻有内容；随后再补齐到 INITIAL_RADIUS。
    pub const FIRST_SCREEN_RADIUS: i32 = 5;

    /// 拖动时按视口加载的外扩余量（单位：区块）。
    ///
    /// 比视口多加载一圈，这样小幅拖动时不用等加载。
    pub const VIEW_MARGIN: i32 = 2;

    /// 计算"当前需要显示的区块范围"（区块坐标闭区间）。
    ///
    /// `viewport` 是地图画布的像素矩形，相机 `scale` 是每方块像素数，
    /// 所以可见方块宽 = viewport.width / scale，再除以 16 得到区块数。
    fn visible_chunk_range(
        cam: &map_view::MapCamera,
        viewport: egui::Rect,
        margin: i32,
    ) -> (i32, i32, i32, i32) {
        let half_w = (viewport.width() / 2.0) / cam.scale.max(0.01);
        let half_h = (viewport.height() / 2.0) / cam.scale.max(0.01);

        let min_bx = cam.center.x - half_w;
        let max_bx = cam.center.x + half_w;
        let min_bz = cam.center.y - half_h;
        let max_bz = cam.center.y + half_h;

        let cx0 = (min_bx / 16.0).floor() as i32 - margin;
        let cz0 = (min_bz / 16.0).floor() as i32 - margin;
        let cx1 = (max_bx / 16.0).floor() as i32 + margin;
        let cz1 = (max_bz / 16.0).floor() as i32 + margin;
        (cx0, cz0, cx1, cz1)
    }

    /// 按当前视口，把**尚未采样**的区块排进加载队列。
    ///
    /// `viewport` 为 `None` 时（还没画过地图）退回"出生点 INITIAL_RADIUS 圈"。
    ///
    /// 已经被采样过的区块只存在于 `DimView::map`（缩略图纹理）里，
    /// 原始区块数据早就释放了，所以这里只需要查纹理缓存就能判断"要不要加载"。
    pub fn request_viewport_sample(&mut self, viewport: Option<egui::Rect>) {
        self.request_viewport_sample_r(viewport, Self::INITIAL_RADIUS);
    }

    /// 同 `request_viewport_sample`，但可指定出生点半径。
    ///
    /// `radius` 只在 viewport 为 None（还没画布）时生效。
    pub fn request_viewport_sample_r(&mut self, viewport: Option<egui::Rect>, radius: i32) {
        let mode = self.slice_mode();
        let Some(tab) = self.tabs.get_mut(self.active) else { return };

        // 先取需要的字段，避免和后面的可变借用打架
        let dim = match tab.dim().cloned() {
            Some(d) => d,
            None => return,
        };

        let (cx0, cz0, cx1, cz1) = match viewport {
            Some(vp) => Self::visible_chunk_range(&tab.view().camera, vp, Self::VIEW_MARGIN),
            None => {
                // 还没有画布尺寸：用出生点附近的固定圈
                let (sx, _sy, sz) = tab.world.level.spawn();
                let scx = (sx as f32 / 16.0).floor() as i32;
                let scz = (sz as f32 / 16.0).floor() as i32;
                let r = radius;
                (scx - r, scz - r, scx + r, scz + r)
            }
        };

        // 收集需要加载的区块。
        //
        // ★ 遍历**区块索引**而不是矩形范围：
        //   全图缩放下视口矩形可能有上亿个格点，但其中绝大多数根本没有区块。
        //   按矩形遍历既慢，又会被"超过 4096 就放弃"的保护逻辑拦住 ——
        //   那样"全图"按钮就完全没用了。
        //   索引里只有真实存在的区块，按范围过滤后数量可控。
        let mut need: Vec<(i32, i32)> = Vec::new();
        {
            let v = tab.view();
            for c in tab.chunks_of_current() {
                if c.x < cx0 || c.x > cx1 || c.z < cz0 || c.z > cz1 {
                    continue;
                }
                let key = (c.x, c.z);
                if v.map.contains(key) || v.pending.contains(&key) {
                    continue;
                }
                need.push(key);
            }
        }

        // 单次提交上限：避免一次把上万个区块塞给后台线程，
        // 导致这一帧卡住。剩下的下一帧会继续补，表现为"分批出现"。
        const MAX_PER_BATCH: usize = 1024;
        if need.len() > MAX_PER_BATCH {
            need.truncate(MAX_PER_BATCH);
        }

        if need.is_empty() {
            // 没有新东西要加载：如果队列已空，清掉 loading 标记
            if tab.view().pending.is_empty() {
                tab.view_mut().loading = false;
            }
            return;
        }

        // 记入 pending，防止重复提交
        {
            let v = tab.view_mut();
            for k in &need {
                v.pending.insert(*k);
            }
            v.loading = true;
        }

        let root = tab.path.clone();
        self.jobs.spawn_sample(root, dim, need, mode);
    }


    /// 为当前维度启动地图采样。
    ///
    /// 每个维度的采样状态独立记录在 `DimView::sampled` 里，
    /// 切走再切回来不会重复采样，也不会因为清空而丢失已渲染的地图。
    pub fn request_sample(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        // 已经做过初始加载就不再重复
        if tab.view().sampled {
            return;
        }
        if tab.chunks_of_current().is_empty() {
            return;
        }
        // 打开存档时只加载出生点周围一圈，不再全量采样。
        //
        // 半径 5 先出图（约 0.7 秒），补齐到半径 20 由后续帧的
        // 展开阶段完成。出生点已经在 open_world 里置为相机中心，
        // 所以这一圈正好铺满首屏。
        self.request_viewport_sample_r(None, Self::FIRST_SCREEN_RADIUS);
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
                JobResult::ScannedRange { dimension, chunks } => {
                    if let Some(tab) = self.tab_mut() {
                        // 把重扫到的区块并回索引（按坐标去重）
                        let list = tab.chunks.entry(dimension.clone()).or_default();
                        list.retain(|c| {
                            !chunks.iter().any(|n| n.x == c.x && n.z == c.z)
                        });
                        list.extend(chunks.clone());
                        list.sort_unstable_by_key(|c| (c.z, c.x));
                    }
                    // 重新排队采样（纹理已被 drop_range 清掉）
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        tab.view_mut().last_view_center = None;
                    }
                }
                JobResult::Sampled { dimension, surfaces, missing, complete } => {
                    let loaded = surfaces.len();
                    if let Some(tab) = self.tab_mut() {
                        // 纹理写进该维度自己的缓存；即使当前已切到别的维度也能正常保存。
                        // 写完纹理就不再需要原始区块数据了，chunk_surface 返回的
                        // ChunkSurface 在下一批里自然释放 —— 这就是"缩略图化"。
                        let v = tab.ensure_view(&dimension);
                        for s in &surfaces {
                            let key = (s.x, s.z);
                            v.map.insert(ctx, s);
                            v.pending.remove(&key);
                        }
                        // 无数据的区块也要从 pending 里移走，
                        // 否则"加载中"会一直显示下去。
                        for key in &missing {
                            v.pending.remove(key);
                            v.map.mark_missing(*key);
                        }
                        if complete {
                            v.sampled = true;
                            // 首屏那一圈做完了 → 进入"补齐到 INITIAL_RADIUS"阶段。
                            // 不能直接跳到 2，否则半径 20 那圈永远补不上。
                            if v.expand_stage == 0 {
                                v.expand_stage = 1;
                            } else {
                                v.expand_stage = 2;
                            }
                        }
                        // 只要队列空了就撤掉"加载中"，不必等 complete：
                        // 中途可能有新批次插进来，死等 complete 会让提示卡住。
                        if v.pending.is_empty() {
                            v.loading = false;
                        }
                    }
                    let _ = loaded;
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
                    bounds,
                } => {
                    self.last_backup = Some(mcworld_core::safety::LastBackup {
                        dir: backup_dir,
                        label: format!("删除 {count} 个区块"),
                    });
                    self.set_status(message, StatusKind::Success);
                    // 只刷新被删区域 + 周边 5 区块，不再全图重扫。
                    self.refresh_region(bounds.0, bounds.1, bounds.2, bounds.3);
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
                JobResult::CacheSaved { message } => {
                    self.set_status(message, StatusKind::Success);
                }
                JobResult::CacheSaveFailed { error } => {
                    self.set_status(format!("保存缓存失败: {error}"), StatusKind::Error);
                }
                JobResult::CacheLoaded {
                    dimension,
                    tiles,
                    stale,
                } => {
                    let reused = tiles.len();
                    let stale_n = stale.len();
                    if let Some(tab) = self.tab_mut() {
                        let v = tab.ensure_view(&dimension);
                        for (key, px) in &tiles {
                            v.map.insert_from_pixels(ctx, *key, px.clone());
                        }
                        v.sampled = true;
                        v.expand_stage = 2;
                        // 变化的区块排进重采样队列
                        for key in &stale {
                            v.pending.insert(*key);
                        }
                    }
                    if reused > 0 {
                        self.set_status(
                            format!(
                                "已从缓存载入 {reused} 个区块的缩略图（{stale_n} 个有变动，正在更新）"
                            ),
                            StatusKind::Info,
                        );
                    }
                    // 有变动的区块立刻重新采样
                    if stale_n > 0 {
                        self.request_stale_resample();
                    }
                }
                JobResult::HtmlExported { message } => {
                    self.set_status(message, StatusKind::Success);
                }
                JobResult::HtmlExportFailed { error } => {
                    self.set_status(format!("导出失败: {error}"), StatusKind::Error);
                }
                JobResult::Failed(e) => {
                    self.set_status(format!("操作失败: {e}"), StatusKind::Error);
                    // 任务失败时 pending 里的区块不会有任何回包，
                    // 必须主动清掉，否则"加载中"会永久挂在屏幕上。
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        let v = tab.view_mut();
                        v.pending.clear();
                        v.loading = false;
                    }
                }
                JobResult::Info(m) => self.set_status(m, StatusKind::Info),
            }
        }

    }

    /// 重新扫描当前维度（写操作后）。
    pub fn request_stale_resample(&mut self) {
        let mode = self.slice_mode();
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let need: Vec<(i32, i32)> = tab.view().pending.iter().copied().collect();
        if need.is_empty() {
            return;
        }
        let dim = match tab.dim().cloned() {
            Some(d) => d,
            None => return,
        };
        let root = tab.path.clone();
        tab.view_mut().loading = true;
        self.jobs.spawn_sample(root, dim, need, mode);
    }

    /// 触发一次"保存缓存"。
    ///
    /// 把当前维度已渲染的缩略图连同 region 指纹存到磁盘。
    /// 下次打开同一存档时可以直接复用，省掉重新采样的时间。
    pub fn request_save_cache(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let dim = match tab.dim().cloned() {
            Some(d) => d,
            None => return,
        };

        // 从纹理缓存里取出每区块的像素
        let tiles = tab.view().map.snapshot_for_cache();
        if tiles.is_empty() {
            self.set_status("当前维度还没有已渲染的区块，无法保存缓存", StatusKind::Info);
            return;
        }

        let save_root = tab.path.clone();
        let slice = slice_key(&self.slice_mode());
        let dv = tab.world.level.data_version();
        let region_dir = dim.dir(mcworld_core::world::RegionKind::Terrain);
        let n = tiles.len();

        self.jobs.spawn_save_cache(
            save_root,
            dim.id.clone(),
            dim.display.clone(),
            region_dir,
            slice,
            dv,
            tiles,
        );
        self.set_status(format!("正在保存 {n} 个区块的缩略图缓存…"), StatusKind::Info);
    }

    /// 打开存档后尝试读缓存。
    ///
    /// 读到就直接把缩略图塞进纹理缓存，只对"变了"的区块重新采样。
    pub fn try_load_cache(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let dim = match tab.dim().cloned() {
            Some(d) => d,
            None => return,
        };
        let save_root = tab.path.clone();
        let slice = slice_key(&self.slice_mode());
        let dv = tab.world.level.data_version();
        let region_dir = dim.dir(mcworld_core::world::RegionKind::Terrain);

        self.jobs
            .spawn_load_cache(save_root, dim.id, region_dir, slice, dv);
    }

    /// 局部刷新：只重扫指定区块范围（外扩 `REFRESH_MARGIN` 圈）。
    ///
    /// 删除区块后调用。相比 `refresh_current`（整体作废 + 全量重扫），
    /// 这里只做三件事：
    ///   1. 从区块索引里移除该范围内的条目
    ///   2. 丢弃该范围内的缩略图纹理，让它们重新排队采样
    ///   3. 重新扫描该范围（携带邻域，因为区块边界可能互相影响）
    ///
    /// 地图其余部分完全不动，避免大存档删几个区块就卡全图。
    pub fn refresh_region(&mut self, x0: i32, z0: i32, x1: i32, z1: i32) {
        const REFRESH_MARGIN: i32 = 5;

        let (rx0, rz0) = (x0 - REFRESH_MARGIN, z0 - REFRESH_MARGIN);
        let (rx1, rz1) = (x1 + REFRESH_MARGIN, z1 + REFRESH_MARGIN);

        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let dim_id = tab.dim_id();

        // (1) 从索引里去掉该范围
        if let Some(list) = tab.chunks.get_mut(&dim_id) {
            list.retain(|c| !(c.x >= rx0 && c.x <= rx1 && c.z >= rz0 && c.z <= rz1));
        }

        // (2) 丢弃该范围的缩略图 + pending，让它们重新排队
        {
            let v = tab.ensure_view(&dim_id);
            v.map.drop_range(rx0, rz0, rx1, rz1);
            v.pending.retain(|(cx, cz)| {
                !(*cx >= rx0 && *cx <= rx1 && *cz >= rz0 && *cz <= rz1)
            });
            // 强制下一帧重新计算视口，把这块补回来
            v.last_view_center = None;
        }

        // (3) 后台重扫该范围
        let dim = match tab.dim().cloned() {
            Some(d) => d,
            None => return,
        };
        let root = tab.path.clone();
        self.jobs.spawn_scan_range(root, dim, rx0, rz0, rx1, rz1);
    }

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

        // ---- 大存档：提示可以保存缩略图缓存 ----
        //
        // 区块数超过阈值时弹一次提示。只弹一次（cache_prompt_done），
        // 否则每次扫描完成都弹会很烦。
        if !self.cache_prompt_done {
            let n = self.tab().map(|t| t.chunk_count()).unwrap_or(0);
            if n > 2000 {
                self.cache_prompt_done = true;
                self.cache_prompt_chunks = n;
                self.show_cache_prompt = true;
            }
        }

        // ---- 初次扫描完成后尝试读缓存 ----
        if !self.cache_load_tried {
            let ready = self.tab().map(|t| t.is_scanned()).unwrap_or(false);
            if ready {
                self.cache_load_tried = true;
                self.try_load_cache();
            }
        }

        // 初始加载的展开阶段：
        //   阶段 1 → 把半径补齐到 INITIAL_RADIUS（首屏只做过半径 5）
        //   阶段 2 → 之后完全交给视口加载，不再主动补
        {
            let stage = self
                .tab()
                .map(|t| t.view().expand_stage)
                .unwrap_or(0);
            let busy = self
                .tab()
                .map(|t| t.view().loading || !t.view().pending.is_empty())
                .unwrap_or(false);
            if stage == 1 && !busy {
                // 首屏已完成且当前没有在跑的任务 → 补半径 20 那一圈。
                //
                // ★ 必须先置 stage = 2 再请求：
                //   如果半径 20 内已经没有没采样的区块（例如小存档全部装得下），
                //   request_viewport_sample_r 会直接返回，不会产生任何"完成"回调。
                //   要是这时还停在 stage 1，下一帧会再进来一次 ——
                //   既白白遍历一遍索引，又把 loading 置成 true 却没人清，
                //   结果"加载中"永远挂在屏幕上。
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    let v = tab.view_mut();
                    v.expand_stage = 2;
                    v.loading = true;
                }
                self.request_viewport_sample_r(None, Self::INITIAL_RADIUS);
                // 没排出任何任务（need 为空）时 loading 会留在 true，
                // 这里立刻纠正。
                let idle = self
                    .tab()
                    .map(|t| t.view().pending.is_empty())
                    .unwrap_or(true);
                if idle {
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        tab.view_mut().loading = false;
                    }
                }
            }
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

        // ★ 底部状态栏必须在 `map_area` **之前**声明。
        //
        //   `map_area` 用的是 `CentralPanel`，它会吃掉"当前剩余的全部空间"。
        //   egui 要求所有 `TopBottomPanel` / `SidePanel` 都先于 `CentralPanel`
        //   添加；状态栏如果放在后面，中央面板已经按"没有状态栏"算过高度，
        //   于是状态栏直接压在画布底部 ——
        //   表现为地图右下角有一条被截断的黑色悬停提示框
        //   （框本身 22px 高，只有顶部 7px 露在状态栏上方，文字全被盖住）。
        panels::status_bar(self, ctx);

        // 中央地图
        panels::map_area(self, ctx);

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
    // 无界面导出模式：
    //   mcworld-app <存档路径> --export-html <输出.html> [维度id ...]
    //
    // 便于批处理，也方便在没有图形环境时导出。
    // 不指定维度就是全部维度。
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        // 无界面缓存自检：
        //   mcworld-app <存档> --cache-test
        // 保存一份缓存 → 立刻读回 → 对比指纹，验证整条链路。
        if args.iter().any(|a| a == "--cache-test") {
            let no_save = args.iter().any(|a| a == "--no-save");
            let save = args.first().cloned().unwrap_or_default();
            if save.is_empty() {
                eprintln!("用法: mcworld-app <存档> --cache-test");
                std::process::exit(2);
            }
            let root = std::path::Path::new(&save);
            let world = match World::open(root) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("打开失败: {e}");
                    std::process::exit(1);
                }
            };
            let dv = world.level.data_version();
            let mut any = false;
            for dim in &world.dimensions {
                if !dim.exists(mcworld_core::world::RegionKind::Terrain) {
                    continue;
                }
                let metas = world.scan_chunks(dim, &mut |_, _| {}).unwrap_or_default();
                if metas.is_empty() {
                    continue;
                }
                any = true;

                // 造几块假缩略图（16x16 RGBA），模拟已渲染的区块
                let mut tiles: Vec<((i32, i32), Vec<u8>)> = Vec::new();
                for (i, m) in metas.iter().take(50).enumerate() {
                    let mut px = vec![0u8; 16 * 16 * 4];
                    for q in px.chunks_exact_mut(4) {
                        q[0] = (i * 5) as u8;
                        q[1] = 100;
                        q[2] = 50;
                        q[3] = 255;
                    }
                    tiles.push(((m.x, m.z), px));
                }

                let safe: String = dim
                    .id
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                let dir = crate::cache::save_cache_dir(root).join(safe);
                let region_dir = dim.dir(mcworld_core::world::RegionKind::Terrain);
                let stamps = crate::cache::collect_stamps(&region_dir);
                let meta = crate::cache::CacheMeta {
                    dim_id: dim.id.clone(),
                    slice: "surface".into(),
                    data_version: dv,
                    regions: stamps,
                    chunks: tiles.iter().map(|(k, _)| *k).collect(),
                };
                let data: Vec<Vec<u8>> = tiles.iter().map(|(_, px)| px.clone()).collect();

                if !no_save {
                if let Err(e) = crate::cache::save(&dir, &meta, &data) {
                    eprintln!("[{}] 保存失败: {e}", dim.display);
                    continue;
                }
                println!("[{}] 保存 {} 个区块缩略图 -> {}", dim.display, data.len(), dir.display());
                }

                match crate::cache::load(&dir) {
                    Some((m2, t2)) => {
                        println!("[{}] 读回 {} 区块, dim={}, dv={}", dim.display, t2.len(), m2.dim_id, m2.data_version);
                        let now = crate::cache::collect_stamps(&region_dir);
                        let changed = m2.changed_regions(&now);
                        if changed.is_empty() {
                            println!("[{}] OK 指纹一致，可直接复用", dim.display);
                        } else {
                            println!("[{}] WARN 检测到 {} 个 region 变化", dim.display, changed.len());
                        }
                        println!("[{}] OK 首块像素 {} {} {} {}", dim.display,
                            t2[0][0], t2[0][1], t2[0][2], t2[0][3]);
                    }
                    None => println!("[{}] FAIL 读回失败", dim.display),
                }
                println!();
            }
            if !any {
                eprintln!("没有可测试的维度");
                std::process::exit(1);
            }
            std::process::exit(0);
        }

        if let Some(pos) = args.iter().position(|a| a == "--export-html") {
            let save = args.first().cloned().unwrap_or_default();
            let out = args.get(pos + 1).cloned().unwrap_or_default();
            let dims: Vec<String> = args.iter().skip(pos + 2).cloned().collect();
            if save.is_empty() || out.is_empty() {
                eprintln!("用法: mcworld-app <存档> --export-html <输出.html> [维度...]");
                std::process::exit(2);
            }
            match crate::html_export::export_html(
                std::path::Path::new(&save),
                &dims,
                std::path::Path::new(&out),
                mcworld_core::chunk::SliceMode::Surface,
                &mut |dim, done, total| {
                    if done % 200 == 0 || done == total {
                        eprintln!("{dim} {done}/{total}");
                    }
                },
            ) {
                Ok(msg) => {
                    println!("{msg}");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("导出失败: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

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


/// 切片方式的稳定字符串表示（缓存指纹用）。
fn slice_key(mode: &mcworld_core::chunk::SliceMode) -> String {
    match mode {
        mcworld_core::chunk::SliceMode::Surface => "surface".to_string(),
        mcworld_core::chunk::SliceMode::Fixed(y) => format!("fixed:{y}"),
    }
}
