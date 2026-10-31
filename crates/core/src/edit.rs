//! 编辑操作：实体删除、批量清理、区块复制粘贴、结构导出。
//!
//! 所有函数都只改传入的 region 文件对象，落盘由调用方通过
//! [`crate::safety::ChunkEditor::commit`] 统一提交，便于整体回滚。

use std::collections::{HashMap, HashSet};

use crate::anvil::{self, RegionFile};
use crate::nbt::{Compound, Tag, TagType};
use crate::world::{Dimension, RegionKind};

/// 实体筛选条件。
#[derive(Clone, Debug, Default)]
pub struct EntityFilter {
    /// 中心点（方块坐标）。None 表示不限位置。
    pub center: Option<(f64, f64, f64)>,
    /// 球形半径（方块）。仅在 center 存在时生效。
    pub radius: f64,
    /// 只处理这些区块（区块坐标）。空表示不限。
    pub chunks: HashSet<(i32, i32)>,
    /// 只删这些类型（空表示不限）。
    pub include_types: HashSet<String>,
    /// 永远不删这些类型。
    pub exclude_types: HashSet<String>,
    /// 只删 NBT 中某个字段等于给定值的实体（如 `Item.id` = `minecraft:chest`）。
    pub nbt_match: Option<(String, String)>,
}

impl EntityFilter {
    /// 以某个点为中心的球形筛选。
    pub fn around(x: f64, y: f64, z: f64, radius: f64) -> Self {
        EntityFilter {
            center: Some((x, y, z)),
            radius,
            ..Default::default()
        }
    }

    pub fn with_types(mut self, types: &[&str]) -> Self {
        for t in types {
            self.include_types.insert(normalize_id(t));
        }
        self
    }

    pub fn excluding(mut self, types: &[&str]) -> Self {
        for t in types {
            self.exclude_types.insert(normalize_id(t));
        }
        self
    }

    /// 判断某实体是否命中。
    pub fn matches(&self, entity: &Compound) -> bool {
        let id = entity
            .get("id")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");

        if !self.include_types.is_empty() && !self.include_types.contains(id) {
            return false;
        }
        if self.exclude_types.contains(id) {
            return false;
        }

        if let Some((cx, cy, cz)) = self.center {
            let Some(pos) = entity_pos(entity) else {
                return false;
            };
            let d2 = (pos[0] - cx).powi(2) + (pos[1] - cy).powi(2) + (pos[2] - cz).powi(2);
            if d2 > self.radius * self.radius {
                return false;
            }
        }

        if let Some((path, expected)) = &self.nbt_match {
            if lookup_path(entity, path).as_deref() != Some(expected.as_str()) {
                return false;
            }
        }

        true
    }
}

/// 统一资源位置写法（补 `minecraft:` 前缀）。
pub fn normalize_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// 读取实体坐标。
pub fn entity_pos(e: &Compound) -> Option<[f64; 3]> {
    let (_, items) = e.get("Pos")?.as_list()?;
    if items.len() < 3 {
        return None;
    }
    Some([items[0].as_f64()?, items[1].as_f64()?, items[2].as_f64()?])
}

/// 按 `a.b.c` 路径取 NBT 值并转成字符串。
pub fn lookup_path(root: &Compound, path: &str) -> Option<String> {
    let mut cur: &Tag = &Tag::Compound(root.clone());
    for seg in path.split('.') {
        cur = match cur {
            Tag::Compound(c) => c.get(seg)?,
            Tag::List { items, .. } => {
                let i: usize = seg.parse().ok()?;
                items.get(i)?
            }
            _ => return None,
        };
    }
    Some(match cur {
        Tag::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// 删除实体的结果统计。
#[derive(Clone, Debug, Default)]
pub struct DeleteStats {
    pub removed: usize,
    pub scanned: usize,
    /// 按类型统计的删除数。
    pub by_type: HashMap<String, usize>,
    /// 被清空（变为无实体）的区块数。
    pub chunks_emptied: usize,
    /// 改动的 region 文件路径。
    pub touched_files: Vec<std::path::PathBuf>,
}

impl DeleteStats {
    pub fn merge(&mut self, other: DeleteStats) {
        self.removed += other.removed;
        self.scanned += other.scanned;
        self.chunks_emptied += other.chunks_emptied;
        for (k, v) in other.by_type {
            *self.by_type.entry(k).or_default() += v;
        }
        for f in other.touched_files {
            if !self.touched_files.contains(&f) {
                self.touched_files.push(f);
            }
        }
    }

    /// 删除数按类型降序排列。
    pub fn top_types(&self, n: usize) -> Vec<(String, usize)> {
        let mut v: Vec<_> = self.by_type.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }
}

/// 在内存中的实体 region 上执行删除。
///
/// 返回统计信息；调用方需把 `touched_files` 交给 `Session::touch` 备份后落盘。
pub fn delete_entities(
    dim: &Dimension,
    filter: &EntityFilter,
) -> crate::safety::Result<DeleteStats> {
    let mut stats = DeleteStats::default();
    let dir = dim.dir(RegionKind::Entities);
    let files = anvil::list_region_files(&dir)?;

    for (rx, rz, path) in files {
        let mut rf = RegionFile::open(&path)?;
        let mut changed = false;

        for idx in rf.indices() {
            let cx = rx * 32 + (idx % 32) as i32;
            let cz = rz * 32 + (idx / 32) as i32;
            // 指定了区块范围时，跳过不在范围内的区块（省去解压整块的开销）
            if !filter.chunks.is_empty() && !filter.chunks.contains(&(cx, cz)) {
                continue;
            }
            let Some(tag) = rf.chunk(cx, cz)? else { continue };
            let mut tag = tag;
            let Some(c) = tag.as_compound_mut() else { continue };
            // 1.17+ 用 Entities；proto-chunk 里可能是 entities
            let key = if c.contains_key("Entities") {
                "Entities"
            } else if c.contains_key("entities") {
                "entities"
            } else {
                stats.scanned += 1;
                continue;
            };

            let Some((elem, items)) = c.get_mut(key).and_then(|t| t.as_list_mut()) else {
                continue;
            };
            let elem = *elem;
            let before = items.len();
            stats.scanned += before;

            let mut kept: Vec<Tag> = Vec::with_capacity(items.len());
            for item in items.drain(..) {
                let hit = item
                    .as_compound()
                    .map(|ec| filter.matches(ec))
                    .unwrap_or(false);
                if hit {
                    let ty = item
                        .as_compound()
                        .and_then(|c| c.get("id"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    *stats.by_type.entry(ty).or_default() += 1;
                    stats.removed += 1;
                } else {
                    kept.push(item);
                }
            }
            let after = kept.len();
            *items = kept;

            if after != before {
                changed = true;
                if after == 0 {
                    stats.chunks_emptied += 1;
                }
                // 空列表必须保留元素类型，否则游戏读到 End 类型的列表会出错
                if let Some(Tag::List { elem: e, .. }) = c.get_mut(key) {
                    if *e == TagType::End {
                        *e = elem;
                    }
                }
                rf.put_chunk(cx, cz, &tag)?;
            }
        }

        if changed {
            rf.save()?;
            stats.touched_files.push(path);
        }
    }
    Ok(stats)
}

/// 统计各类型的实体数量（用于实体管理面板）。
pub fn entity_type_counts(
    dim: &Dimension,
) -> crate::safety::Result<HashMap<String, usize>> {
    let mut out = HashMap::new();
    let dir = dim.dir(RegionKind::Entities);
    for (rx, rz, path) in anvil::list_region_files(&dir)? {
        let mut rf = RegionFile::open(&path)?;
        for idx in rf.indices() {
            let (cx, cz) = anvil::abs_chunk(rx, rz, idx % 32, idx / 32);
            let Some(tag) = rf.chunk(cx, cz)? else { continue };
            let Some(c) = tag.as_compound() else { continue };
            let key = if c.contains_key("Entities") { "Entities" } else { "entities" };
            let Some((_, items)) = c.get(key).and_then(|t| t.as_list()) else {
                continue;
            };
            for e in items {
                if let Some(id) = e.as_compound().and_then(|c| c.get("id")).and_then(|t| t.as_str()) {
                    *out.entry(id.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    Ok(out)
}

/// 预设的批量清理规则。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CleanupPreset {
    /// 掉落物（item）
    Items,
    /// 经验球
    XpOrbs,
    /// 卡住的掉落方块（falling_block）
    FallingBlocks,
    /// 点燃的 TNT
    Tnt,
    /// 箭与投掷物
    Projectiles,
    /// 所有非生物实体（掉落物/箭/矿车等）
    NonLiving,
}

impl CleanupPreset {
    pub fn label(self) -> &'static str {
        match self {
            CleanupPreset::Items => "掉落物",
            CleanupPreset::XpOrbs => "经验球",
            CleanupPreset::FallingBlocks => "掉落方块",
            CleanupPreset::Tnt => "点燃的 TNT",
            CleanupPreset::Projectiles => "箭与投掷物",
            CleanupPreset::NonLiving => "全部非生物实体",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            CleanupPreset::Items => "清除掉在地上的物品实体（最常见的卡顿源）",
            CleanupPreset::XpOrbs => "清除经验球",
            CleanupPreset::FallingBlocks => "清除卡在空中的沙砾/沙，避免持续 tick",
            CleanupPreset::Tnt => "清除已点燃的 TNT，防止事后爆炸",
            CleanupPreset::Projectiles => "清除箭、雪球、末影珍珠等飞行物",
            CleanupPreset::NonLiving => "清除除生物以外的所有实体，可能包含重要展示实体",
        }
    }

    /// 转成筛选条件。
    pub fn to_filter(self) -> EntityFilter {
        let f = EntityFilter::default();
        match self {
            CleanupPreset::Items => f.with_types(&["item"]),
            CleanupPreset::XpOrbs => f.with_types(&["experience_orb"]),
            CleanupPreset::FallingBlocks => f.with_types(&["falling_block"]),
            CleanupPreset::Tnt => f.with_types(&["tnt"]),
            CleanupPreset::Projectiles => f.with_types(&[
                "arrow",
                "spectral_arrow",
                "snowball",
                "egg",
                "ender_pearl",
                "eye_of_ender",
                "experience_bottle",
                "splash_potion",
                "lingering_potion",
                "trident",
                "fireball",
                "small_fireball",
                "dragon_fireball",
                "wither_skull",
                "shulker_bullet",
                "llama_spit",
                "firework_rocket",
                "fishing_bobber",
                "wind_charge",
                "breeze_wind_charge",
            ]),
            CleanupPreset::NonLiving => {
                // 用"排除生物"的方式更稳妥：列出常见生物反向排除
                f.excluding(&[
                    "player", "villager", "wandering_trader", "iron_golem", "snow_golem",
                    "armor_stand", "item_frame", "glow_item_frame", "painting",
                    "sheep", "cow", "pig", "chicken", "horse", "donkey", "mule",
                    "llama", "trader_llama", "wolf", "cat", "ocelot", "fox", "rabbit",
                    "parrot", "bat", "bee", "axolotl", "turtle", "dolphin", "squid",
                    "glow_squid", "cod", "salmon", "tropical_fish", "pufferfish",
                    "tadpole", "frog", "allay", "goat", "camel", "sniffer", "armadillo",
                    "zombie", "skeleton", "creeper", "spider", "cave_spider", "enderman",
                    "endermite", "silverfish", "witch", "pillager", "vindicator",
                    "evoker", "illusioner", "ravager", "vex", "blaze", "ghast",
                    "magma_cube", "slime", "wither_skeleton", "wither", "ender_dragon",
                    "shulker", "guardian", "elder_guardian", "drowned", "husk",
                    "stray", "phantom", "pillager", "zoglin", "hoglin", "piglin",
                    "piglin_brute", "zombified_piglin", "strider", "warden",
                ])
            }
        }
    }
}

/// 所有预设（用于 UI 列表）。
pub const ALL_PRESETS: &[CleanupPreset] = &[
    CleanupPreset::Items,
    CleanupPreset::XpOrbs,
    CleanupPreset::FallingBlocks,
    CleanupPreset::Tnt,
    CleanupPreset::Projectiles,
    CleanupPreset::NonLiving,
];

// ---------------------------------------------------------------------------
// 区块复制 / 粘贴
// ---------------------------------------------------------------------------

/// 复制出来的一个区块（保留原始 NBT，粘贴时改写坐标）。
#[derive(Clone, Debug)]
pub struct CopiedChunk {
    /// 相对选区左上角的偏移（区块单位）。
    pub dx: i32,
    pub dz: i32,
    pub tag: Tag,
}

/// 一个区块选区剪贴板。
#[derive(Clone, Debug, Default)]
pub struct ChunkClipboard {
    /// 源维度 id，便于提示"跨维度粘贴"。
    pub source_dimension: String,
    pub chunks: Vec<CopiedChunk>,
    /// 源选区原点（区块坐标）。
    pub origin: (i32, i32),
    /// 方块实体的数量（预览用）。
    pub block_entities: usize,
    /// 实体数量（若一并复制）。
    pub entities: usize,
}

impl ChunkClipboard {
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// 选区包围盒（区块坐标）。
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let mut min_dx = i32::MAX;
        let mut min_dz = i32::MAX;
        let mut max_dx = i32::MIN;
        let mut max_dz = i32::MIN;
        for c in &self.chunks {
            min_dx = min_dx.min(c.dx);
            min_dz = min_dz.min(c.dz);
            max_dx = max_dx.max(c.dx);
            max_dz = max_dz.max(c.dz);
        }
        if self.chunks.is_empty() {
            (0, 0, 0, 0)
        } else {
            (min_dx, min_dz, max_dx, max_dz)
        }
    }
}

/// 从维度中复制一个矩形区块选区。
pub fn copy_chunks(
    dim: &Dimension,
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
    include_entities: bool,
) -> crate::safety::Result<ChunkClipboard> {
    let (xa, xb) = (x0.min(x1), x0.max(x1));
    let (za, zb) = (z0.min(z1), z0.max(z1));

    let mut cb = ChunkClipboard {
        source_dimension: dim.id.clone(),
        origin: (xa, za),
        ..Default::default()
    };

    // 按 region 分组批量读取，避免逐个打开
    let mut cache: HashMap<(i32, i32), RegionFile> = HashMap::new();
    let mut ent_cache: HashMap<(i32, i32), RegionFile> = HashMap::new();

    for cx in xa..=xb {
        for cz in za..=zb {
            let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
            let path = dim.dir(RegionKind::Terrain).join(anvil::region_file_name(rx, rz));
            if !path.exists() {
                continue;
            }
            if !cache.contains_key(&(rx, rz)) {
                cache.insert((rx, rz), RegionFile::open(&path)?);
            }
            let Some(tag) = cache.get_mut(&(rx, rz)).unwrap().chunk(cx, cz)? else {
                continue;
            };

            let be_count = tag
                .as_compound()
                .and_then(|c| c.get("block_entities"))
                .and_then(|t| t.as_list())
                .map(|(_, v)| v.len())
                .unwrap_or(0);
            cb.block_entities += be_count;

            cb.chunks.push(CopiedChunk { dx: cx - xa, dz: cz - za, tag });
        }
    }

    if include_entities {
        for cx in xa..=xb {
            for cz in za..=zb {
                let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
                let path = dim.dir(RegionKind::Entities).join(anvil::region_file_name(rx, rz));
                if !path.exists() {
                    continue;
                }
                if !ent_cache.contains_key(&(rx, rz)) {
                    ent_cache.insert((rx, rz), RegionFile::open(&path)?);
                }
                let Some(tag) = ent_cache.get_mut(&(rx, rz)).unwrap().chunk(cx, cz)? else {
                    continue;
                };
                let n = tag
                    .as_compound()
                    .and_then(|c| c.get("Entities").or_else(|| c.get("entities")))
                    .and_then(|t| t.as_list())
                    .map(|(_, v)| v.len())
                    .unwrap_or(0);
                cb.entities += n;
                // 附在对应区块上，粘贴时一起搬
                if let Some(cc) = cb
                    .chunks
                    .iter_mut()
                    .find(|c| c.dx == cx - xa && c.dz == cz - za)
                {
                    let mut wrapper = Compound::new();
                    wrapper.insert("__entities".into(), tag);
                    if let Tag::Compound(inner) = &mut cc.tag {
                        inner.insert("__source_entities".into(), Tag::Compound(wrapper));
                    }
                }
            }
        }
    }

    Ok(cb)
}

/// 粘贴结果统计。
#[derive(Clone, Debug, Default)]
pub struct PasteStats {
    pub chunks_written: usize,
    pub chunks_skipped: usize,
    pub block_entities: usize,
    pub entities: usize,
    pub touched_regions: HashSet<(i32, i32)>,
}

/// 把剪贴板粘贴到目标位置（区块坐标）。
///
/// 会重写：`xPos`/`zPos`、方块实体的 `x`/`y`/`z`、
/// 实体坐标与 UUID（避免与原实体冲突）、`structures` 引用（保守清除，
/// 因为跨位置的结构引用会指向错误的区块）。
pub fn paste_chunks(
    dim: &Dimension,
    clipboard: &ChunkClipboard,
    target_x: i32,
    target_z: i32,
    overwrite: bool,
) -> crate::safety::Result<PasteStats> {
    let mut stats = PasteStats::default();
    let mut regions: HashMap<(i32, i32), RegionFile> = HashMap::new();
    let mut ent_regions: HashMap<(i32, i32), RegionFile> = HashMap::new();
    let mut uuid_seed: u64 = 0x9E37_79B9_7F4A_7C15;

    for chunk in &clipboard.chunks {
        let cx = target_x + chunk.dx;
        let cz = target_z + chunk.dz;
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));

        let path = dim.dir(RegionKind::Terrain).join(anvil::region_file_name(rx, rz));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if !regions.contains_key(&(rx, rz)) {
            regions.insert((rx, rz), RegionFile::open(&path)?);
        }
        let rf = regions.get_mut(&(rx, rz)).unwrap();

        if !overwrite && rf.contains(cx, cz) {
            stats.chunks_skipped += 1;
            continue;
        }

        let mut tag = chunk.tag.clone();
        // 拆分出附带的实体（用 shift_remove 保持其余键顺序稳定）
        let entities_tag = tag
            .as_compound_mut()
            .and_then(|c| c.shift_remove("__source_entities"));

        rewrite_chunk_position(&mut tag, cx, cz);
        let be = count_block_entities(&tag);
        stats.block_entities += be;

        rf.put_chunk(cx, cz, &tag)?;
        stats.chunks_written += 1;
        stats.touched_regions.insert((rx, rz));

        // 搬运实体
        if let Some(wrapper) = entities_tag {
            let inner = wrapper
                .as_compound()
                .and_then(|c| c.get("__entities"))
                .cloned();
            if let Some(ent_chunk) = inner {
                let epath = dim.dir(RegionKind::Entities).join(anvil::region_file_name(rx, rz));
                if let Some(p) = epath.parent() {
                    std::fs::create_dir_all(p).ok();
                }
                if !ent_regions.contains_key(&(rx, rz)) {
                    ent_regions.insert((rx, rz), RegionFile::open(&epath)?);
                }
                let erf = ent_regions.get_mut(&(rx, rz)).unwrap();
                let mut ec = ent_chunk;
                if let Some(c) = ec.as_compound_mut() {
                    c.insert("Position".into(), int_array(&[cx, cz]));
                    // 平移所有实体坐标并重分配 UUID
                    if let Some((_, items)) = c.get_mut("Entities").and_then(|t| t.as_list_mut()) {
                        let dx = (target_x - clipboard.origin.0) as f64 * 16.0;
                        let dz = (target_z - clipboard.origin.1) as f64 * 16.0;
                        for e in items.iter_mut() {
                            if let Some(ec2) = e.as_compound_mut() {
                                shift_entity(ec2, dx, dz, &mut uuid_seed);
                                stats.entities += 1;
                            }
                        }
                    }
                }
                erf.put_chunk(cx, cz, &ec)?;
            }
        }
    }

    // 落盘
    for (_, mut rf) in regions {
        rf.save()?;
    }
    for (_, mut rf) in ent_regions {
        rf.save()?;
    }
    Ok(stats)
}

fn int_array(v: &[i32]) -> Tag {
    Tag::IntArray(v.to_vec())
}

fn count_block_entities(tag: &Tag) -> usize {
    tag.as_compound()
        .and_then(|c| c.get("block_entities"))
        .and_then(|t| t.as_list())
        .map(|(_, v)| v.len())
        .unwrap_or(0)
}

/// 改写区块坐标，并平移方块实体与（必要的）结构引用。
///
/// `structures.starts` 里的结构记录带有绝对坐标与跨区块引用，
/// 跨位置粘贴时保留它们会产生指向错误位置的结构，因此整体清除。
fn rewrite_chunk_position(tag: &mut Tag, cx: i32, cz: i32) {
    let Some(c) = tag.as_compound_mut() else { return };
    let old_x = c.get("xPos").and_then(|t| t.as_i32()).unwrap_or(0);
    let old_z = c.get("zPos").and_then(|t| t.as_i32()).unwrap_or(0);
    let dx = (cx - old_x) * 16;
    let dz = (cz - old_z) * 16;

    c.insert("xPos".into(), Tag::Int(cx));
    c.insert("zPos".into(), Tag::Int(cz));

    // 平移方块实体坐标
    if let Some((_, items)) = c.get_mut("block_entities").and_then(|t| t.as_list_mut()) {
        for b in items.iter_mut() {
            if let Some(bc) = b.as_compound_mut() {
                if let Some(x) = bc.get("x").and_then(|t| t.as_i32()) {
                    bc.insert("x".into(), Tag::Int(x + dx));
                }
                if let Some(z) = bc.get("z").and_then(|t| t.as_i32()) {
                    bc.insert("z".into(), Tag::Int(z + dz));
                }
            }
        }
    }

    // 平移 block_ticks / fluid_ticks
    for key in ["block_ticks", "fluid_ticks"] {
        if let Some((_, items)) = c.get_mut(key).and_then(|t| t.as_list_mut()) {
            for t in items.iter_mut() {
                if let Some(tc) = t.as_compound_mut() {
                    if let Some(x) = tc.get("x").and_then(|t| t.as_i32()) {
                        tc.insert("x".into(), Tag::Int(x + dx));
                    }
                    if let Some(z) = tc.get("z").and_then(|t| t.as_i32()) {
                        tc.insert("z".into(), Tag::Int(z + dz));
                    }
                }
            }
        }
    }

    // 结构引用不再有效，清除并标记需重新扫描
    c.insert("structures".into(), Tag::Compound(Compound::new()));
    // 光照标记复位，让游戏重算
    c.insert("isLightOn".into(), Tag::Byte(0));
}

/// 平移实体坐标并重新分配 UUID。
fn shift_entity(e: &mut Compound, dx: f64, dz: f64, seed: &mut u64) {
    if let Some((_, items)) = e.get_mut("Pos").and_then(|t| t.as_list_mut()) {
        if items.len() >= 3 {
            if let Some(x) = items[0].as_f64() {
                items[0] = Tag::Double(x + dx);
            }
            if let Some(z) = items[2].as_f64() {
                items[2] = Tag::Double(z + dz);
            }
        }
    }
    // 重分配 UUID：xorshift 生成 128 位，保证与原实体不冲突
    let mut next = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed as i32
    };
    e.insert("UUID".into(), Tag::IntArray(vec![next(), next(), next(), next()]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(id: &str, x: f64, y: f64, z: f64) -> Compound {
        let mut c = Compound::new();
        c.insert("id".into(), Tag::String(id.into()));
        c.insert(
            "Pos".into(),
            Tag::new_list(
                TagType::Double,
                vec![Tag::Double(x), Tag::Double(y), Tag::Double(z)],
            ),
        );
        c
    }

    #[test]
    fn radius_filter_matches_only_inside() {
        let f = EntityFilter::around(0.0, 64.0, 0.0, 10.0);
        assert!(f.matches(&ent("minecraft:item", 5.0, 64.0, 5.0)));
        assert!(!f.matches(&ent("minecraft:item", 20.0, 64.0, 0.0)));
        // 3D 球形：Y 方向也受限
        assert!(!f.matches(&ent("minecraft:item", 0.0, 80.0, 0.0)));
    }

    #[test]
    fn type_filter_works_with_and_without_namespace() {
        let f = EntityFilter::default().with_types(&["item"]);
        assert!(f.matches(&ent("minecraft:item", 0.0, 0.0, 0.0)));
        assert!(!f.matches(&ent("minecraft:zombie", 0.0, 0.0, 0.0)));
    }

    #[test]
    fn exclude_beats_include() {
        let f = EntityFilter::default()
            .with_types(&["item", "zombie"])
            .excluding(&["zombie"]);
        assert!(f.matches(&ent("minecraft:item", 0.0, 0.0, 0.0)));
        assert!(!f.matches(&ent("minecraft:zombie", 0.0, 0.0, 0.0)));
    }

    #[test]
    fn normalize_id_adds_namespace() {
        assert_eq!(normalize_id("item"), "minecraft:item");
        assert_eq!(normalize_id("minecraft:item"), "minecraft:item");
        assert_eq!(normalize_id("somemod:thing"), "somemod:thing");
    }

    #[test]
    fn chunk_filter_defaults_to_unrestricted() {
        let f = EntityFilter::default();
        assert!(f.chunks.is_empty(), "默认不限制区块");
    }

    #[test]
    fn chunk_filter_can_target_single_chunk() {
        let mut f = EntityFilter::default();
        f.chunks.insert((39, 3120));
        assert!(f.chunks.contains(&(39, 3120)));
        assert!(!f.chunks.contains(&(38, 3120)));
        // 区块过滤不影响实体内部判定
        assert!(f.matches(&ent("minecraft:item", 0.0, 0.0, 0.0)));
    }

    #[test]
    fn lookup_path_reads_nested() {
        let mut item = Compound::new();
        item.insert("id".into(), Tag::String("minecraft:chest".into()));
        let mut c = Compound::new();
        c.insert("Item".into(), Tag::Compound(item));
        assert_eq!(
            lookup_path(&c, "Item.id").as_deref(),
            Some("minecraft:chest")
        );
        assert_eq!(lookup_path(&c, "Item.missing"), None);
    }

    #[test]
    fn presets_cover_expected_types() {
        let f = CleanupPreset::Items.to_filter();
        assert!(f.matches(&ent("minecraft:item", 0.0, 0.0, 0.0)));
        assert!(!f.matches(&ent("minecraft:zombie", 0.0, 0.0, 0.0)));

        let fb = CleanupPreset::FallingBlocks.to_filter();
        assert!(fb.matches(&ent("minecraft:falling_block", 0.0, 0.0, 0.0)));

        // 非生物预设不能误删村民
        let nl = CleanupPreset::NonLiving.to_filter();
        assert!(nl.matches(&ent("minecraft:item", 0.0, 0.0, 0.0)));
        assert!(nl.matches(&ent("minecraft:arrow", 0.0, 0.0, 0.0)));
        assert!(!nl.matches(&ent("minecraft:villager", 0.0, 0.0, 0.0)));
        assert!(!nl.matches(&ent("minecraft:armor_stand", 0.0, 0.0, 0.0)));
    }

    #[test]
    fn rewrite_position_shifts_block_entities() {
        let mut c = Compound::new();
        c.insert("xPos".into(), Tag::Int(0));
        c.insert("zPos".into(), Tag::Int(0));
        let mut be = Compound::new();
        be.insert("x".into(), Tag::Int(5));
        be.insert("y".into(), Tag::Int(64));
        be.insert("z".into(), Tag::Int(7));
        c.insert(
            "block_entities".into(),
            Tag::new_list(TagType::Compound, vec![Tag::Compound(be)]),
        );
        let mut tag = Tag::Compound(c);

        rewrite_chunk_position(&mut tag, 10, 20);
        let c = tag.as_compound().unwrap();
        assert_eq!(c["xPos"].as_i32(), Some(10));
        assert_eq!(c["zPos"].as_i32(), Some(20));
        let (_, items) = c["block_entities"].as_list().unwrap();
        let be = items[0].as_compound().unwrap();
        // 平移 (10-0)*16 = 160, (20-0)*16 = 320
        assert_eq!(be["x"].as_i32(), Some(165));
        assert_eq!(be["z"].as_i32(), Some(327));
    }

    #[test]
    fn shift_entity_moves_and_reassigns_uuid() {
        let mut e = ent("minecraft:item", 0.0, 64.0, 0.0);
        let mut seed = 12345u64;
        shift_entity(&mut e, 160.0, 320.0, &mut seed);
        let (_, pos) = e["Pos"].as_list().unwrap();
        assert_eq!(pos[0].as_f64(), Some(160.0));
        assert_eq!(pos[2].as_f64(), Some(320.0));
        assert_eq!(e["UUID"].as_int_array().unwrap().len(), 4);
    }
}
