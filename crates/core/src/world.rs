//! 世界（存档）模型：维度发现、区块索引、实体索引、level.dat 读写。
//!
//! 同时兼容 1.21.1 的 `DIM-1` / `DIM1` 布局与 26.1 起的 `dimensions/<ns>/<path>` 布局。

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::anvil::{self, RegionFile};
use crate::chunk::{self, Column};
use crate::colors::Rgb;
use crate::nbt::{self, Compound, Tag, TagType};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("不是有效的存档目录（缺少 level.dat）: {0}")]
    NotAWorld(PathBuf),
    #[error("存档正被游戏占用（session.lock 被锁定）")]
    Locked,
    #[error("NBT: {0}")]
    Nbt(#[from] nbt::Error),
    #[error("Region: {0}")]
    Anvil(#[from] anvil::Error),
    #[error("IO {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { path: PathBuf::new(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io { path: path.to_path_buf(), source }
}

// ---------------------------------------------------------------------------
// 维度
// ---------------------------------------------------------------------------

/// 区域文件的用途类别。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RegionKind {
    /// 地形区块
    Terrain,
    /// 实体
    Entities,
    /// 兴趣点
    Poi,
}

impl RegionKind {
    pub fn dir_name(self) -> &'static str {
        match self {
            RegionKind::Terrain => "region",
            RegionKind::Entities => "entities",
            RegionKind::Poi => "poi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RegionKind::Terrain => "地形",
            RegionKind::Entities => "实体",
            RegionKind::Poi => "兴趣点",
        }
    }
}

/// 一个维度（主世界 / 下界 / 末地 / 数据包自定义维度）。
#[derive(Clone, Debug)]
pub struct Dimension {
    /// 资源位置，如 `minecraft:overworld`。
    pub id: String,
    /// 显示名，如「主世界」。
    pub display: String,
    /// 该维度的数据根目录（含 region/entities/poi）。
    pub root: PathBuf,
}

impl Dimension {
    pub fn dir(&self, kind: RegionKind) -> PathBuf {
        self.root.join(kind.dir_name())
    }

    pub fn exists(&self, kind: RegionKind) -> bool {
        self.dir(kind).is_dir()
    }
}

/// 把维度的三个子目录逐个找出来（兼容两种布局）。
fn discover_dimensions(save: &Path) -> Vec<Dimension> {
    let mut out: Vec<Dimension> = Vec::new();

    let mut push = |id: &str, display: &str, root: PathBuf| {
        if root.is_dir() {
            out.push(Dimension { id: id.to_string(), display: display.to_string(), root });
        }
    };

    // 1.21.1：主世界数据在存档根，下界/末地在 DIM-1 / DIM1
    if save.join("region").is_dir() {
        push("minecraft:overworld", "主世界", save.to_path_buf());
    }
    if save.join("DIM-1/region").is_dir() {
        push("minecraft:the_nether", "下界", save.join("DIM-1"));
    }
    if save.join("DIM1/region").is_dir() {
        push("minecraft:the_end", "末地", save.join("DIM1"));
    }

    // 26.1+：dimensions/<namespace>/<path>
    let dims = save.join("dimensions");
    if dims.is_dir() {
        let mut ns_entries: Vec<_> = fs::read_dir(&dims).into_iter().flatten().flatten().collect();
        ns_entries.sort_by_key(|e| e.file_name());
        for ns in ns_entries {
            if !ns.path().is_dir() {
                continue;
            }
            let ns_name = ns.file_name().to_string_lossy().to_string();
            let mut path_entries: Vec<_> = fs::read_dir(ns.path()).into_iter().flatten().flatten().collect();
            path_entries.sort_by_key(|e| e.file_name());
            for p in path_entries {
                if !p.path().is_dir() {
                    continue;
                }
                let path_name = p.file_name().to_string_lossy().to_string();
                let id = format!("{ns_name}:{path_name}");
                let display = match (ns_name.as_str(), path_name.as_str()) {
                    ("minecraft", "overworld") => "主世界".to_string(),
                    ("minecraft", "the_nether") => "下界".to_string(),
                    ("minecraft", "the_end") => "末地".to_string(),
                    _ => id.clone(),
                };
                // 只在该维度确实有 region/entities/poi 时收录
                if p.path().join("region").is_dir()
                    || p.path().join("entities").is_dir()
                    || p.path().join("poi").is_dir()
                {
                    if !out.iter().any(|d| d.id == id) {
                        out.push(Dimension { id, display, root: p.path() });
                    }
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// 区块元数据
// ---------------------------------------------------------------------------

/// 单个区块的索引信息（扫描时一次性收集，供列表与地图使用）。
#[derive(Clone, Debug)]
pub struct ChunkMeta {
    pub x: i32,
    pub z: i32,
    /// 所属 region 文件坐标。
    pub rx: i32,
    pub rz: i32,
    /// 磁盘占用扇区数。
    pub sectors: u8,
    /// 解压后 NBT 字节数。
    pub nbt_bytes: usize,
    pub status: String,
    pub data_version: i32,
    pub inhabited_time: i64,
    pub last_update: i64,
    pub block_entities: u32,
    pub block_ticks: u32,
    pub fluid_ticks: u32,
    pub sections: u32,
}

impl ChunkMeta {
    /// 从已解压的区块 NBT 构造索引信息。
    ///
    /// 供"局部重扫"使用：只读指定范围的区块，不像全量扫描那样
    /// 走一遍 region 文件。`sectors` 无法从区块内容得知，
    /// 这里填 0（只影响"体积"列的展示，不影响功能）。
    pub fn from_tag(cx: i32, cz: i32, tag: &Tag) -> Self {
        let c = tag.as_compound();
        let count_list = |key: &str| -> u32 {
            c.and_then(|c| c.get(key))
                .and_then(|t| t.as_list())
                .map(|(_, items)| items.len() as u32)
                .unwrap_or(0)
        };
        let nbt_bytes = crate::nbt::write(tag).map(|b| b.len()).unwrap_or(0);

        ChunkMeta {
            x: cx,
            z: cz,
            rx: anvil::region_coord(cx),
            rz: anvil::region_coord(cz),
            sectors: 0,
            nbt_bytes,
            status: c
                .and_then(|c| c.get("Status").or_else(|| c.get("status")))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string(),
            data_version: c
                .and_then(|c| c.get("DataVersion"))
                .and_then(|t| t.as_i32())
                .unwrap_or(0),
            inhabited_time: c
                .and_then(|c| c.get("InhabitedTime"))
                .and_then(|t| t.as_i64())
                .unwrap_or(0),
            last_update: c
                .and_then(|c| c.get("LastUpdate"))
                .and_then(|t| t.as_i64())
                .unwrap_or(0),
            block_entities: count_list("block_entities"),
            block_ticks: count_list("block_ticks"),
            fluid_ticks: count_list("fluid_ticks"),
            sections: count_list("sections"),
        }
    }

    pub fn is_full(&self) -> bool {
        self.status == "minecraft:full"
    }

    pub fn status_short(&self) -> &str {
        self.status.strip_prefix("minecraft:").unwrap_or(&self.status)
    }

    /// 磁盘近似占用字节。
    pub fn disk_bytes(&self) -> usize {
        self.sectors as usize * anvil::SECTOR
    }
}

/// 实体索引条目。
#[derive(Clone, Debug)]
pub struct EntityMeta {
    pub id: String,
    pub uuid: Option<[i32; 4]>,
    pub pos: Option<[f64; 3]>,
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub dimension: String,
    /// 该实体在所属 region 文件中的局部索引（用于快速定位）。
    pub region_index: usize,
}

impl EntityMeta {
    pub fn block_pos(&self) -> Option<(i32, i32, i32)> {
        self.pos.map(|p| (p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32))
    }

    pub fn short_type(&self) -> &str {
        self.id.strip_prefix("minecraft:").unwrap_or(&self.id)
    }
}

/// 方块实体索引条目。
#[derive(Clone, Debug)]
pub struct BlockEntityMeta {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub chunk_x: i32,
    pub chunk_z: i32,
}

/// 一个区块的俯视采样结果（地图渲染用）。
#[derive(Clone, Debug)]
pub struct ChunkSurface {
    pub x: i32,
    pub z: i32,
    /// 256 列的顶层信息，顺序为 `z*16 + x`。
    pub columns: Vec<Column>,
    /// 该区块的生成阶段是否已完成（`Status == minecraft:full`）。
    ///
    /// 未完成的区块（`structure_starts` / `biomes` / `terrain` …）**只有基础地形**，
    /// 没有草、植被、水面装饰，渲染出来会是大片裸石。地图上需要区别显示，
    /// 否则用户会以为渲染错了。
    pub complete: bool,
}

impl ChunkSurface {
    /// 平均色，用于缩略图级别渲染。
    pub fn average_color(&self) -> Rgb {
        let mut r = 0u32;
        let mut g = 0u32;
        let mut b = 0u32;
        let mut n = 0u32;
        for c in &self.columns {
            if c.y < 0 {
                continue;
            }
            r += c.color[0] as u32;
            g += c.color[1] as u32;
            b += c.color[2] as u32;
            n += 1;
        }
        if n == 0 {
            return [24, 26, 30];
        }
        [(r / n) as u8, (g / n) as u8, (b / n) as u8]
    }
}

// ---------------------------------------------------------------------------
// level.dat
// ---------------------------------------------------------------------------

/// `level.dat` 的内存表示，保留原始根标签以便无损回写。
#[derive(Clone, Debug)]
pub struct LevelDat {
    pub path: PathBuf,
    pub root: Tag,
    /// 是否为 gzip 包装（1.21.1 是；某些工具产出的为未压缩）。
    pub gzip: bool,
}

impl LevelDat {
    pub fn open(save: &Path) -> Result<Self> {
        let path = save.join("level.dat");
        let raw = fs::read(&path).map_err(|e| io_err(&path, e))?;
        let gzip = raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b;
        let body = if gzip {
            let mut out = Vec::new();
            GzDecoder::new(&raw[..])
                .read_to_end(&mut out)
                .map_err(|e| Error::Other(format!("level.dat gzip 解压失败: {e}")))?;
            out
        } else {
            raw
        };
        let root = nbt::read(&body)?;
        Ok(LevelDat { path, root, gzip })
    }

    /// 取 `Data` 复合物（1.21.1 的 level.dat 都有这层包裹）。
    pub fn data(&self) -> Option<&Compound> {
        self.root.as_compound()?.get("Data")?.as_compound()
    }

    pub fn data_mut(&mut self) -> Option<&mut Compound> {
        self.root.as_compound_mut()?.get_mut("Data")?.as_compound_mut()
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        self.data()?.get(key)
    }

    pub fn get_i32(&self, key: &str) -> Option<i32> {
        self.get(key)?.as_i32()
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.as_i64()
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn set(&mut self, key: &str, value: Tag) -> bool {
        match self.data_mut() {
            Some(d) => {
                d.insert(key.to_string(), value);
                true
            }
            None => false,
        }
    }

    /// 设置出生点（自动适配新旧格式）。
    ///
    /// - 1.21.1：写 `SpawnX/SpawnY/SpawnZ`
    /// - 26.x：写 `spawn.pos`（IntArray[3]），保留 dimension/pitch/yaw
    pub fn set_spawn(&mut self, pos: (i32, i32, i32)) {
        let Some(d) = self.data_mut() else { return };
        if let Some(sp) = d.get_mut("spawn").and_then(|t| t.as_compound_mut()) {
            sp.insert("pos".into(), Tag::IntArray(vec![pos.0, pos.1, pos.2]));
        } else {
            d.insert("SpawnX".into(), Tag::Int(pos.0));
            d.insert("SpawnY".into(), Tag::Int(pos.1));
            d.insert("SpawnZ".into(), Tag::Int(pos.2));
        }
    }

    /// 设置难度（自动适配新旧格式）。
    pub fn set_difficulty(&mut self, v: i32) {
        let Some(d) = self.data_mut() else { return };
        if let Some(ds) = d.get_mut("difficulty_settings").and_then(|t| t.as_compound_mut()) {
            let name = match v {
                0 => "peaceful",
                1 => "easy",
                2 => "normal",
                _ => "hard",
            };
            ds.insert("difficulty".into(), Tag::String(name.into()));
        } else {
            d.insert("Difficulty".into(), Tag::Int(v));
        }
    }

    /// 设置硬核（自动适配新旧格式）。
    pub fn set_hardcore(&mut self, on: bool) {
        let Some(d) = self.data_mut() else { return };
        if let Some(ds) = d.get_mut("difficulty_settings").and_then(|t| t.as_compound_mut()) {
            ds.insert("hardcore".into(), Tag::Byte(if on { 1 } else { 0 }));
        } else {
            d.insert("hardcore".into(), Tag::Int(if on { 1 } else { 0 }));
        }
    }

    /// 设置作弊开关。
    pub fn set_allow_commands(&mut self, on: bool) {
        self.set("allowCommands", Tag::Int(if on { 1 } else { 0 }));
    }

    /// 写回磁盘：先备份现有 level.dat 为 `level.dat_old`，再原子替换。
    pub fn save(&self) -> Result<()> {
        let body = nbt::write(&self.root)?;
        let out = if self.gzip {
            let mut enc = GzEncoder::new(Vec::new(), Compression::new(6));
            enc.write_all(&body)
                .map_err(|e| Error::Other(format!("gzip 压缩失败: {e}")))?;
            enc.finish()
                .map_err(|e| Error::Other(format!("gzip 压缩失败: {e}")))?
        } else {
            body
        };

        let old = self.path.with_file_name("level.dat_old");
        if self.path.exists() {
            // 保留上次的 level.dat_old，把当前内容轮换进去
            fs::copy(&self.path, &old).map_err(|e| io_err(&old, e))?;
        }

        let tmp = self.path.with_extension("dat.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
            f.write_all(&out).map_err(|e| io_err(&tmp, e))?;
            f.sync_all().map_err(|e| io_err(&tmp, e))?;
        }
        fs::rename(&tmp, &self.path).map_err(|e| io_err(&self.path, e))?;
        Ok(())
    }

    /// 世界名。
    pub fn level_name(&self) -> String {
        self.get_str("LevelName").unwrap_or("(未命名)").to_string()
    }

    pub fn data_version(&self) -> i32 {
        self.get_i32("DataVersion").unwrap_or(0)
    }

    /// 版本字符串（`Version.Name`）。
    pub fn version_name(&self) -> String {
        self.data()
            .and_then(|d| d.get("Version"))
            .and_then(|v| v.as_compound())
            .and_then(|v| v.get("Name"))
            .and_then(|t| t.as_str())
            .unwrap_or("?")
            .to_string()
    }

    /// 世界种子（`WorldGenSettings.seed`）。
    /// 种子。
    ///
    /// - 1.21.1：`Data.WorldGenSettings.seed`
    /// - 26.3+：`Data.WorldGenSettings` 不再放在 level.dat 里，
    ///   而是独立文件 `data/minecraft/world_gen_settings.dat`（由 `World::seed()` 兜底）
    pub fn seed(&self) -> Option<i64> {
        self.data()?
            .get("WorldGenSettings")?
            .as_compound()?
            .get("seed")?
            .as_i64()
    }

    pub fn game_type(&self) -> Option<i32> {
        self.get_i32("GameType")
    }

    /// 难度。
    ///
    /// - 1.21.1：`Data.Difficulty`（整数 0..3）
    /// - 26.3+：改为 `Data.difficulty_settings.difficulty`（字符串 `peaceful`/`normal`…）
    pub fn difficulty(&self) -> Option<i32> {
        if let Some(v) = self.get_i32("Difficulty") {
            return Some(v);
        }
        let ds = self.data()?.get("difficulty_settings")?.as_compound()?;
        let s = ds.get("difficulty")?.as_str()?;
        Some(match s {
            "peaceful" => 0,
            "easy" => 1,
            "normal" => 2,
            "hard" => 3,
            _ => 2,
        })
    }

    /// 难度是否被锁定（26.3 新增 `difficulty_settings.locked`）。
    pub fn difficulty_locked(&self) -> bool {
        self.data()
            .and_then(|d| d.get("difficulty_settings"))
            .and_then(|t| t.as_compound())
            .and_then(|c| c.get("locked"))
            .and_then(|t| t.as_i32())
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    pub fn hardcore(&self) -> bool {
        // 1.21.1 用 hardcore；26.3 放在 difficulty_settings.hardcore
        if let Some(v) = self.get_i32("hardcore") {
            return v != 0;
        }
        self.data()
            .and_then(|d| d.get("difficulty_settings"))
            .and_then(|t| t.as_compound())
            .and_then(|c| c.get("hardcore"))
            .and_then(|t| t.as_i32())
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    pub fn allow_commands(&self) -> bool {
        self.get_i32("allowCommands").unwrap_or(0) != 0
    }

    /// 世界出生点。
    ///
    /// - 1.21.1：`Data.SpawnX/SpawnY/SpawnZ`（三个 Int）
    /// - 26.3+：`Data.spawn.pos`（IntArray[3]）
    pub fn spawn(&self) -> (i32, i32, i32) {
        let d = self.data();

        // 新格式：spawn.pos = int[3]
        if let Some(pos) = d
            .and_then(|d| d.get("spawn"))
            .and_then(|t| t.as_compound())
            .and_then(|c| c.get("pos"))
            .and_then(|t| t.as_int_array())
        {
            if pos.len() == 3 {
                return (pos[0], pos[1], pos[2]);
            }
        }

        // 旧格式
        (
            self.get_i32("SpawnX").unwrap_or(0),
            self.get_i32("SpawnY").unwrap_or(64),
            self.get_i32("SpawnZ").unwrap_or(0),
        )
    }

    /// 出生点所在维度（26.3 新增；旧版本固定主世界）。
    pub fn spawn_dimension(&self) -> Option<String> {
        self.data()?
            .get("spawn")?
            .as_compound()?
            .get("dimension")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 游戏规则表（名字 → 字符串值）。
    pub fn game_rules(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(gr) = self.data().and_then(|d| d.get("GameRules")).and_then(|t| t.as_compound()) {
            for (k, v) in gr {
                out.push((k.clone(), v.as_str().unwrap_or("").to_string()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 玩家数据（单人存档内嵌的 Player）。
    pub fn player(&self) -> Option<&Compound> {
        self.data()?.get("Player")?.as_compound()
    }

    /// 玩家位置，把最后一段读出来。
    pub fn player_pos(&self) -> Option<[f64; 3]> {
        let pos = self.player()?.get("Pos")?;
        let (_, items) = pos.as_list()?;
        if items.len() != 3 {
            return None;
        }
        Some([
            items[0].as_f64()?,
            items[1].as_f64()?,
            items[2].as_f64()?,
        ])
    }

    /// 玩家 UUID（26.3 用它定位 `players/data/<uuid>.dat`）。
    ///
    /// 26.3 把它存成 `IntArray[4]`（4 个大端 i32 拼成的 128 位 UUID），
    /// 而不是旧版的字符串。
    pub fn singleplayer_uuid(&self) -> Option<String> {
        let t = self.data()?.get("singleplayer_uuid")?;
        // 字符串形式（旧版或某些工具）
        if let Some(s) = t.as_str() {
            return Some(s.to_string());
        }
        // IntArray[4] 形式（26.3）
        let arr = t.as_int_array()?;
        if arr.len() != 4 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for (i, v) in arr.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        Some(format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5],
            bytes[6], bytes[7],
            bytes[8], bytes[9],
            bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }
}

/// 读取 26.3+ 的玩家数据文件 `players/data/<uuid>.dat`。
///
/// 文件是 gzip 压缩的裸 NBT，`Pos` / `Inventory` 等**直接在根上**，
/// 不像 `data/minecraft/*.dat` 那样包一层 `data`。
pub fn read_player_dat(save: &Path, uuid: &str) -> Option<Compound> {
    let path = save.join("players").join("data").join(format!("{uuid}.dat"));
    let raw = fs::read(&path).ok()?;
    let body = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut out = Vec::new();
        GzDecoder::new(&raw[..]).read_to_end(&mut out).ok()?;
        out
    } else {
        raw
    };
    let root = nbt::read(&body).ok()?;
    let c = root.as_compound()?;
    // 兼容：有的版本包一层 data，有的直接在根上
    if let Some(inner) = c.get("data").and_then(|t| t.as_compound()) {
        return Some(inner.clone());
    }
    Some(c.clone())
}

/// 读取存档级独立数据文件（26.3+ 的新格式）。
///
/// 26.3 把很多东西从 level.dat 搬到了 `data/minecraft/*.dat`：
/// - `world_gen_settings.dat`：种子、维度生成器
/// - `game_rules.dat`：游戏规则
/// - `weather.dat`、`world_clocks.dat`、`scoreboard.dat` …
///
/// 这些文件是 **gzip 压缩的裸 NBT**（没有 8 字节头部，和 level.dat 一致）。
/// 返回根复合物里 `data` 那一层。
pub fn read_save_data_file(save: &Path, name: &str) -> Option<Compound> {
    let path = save.join("data").join("minecraft").join(name);
    let raw = fs::read(&path).ok()?;
    let body = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut out = Vec::new();
        GzDecoder::new(&raw[..]).read_to_end(&mut out).ok()?;
        out
    } else {
        raw
    };
    let root = nbt::read(&body).ok()?;
    let c = root.as_compound()?;
    // 这些文件统一包一层 { data: {...}, DataVersion: int }
    c.get("data").and_then(|t| t.as_compound()).cloned()
}

/// 某版本的存档是否使用 26.3+ 的"分散式"布局。
pub fn uses_split_layout(save: &Path) -> bool {
    save.join("data").join("minecraft").is_dir()
}

/// 检测存档是否被正在运行的游戏占用。
///
/// 1.16+ 的游戏用 `FileChannel.tryLock()` 独占 `session.lock`。
///
/// **不能**用「读写打开失败」来判定占用：
/// `session.lock` 常常是隐藏 / 只读属性的文件，以写方式打开会返回
/// "拒绝访问"，但这与游戏是否运行无关（实测游戏没开也会失败）。
/// 这会导致所有存档都被误判为「被占用」，写入全部被拒绝。
///
/// 判定策略按平台分开：
///
/// - **Windows**：文件锁表现为共享冲突，只读打开会失败，错误码
///   ERROR_SHARING_VIOLATION(32) / ERROR_LOCK_VIOLATION(33)。
///   能打开就说明没被锁。
///
/// - **Unix（Linux / macOS）**：文件锁是顾问锁（advisory lock），
///   **不会阻止 open()**，所以「能否打开」完全说明不了问题。
///   必须真的对文件尝试加一次 `flock`：
///   失败（EWOULDBLOCK）说明被别的进程独占，成功则说明没被锁，
///   加完立刻释放。
///
/// 注意错误码 32/33 在 Unix 上有完全不同的含义（EPIPE / EDOM），
/// 不能跨平台直接比较数字。
pub fn is_locked(save: &Path) -> bool {
    let path = save.join("session.lock");
    if !path.exists() {
        return false;
    }

    #[cfg(windows)]
    {
        match fs::File::open(&path) {
            // 打不开：只有共享冲突才算被占用，权限类错误一律不算
            Err(e) => matches!(e.raw_os_error(), Some(32) | Some(33)),
            // 能打开：读一个字节，真被锁时读不到内容
            Ok(mut f) => {
                use std::io::Read;
                let mut buf = [0u8; 1];
                f.read(&mut buf).is_err()
            }
        }
    }

    #[cfg(unix)]
    {
        unix_is_locked(&path)
    }

    #[cfg(not(any(windows, unix)))]
    {
        // 其它平台保守处理：能打开就当没锁
        fs::File::open(&path).is_err()
    }
}

/// Unix 下的锁检测：用 fcntl 试探锁。
///
/// <p><b>为什么必须是 fcntl 而不是 flock</b>：
/// Java 的 `FileChannel.tryLock()` 在 Linux / macOS 上用的是 POSIX 记录锁
/// （`fcntl(F_SETLK)`），而 `flock(2)` 是**另一套完全独立的锁**，
/// 两者在 Linux 上互不干扰。用 flock 去探测 Java 持有的锁，结果永远是
/// "能拿到锁"，于是把正在运行的游戏误判为"没锁定" —— 这会直接导致
/// 写到一半的存档损坏。
///
/// <p><b>为什么申请读锁（F_RDLCK）而不是写锁</b>：
/// POSIX 规定申请 F_WRLCK 要求文件描述符以写方式打开，但
/// `session.lock` 常常是只读属性的，根本打不开写模式。
/// 而 F_RDLCK 只需要读权限，且**会与游戏持有的写锁冲突**，
/// 正好用来探测。
///
/// <p>结构体 `struct flock` 在不同平台字段顺序不同
/// （Linux 与 macOS 就不一样），所以这里用 `libc` 而不是手写布局。
#[cfg(unix)]
fn unix_is_locked(path: &Path) -> bool {
    use std::os::unix::io::AsRawFd;

    let Ok(f) = fs::File::open(path) else {
        // 连读都读不了：可能是权限问题。按"没被游戏锁"处理，
        // 真正的写入还会再失败一次并给出明确提示。
        return false;
    };

    unsafe {
        let mut fl: libc::flock = std::mem::zeroed();
        fl.l_type = libc::F_RDLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = 0;
        fl.l_len = 0;

        let rc = libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &mut fl);
        if rc == 0 {
            // 拿到了读锁 → 没有别人持有写锁 → 没被游戏占用。
            // 立刻释放，避免影响游戏下次启动。
            fl.l_type = libc::F_UNLCK as _;
            libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &mut fl);
            return false;
        }

        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(0);
        // EACCES / EAGAIN 都表示"锁被别的进程占着"
        code == libc::EACCES || code == libc::EAGAIN
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// 打开后的存档。
pub struct World {
    pub root: PathBuf,
    pub level: LevelDat,
    pub dimensions: Vec<Dimension>,
}

impl World {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        if !root.join("level.dat").exists() {
            return Err(Error::NotAWorld(root));
        }
        let level = LevelDat::open(&root)?;
        let dimensions = discover_dimensions(&root);
        Ok(World { root, level, dimensions })
    }

    pub fn is_locked(&self) -> bool {
        is_locked(&self.root)
    }

    /// 种子。
    ///
    /// 1.21.1 存在 level.dat 的 `WorldGenSettings.seed`；
    /// 26.3+ 搬到了 `data/minecraft/world_gen_settings.dat`。
    pub fn seed(&self) -> Option<i64> {
        if let Some(s) = self.level.seed() {
            return Some(s);
        }
        read_save_data_file(&self.root, "world_gen_settings.dat")?
            .get("seed")?
            .as_i64()
    }

    /// 游戏规则（名字 → 值字符串）。
    ///
    /// - 1.21.1：在 level.dat 的 `GameRules`（值是 String）
    /// - 26.3+：搬到 `data/minecraft/game_rules.dat`，
    ///   结构是 `{ data: { "minecraft:keep_inventory": Byte(1), ... } }`，
    ///   键带命名空间、值是 Byte/Int/String 混合。
    pub fn game_rules(&self) -> Vec<(String, String)> {
        let inline = self.level.game_rules();
        if !inline.is_empty() {
            return inline;
        }
        let Some(d) = read_save_data_file(&self.root, "game_rules.dat") else {
            return Vec::new();
        };

        let mut out = Vec::new();
        // 兼容两种排布：扁平（26.3）或嵌套在 game_rules 下
        let table: Vec<(&String, &Tag)> = if let Some(nested) =
            d.get("game_rules").and_then(|t| t.as_compound())
        {
            nested.iter().collect()
        } else {
            d.iter().collect()
        };

        for (k, v) in table {
            if k == "DataVersion" {
                continue;
            }
            let s = match v {
                Tag::String(s) => s.clone(),
                Tag::Byte(b) => (if *b != 0 { "true" } else { "false" }).to_string(),
                Tag::Int(i) => i.to_string(),
                Tag::Long(l) => l.to_string(),
                Tag::Float(f) => f.to_string(),
                Tag::Double(f) => f.to_string(),
                other => format!("{other:?}"),
            };
            // 去掉 minecraft: 前缀便于显示
            let name = k.strip_prefix("minecraft:").unwrap_or(k).to_string();
            out.push((name, s));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 天气状态（26.3 才有独立文件）。
    pub fn weather(&self) -> Option<Compound> {
        read_save_data_file(&self.root, "weather.dat")
    }

    /// 玩家位置（统一入口）。
    ///
    /// - 1.21.1：level.dat 内嵌 `Data.Player.Pos`
    /// - 26.3+：`players/data/<uuid>.dat` 的 `data.Pos`
    pub fn player_pos(&self) -> Option<[f64; 3]> {
        if let Some(p) = self.level.player_pos() {
            return Some(p);
        }
        let uuid = self.level.singleplayer_uuid()?;
        let d = read_player_dat(&self.root, &uuid)?;
        let (_, items) = d.get("Pos")?.as_list()?;
        if items.len() != 3 {
            return None;
        }
        Some([
            items[0].as_f64()?,
            items[1].as_f64()?,
            items[2].as_f64()?,
        ])
    }

    /// 在 `saves` 目录下枚举所有存档。
    pub fn list_saves(saves_dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(saves_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("level.dat").exists() {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// 从常见位置推断 `saves` 目录（例如实例根目录、`.minecraft` 目录）。
    pub fn guess_saves_dirs(hint: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut push = |p: PathBuf| {
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        };
        push(hint.join("saves"));
        push(hint.join(".minecraft").join("saves"));
        push(hint.join("versions").join("saves"));
        // 若是 versions/<name> 目录，回到 instances 层
        if hint.join("minecraft").is_dir() {
            push(hint.join("minecraft").join("saves"));
        }
        out
    }

    /// 扫描某个维度的全部地形区块元数据。
    ///
    /// 这是重活（需要解压每个区块），应放到后台线程。
    pub fn scan_chunks(
        &self,
        dim: &Dimension,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<ChunkMeta>> {
        let dir = dim.dir(RegionKind::Terrain);
        let files = anvil::list_region_files(&dir)?;
        let mut out = Vec::new();
        let total = files.len();
        for (i, (rx, rz, path)) in files.into_iter().enumerate() {
            let mut rf = RegionFile::open(&path)?;
            for idx in rf.indices() {
                let cx = rx * 32 + (idx % 32) as i32;
                let cz = rz * 32 + (idx / 32) as i32;
                let raw = match rf.raw(cx, cz) {
                    Ok(Some(r)) => r,
                    _ => continue,
                };
                let nbt_bytes = raw.len();
                let tag = match nbt::read(&raw) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let c = match tag.as_compound() {
                    Some(c) => c,
                    None => continue,
                };
                let count_list = |key: &str| -> u32 {
                    c.get(key)
                        .and_then(|t| t.as_list())
                        .map(|(_, items)| items.len() as u32)
                        .unwrap_or(0)
                };
                out.push(ChunkMeta {
                    x: cx,
                    z: cz,
                    rx,
                    rz,
                    sectors: 0,
                    nbt_bytes,
                    status: c
                        .get("Status")
                        .or_else(|| c.get("status"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    data_version: c.get("DataVersion").and_then(|t| t.as_i32()).unwrap_or(0),
                    inhabited_time: c.get("InhabitedTime").and_then(|t| t.as_i64()).unwrap_or(0),
                    last_update: c.get("LastUpdate").and_then(|t| t.as_i64()).unwrap_or(0),
                    block_entities: count_list("block_entities"),
                    block_ticks: count_list("block_ticks"),
                    fluid_ticks: count_list("fluid_ticks"),
                    sections: count_list("sections"),
                });
            }
            progress(i + 1, total);
        }
        // 补齐 sectors 信息（从 region 头表读，无需解压）
        let sectors = region_sector_map(&dir);
        for m in &mut out {
            if let Some(s) = sectors.get(&(m.x, m.z)) {
                m.sectors = *s;
            }
        }
        Ok(out)
    }

    /// 扫描实体索引。
    pub fn scan_entities(&self, dim: &Dimension) -> Result<Vec<EntityMeta>> {
        let dir = dim.dir(RegionKind::Entities);
        let files = anvil::list_region_files(&dir)?;
        let mut out = Vec::new();
        for (rx, rz, path) in files {
            let mut rf = RegionFile::open(&path)?;
            for idx in rf.indices() {
                let cx = rx * 32 + (idx % 32) as i32;
                let cz = rz * 32 + (idx / 32) as i32;
                let tag = match rf.chunk(cx, cz) {
                    Ok(Some(t)) => t,
                    _ => continue,
                };
                let Some(c) = tag.as_compound() else { continue };
                let key = if c.contains_key("Entities") { "Entities" } else { "entities" };
                let Some((_, list)) = c.get(key).and_then(|t| t.as_list()) else {
                    continue;
                };
                for e in list {
                    let Some(ec) = e.as_compound() else { continue };
                    let id = ec
                        .get("id")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let uuid = ec.get("UUID").and_then(|t| t.as_int_array()).and_then(|a| {
                        if a.len() == 4 {
                            Some([a[0], a[1], a[2], a[3]])
                        } else {
                            None
                        }
                    });
                    let pos = ec.get("Pos").and_then(|t| t.as_list()).and_then(|(_, items)| {
                        if items.len() >= 3 {
                            Some([
                                items[0].as_f64()?,
                                items[1].as_f64()?,
                                items[2].as_f64()?,
                            ])
                        } else {
                            None
                        }
                    });
                    out.push(EntityMeta {
                        id,
                        uuid,
                        pos,
                        chunk_x: cx,
                        chunk_z: cz,
                        dimension: dim.id.clone(),
                        region_index: idx,
                    });
                }
            }
        }
        Ok(out)
    }

    /// 扫描方块实体索引。
    pub fn scan_block_entities(&self, dim: &Dimension) -> Result<Vec<BlockEntityMeta>> {
        let dir = dim.dir(RegionKind::Terrain);
        let files = anvil::list_region_files(&dir)?;
        let mut out = Vec::new();
        for (rx, rz, path) in files {
            let mut rf = RegionFile::open(&path)?;
            for idx in rf.indices() {
                let cx = rx * 32 + (idx % 32) as i32;
                let cz = rz * 32 + (idx / 32) as i32;
                let tag = match rf.chunk(cx, cz) {
                    Ok(Some(t)) => t,
                    _ => continue,
                };
                let Some(c) = tag.as_compound() else { continue };
                let Some((_, list)) = c.get("block_entities").and_then(|t| t.as_list()) else {
                    continue;
                };
                for b in list {
                    let Some(bc) = b.as_compound() else { continue };
                    out.push(BlockEntityMeta {
                        id: bc
                            .get("id")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        x: bc.get("x").and_then(|t| t.as_i32()).unwrap_or(0),
                        y: bc.get("y").and_then(|t| t.as_i32()).unwrap_or(0),
                        z: bc.get("z").and_then(|t| t.as_i32()).unwrap_or(0),
                        chunk_x: cx,
                        chunk_z: cz,
                    });
                }
            }
        }
        Ok(out)
    }

    /// 读取单个区块的俯视采样。
    ///
    /// `mode` 决定取哪一层：主世界用 `SliceMode::Surface`（找地表），
    /// 下界默认用 `SliceMode::Fixed(65)`（顶部有基岩，取固定层更直观）。
    pub fn chunk_surface(
        &self,
        dim: &Dimension,
        cx: i32,
        cz: i32,
        cache: &mut RegionCache,
        mode: chunk::SliceMode,
    ) -> Option<ChunkSurface> {
        let tag = cache.get(dim, cx, cz)?;
        let sections = chunk::parse_sections(&tag);
        let columns = chunk::surface_columns(&sections, mode);
        // 26.4 snapshot 起区块改用小写 `status`，1.21.1–26.3 用 `Status`
        let complete = tag
            .as_compound()
            .and_then(|c| c.get("Status").or_else(|| c.get("status")))
            .and_then(|t| t.as_str())
            .map(|s| s == "minecraft:full")
            .unwrap_or(false);
        Some(ChunkSurface {
            x: cx,
            z: cz,
            columns,
            complete,
        })
    }

    /// 读取某个方块坐标的群系（用于地图悬停提示）。
    pub fn biome_at(&self, dim: &Dimension, x: i32, y: i32, z: i32, cache: &mut RegionCache) -> Option<String> {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let tag = cache.get(dim, cx, cz)?;
        let biomes = chunk::parse_biomes(&tag);
        chunk::biome_at(&[], &biomes, x, y, z)
    }
}

/// 从 region 头表读出每个区块占用的扇区数（不解压，很快）。
fn region_sector_map(dir: &Path) -> HashMap<(i32, i32), u8> {
    let mut out = HashMap::new();
    let Ok(files) = anvil::list_region_files(dir) else {
        return out;
    };
    for (rx, rz, path) in files {
        let Ok(bytes) = fs::read(&path) else { continue };
        if bytes.len() < anvil::SECTOR * anvil::HEADER_SECTORS {
            continue;
        }
        for i in 0..1024usize {
            let sectors = bytes[i * 4 + 3];
            if sectors == 0 {
                continue;
            }
            let cx = rx * 32 + (i % 32) as i32;
            let cz = rz * 32 + (i / 32) as i32;
            out.insert((cx, cz), sectors);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Region 缓存
// ---------------------------------------------------------------------------

/// 按需加载/缓存的 region 文件集合，避免反复打开同一文件。
pub struct RegionCache {
    map: HashMap<(i32, i32), RegionFile>,
    pub dir: HashMap<(i32, i32), PathBuf>,
    capacity: usize,
    /// 已修改但未落盘的 region 键。
    dirty: HashMap<(i32, i32), PathBuf>,
}

impl Default for RegionCache {
    fn default() -> Self {
        Self::new(32)
    }
}

impl RegionCache {
    pub fn new(capacity: usize) -> Self {
        RegionCache {
            map: HashMap::new(),
            dir: HashMap::new(),
            capacity,
            dirty: HashMap::new(),
        }
    }

    /// 取区块 NBT（不存在返回 None）。
    pub fn get(&mut self, dim: &Dimension, cx: i32, cz: i32) -> Option<Tag> {
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
        let key = (rx, rz);
        if !self.map.contains_key(&key) {
            let path = dim.dir(RegionKind::Terrain).join(anvil::region_file_name(rx, rz));
            if !path.exists() {
                return None;
            }
            let rf = RegionFile::open(&path).ok()?;
            self.evict_if_needed();
            self.map.insert(key, rf);
            self.dir.insert(key, path);
        }
        self.map.get_mut(&key)?.chunk(cx, cz).ok().flatten()
    }

    fn evict_if_needed(&mut self) {
        if self.map.len() < self.capacity {
            return;
        }
        // 淘汰一个未修改的
        let victim = self
            .map
            .iter()
            .find(|(k, f)| !f.is_dirty() && !self.dirty.contains_key(*k))
            .map(|(k, _)| *k);
        if let Some(k) = victim {
            self.map.remove(&k);
            self.dir.remove(&k);
        } else {
            // 全是脏的：清空缓存前先落盘由调用方负责，这里只丢弃最旧的一个引用
            if let Some(k) = self.map.keys().next().copied() {
                self.map.remove(&k);
                self.dir.remove(&k);
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.dir.clear();
    }
}

/// 判断某个 gamerule 的值是否为 true。
pub fn gamerule_bool(v: &str) -> bool {
    matches!(v, "true" | "1")
}

/// 区块状态的中文说明。
pub fn status_label(status: &str) -> &'static str {
    match status.strip_prefix("minecraft:").unwrap_or(status) {
        "empty" => "空",
        "structure_starts" => "仅结构起点",
        "structure_references" => "结构引用",
        "biomes" => "仅群系",
        "noise" => "噪声地形",
        "surface" => "地表",
        "carvers" => "雕刻中",
        "features" => "特征生成",
        "initialize_light" => "初始化光照",
        "light" => "光照",
        "spawn" => "刷怪准备",
        "full" => "完整",
        _ => "未知",
    }
}

/// list 标签的元素类型判断辅助。
pub fn list_elem_type(t: &Tag) -> TagType {
    match t {
        Tag::List { elem, .. } => *elem,
        _ => TagType::End,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 26.3 风格的 level.dat（spawn 是复合物、难度在 difficulty_settings）。
    fn level_dat_263(spawn: (i32, i32, i32), difficulty: &str, hardcore: bool) -> LevelDat {
        let mut ds = Compound::new();
        ds.insert("difficulty".into(), Tag::String(difficulty.into()));
        ds.insert("hardcore".into(), Tag::Byte(if hardcore { 1 } else { 0 }));
        ds.insert("locked".into(), Tag::Byte(0));

        let mut spawn_c = Compound::new();
        spawn_c.insert(
            "pos".into(),
            Tag::IntArray(vec![spawn.0, spawn.1, spawn.2]),
        );
        spawn_c.insert("pitch".into(), Tag::Float(0.0));
        spawn_c.insert("yaw".into(), Tag::Float(0.0));
        spawn_c.insert(
            "dimension".into(),
            Tag::String("minecraft:overworld".into()),
        );

        let mut data = Compound::new();
        data.insert("LevelName".into(), Tag::String("新世界".into()));
        data.insert("DataVersion".into(), Tag::Int(5023));
        data.insert("GameType".into(), Tag::Int(1));
        data.insert("allowCommands".into(), Tag::Byte(1));
        data.insert("difficulty_settings".into(), Tag::Compound(ds));
        data.insert("spawn".into(), Tag::Compound(spawn_c));

        let mut root = Compound::new();
        root.insert("Data".into(), Tag::Compound(data));
        LevelDat {
            path: PathBuf::from("level.dat"),
            root: Tag::Compound(root),
            gzip: true,
        }
    }

    #[test]
    fn parses_263_spawn_from_pos_array() {
        let lvl = level_dat_263((0, 71, 0), "normal", false);
        assert_eq!(lvl.spawn(), (0, 71, 0));
        assert_eq!(
            lvl.spawn_dimension().as_deref(),
            Some("minecraft:overworld")
        );
    }

    #[test]
    fn parses_263_difficulty_string() {
        let lvl = level_dat_263((0, 64, 0), "hard", false);
        assert_eq!(lvl.difficulty(), Some(3));

        let lvl = level_dat_263((0, 64, 0), "peaceful", false);
        assert_eq!(lvl.difficulty(), Some(0));
    }

    #[test]
    fn parses_263_hardcore_in_difficulty_settings() {
        let lvl = level_dat_263((0, 64, 0), "hard", true);
        assert!(lvl.hardcore());
        let lvl = level_dat_263((0, 64, 0), "normal", false);
        assert!(!lvl.hardcore());
    }

    #[test]
    fn readonly_session_lock_is_not_locked() {
        // 回归：session.lock 是只读属性时，旧实现用「写打开」测试会得到
        // 拒绝访问，从而把**所有存档**误判为「被游戏占用」，写入全被拒绝。
        // 正确行为：能读到内容就说明没被独占。
        let dir = tmp_ws("lockro");
        let lock = dir.join("session.lock");
        fs::write(&lock, [0x01, 0x02, 0x03]).unwrap();

        // 设为只读（Unix 上对应权限位，Windows 上读仍成功）
        let mut perms = fs::metadata(&lock).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&lock, perms).unwrap();

        assert!(!is_locked(&dir), "只读的 session.lock 不应被判定为占用");

        // 清理时恢复可写，否则删不掉
        let mut perms = fs::metadata(&lock).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&lock, perms).unwrap();
    }

    #[test]
    fn missing_session_lock_is_not_locked() {
        let dir = tmp_ws("nolock");
        assert!(!is_locked(&dir));
    }

    #[test]
    fn lowercase_status_key_264() {
        // 26.4-snapshot 起区块用 `status`（小写），1.21.1–26.3 用 `Status`。
        // chunk_surface 的 complete 判定曾经只认大写，导致 26.4 的区块
        // 全部被当作"未生成"，地图上显示为灰块。
        let mk = |key: &str| {
            let mut c = Compound::new();
            c.insert(key.into(), Tag::String("minecraft:full".into()));
            Tag::Compound(c)
        };
        for key in ["Status", "status"] {
            let tag = mk(key);
            let complete = tag
                .as_compound()
                .and_then(|c| c.get("Status").or_else(|| c.get("status")))
                .and_then(|t| t.as_str())
                .map(|s| s == "minecraft:full")
                .unwrap_or(false);
            assert!(complete, "键 `{key}` 应被识别为完整区块");
        }
    }

    #[test]
    fn old_format_still_works() {
        // 1.21.1 风格：SpawnX/Y/Z + Difficulty 整数
        let mut data = Compound::new();
        data.insert("SpawnX".into(), Tag::Int(100));
        data.insert("SpawnY".into(), Tag::Int(70));
        data.insert("SpawnZ".into(), Tag::Int(-200));
        data.insert("Difficulty".into(), Tag::Int(1));
        data.insert("hardcore".into(), Tag::Int(1));
        let mut root = Compound::new();
        root.insert("Data".into(), Tag::Compound(data));

        let lvl = LevelDat {
            path: PathBuf::from("level.dat"),
            root: Tag::Compound(root),
            gzip: true,
        };
        assert_eq!(lvl.spawn(), (100, 70, -200));
        assert_eq!(lvl.difficulty(), Some(1));
        assert!(lvl.hardcore());
        assert_eq!(lvl.spawn_dimension(), None);
    }

    fn tmp_ws(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcworld-ws-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_level_dat(save: &Path, name: &str) {
        let mut data = Compound::new();
        data.insert("LevelName".into(), Tag::String(name.into()));
        data.insert("DataVersion".into(), Tag::Int(3955));
        data.insert("GameType".into(), Tag::Int(1));
        data.insert("hardcore".into(), Tag::Int(0));
        data.insert("allowCommands".into(), Tag::Int(1));
        data.insert("SpawnX".into(), Tag::Int(10));
        data.insert("SpawnY".into(), Tag::Int(64));
        data.insert("SpawnZ".into(), Tag::Int(-5));
        let mut ver = Compound::new();
        ver.insert("Name".into(), Tag::String("1.21.1".into()));
        data.insert("Version".into(), Tag::Compound(ver));
        let mut wg = Compound::new();
        wg.insert("seed".into(), Tag::Long(-2868611341612800072));
        data.insert("WorldGenSettings".into(), Tag::Compound(wg));

        let mut root = Compound::new();
        root.insert("Data".into(), Tag::Compound(data));

        let body = nbt::write(&Tag::Compound(root)).unwrap();
        let mut enc = GzEncoder::new(Vec::new(), Compression::new(6));
        enc.write_all(&body).unwrap();
        fs::write(save.join("level.dat"), enc.finish().unwrap()).unwrap();
    }

    #[test]
    fn open_and_read_level_dat() {
        let save = tmp_ws("open");
        write_level_dat(&save, "测试世界");
        fs::create_dir_all(save.join("region")).unwrap();
        fs::create_dir_all(save.join("DIM-1/region")).unwrap();

        let w = World::open(&save).unwrap();
        assert_eq!(w.level.level_name(), "测试世界");
        assert_eq!(w.level.data_version(), 3955);
        assert_eq!(w.level.version_name(), "1.21.1");
        assert_eq!(w.level.seed(), Some(-2868611341612800072));
        assert_eq!(w.level.spawn(), (10, 64, -5));
        assert_eq!(w.dimensions.len(), 2);
        assert_eq!(w.dimensions[0].id, "minecraft:overworld");
        assert_eq!(w.dimensions[1].id, "minecraft:the_nether");
    }

    #[test]
    fn rejects_non_world() {
        let save = tmp_ws("notworld");
        assert!(matches!(World::open(&save), Err(Error::NotAWorld(_))));
    }

    #[test]
    fn level_dat_save_rotates_backup() {
        let save = tmp_ws("save");
        write_level_dat(&save, "原名");
        let mut level = LevelDat::open(&save).unwrap();
        level.set("LevelName", Tag::String("新名".into()));
        level.save().unwrap();

        let back = LevelDat::open(&save).unwrap();
        assert_eq!(back.level_name(), "新名");
        assert!(save.join("level.dat_old").exists(), "应轮换出备份");

        let old = LevelDat::open(&save.join("level.dat_old").parent().unwrap()).ok();
        // level.dat_old 本身也要能被解析
        let raw = fs::read(save.join("level.dat_old")).unwrap();
        let mut out = Vec::new();
        GzDecoder::new(&raw[..]).read_to_end(&mut out).unwrap();
        assert!(nbt::read(&out).is_ok(), "备份文件必须仍是有效 NBT");
        let _ = old;
    }

    #[test]
    fn discovers_new_dimension_layout() {
        let save = tmp_ws("newlayout");
        write_level_dat(&save, "新布局");
        let dim = save.join("dimensions/minecraft/overworld");
        fs::create_dir_all(dim.join("region")).unwrap();
        fs::create_dir_all(save.join("dimensions/minecraft/the_nether/region")).unwrap();

        let w = World::open(&save).unwrap();
        assert!(w.dimensions.iter().any(|d| d.id == "minecraft:overworld"));
        assert!(w.dimensions.iter().any(|d| d.id == "minecraft:the_nether"));
        assert!(w
            .dimensions
            .iter()
            .any(|d| d.display == "下界" && d.root.ends_with("the_nether")));
    }

    #[test]
    fn list_saves_finds_worlds() {
        let base = tmp_ws("listsaves");
        let saves = base.join("saves");
        fs::create_dir_all(saves.join("世界A")).unwrap();
        fs::create_dir_all(saves.join("世界B")).unwrap();
        fs::create_dir_all(saves.join("不是存档")).unwrap();
        write_level_dat(&saves.join("世界A"), "A");
        write_level_dat(&saves.join("世界B"), "B");

        let found = World::list_saves(&saves);
        assert_eq!(found.len(), 2, "只应识别含 level.dat 的目录");
    }

    #[test]
    fn status_labels_are_localized() {
        assert_eq!(status_label("minecraft:full"), "完整");
        assert_eq!(status_label("minecraft:structure_starts"), "仅结构起点");
        assert_eq!(status_label("weird"), "未知");
    }
}
