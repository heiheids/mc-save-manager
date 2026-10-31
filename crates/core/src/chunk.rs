//! 区块（Chunk）解析：调色板解压、方块状态查询、地表采样。
//!
//! 1.21.1 的区块格式（1.18+ 扁平化后）：
//! - `sections[]` 每节 16×16×16，字段 `Y`、`block_states{palette,data}`、`biomes{...}`
//! - `block_states.palette` 是复合物列表 `{Name, Properties}`（1.20.5+ 才出现纯字符串形式，
//!   但写盘时 1.21.1 仍以复合物为主，两种都要能读）
//! - `data` 是 long 数组，索引位宽 `bits = max(4, ceil(log2(palette.len())))`，
//!   **索引不跨 long 打包**（不同于 1.16 之前的连续位流）
//! - 单个 block 的下标顺序为 YZX：`idx = y*256 + z*16 + x`

use crate::colors::{self, Rgb};
use crate::nbt::{Compound, Tag, TagType};

/// 区块在某个高度上的顶层方块信息。
#[derive(Clone, Debug)]
pub struct Column {
    /// 采样到的方块 Y；-1 表示该列在采样层没有方块。
    pub y: i32,
    pub block: String,
    pub color: Rgb,
    /// 该列所属群系 id（如 `minecraft:plains`）。采样时不填，悬停查询时按需读取。
    pub biome: Option<String>,
}

/// 解压 long 数组中的打包索引。
///
/// `bits` 为每项位宽；索引**不跨** long 边界，因此每个 long 存 `64 / bits` 项。
fn unpack_indices(data: &[i64], bits: u32, count: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(count);
    if bits == 0 {
        return out;
    }
    let per_long = (64 / bits) as usize;
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    for v in data.iter() {
        let uv = *v as u64;
        for k in 0..per_long {
            if out.len() >= count {
                return out;
            }
            out.push(((uv >> (k as u32 * bits)) & mask) as u16);
        }
    }
    out
}

/// 读取 block_states.palette，返回每个调色板项的方块名。
/// 读取方块调色板。
///
/// 三种已知形式（按出现顺序覆盖）：
///
/// 1. **裸字符串**：`"minecraft:stone"`（1.20.5 之前 / 部分工具产出）
/// 2. **带 Name 的复合物**：`{Name: "minecraft:stone", Properties: {...}}`（1.16–1.21.1）
/// 3. **空键复合物**：`{"": "minecraft:stone"}`（**26.x 实测**）
///
/// 第 3 种是最容易踩坑的：26.3 的调色板项是复合物，但键名是**空字符串**而不是
/// `Name`。如果只按 `Name` 查，会把所有方块都当成 air，导致地图大面积变空、掉色。
fn read_palette(palette: &Tag) -> Vec<String> {
    let mut out = Vec::new();
    let Some((_, items)) = palette.as_list() else {
        return out;
    };
    for item in items {
        out.push(palette_entry_name(item));
    }
    out
}

/// 从单个调色板项里取出方块 id。
fn palette_entry_name(item: &Tag) -> String {
    match item {
        Tag::String(s) => s.clone(),
        Tag::Compound(c) => {
            // 常见键名
            for key in ["Name", "id", "name", ""] {
                if let Some(s) = c.get(key).and_then(|t| t.as_str()) {
                    return s.to_string();
                }
            }
            // 兜底：取第一个字符串值（26.x 的空键形式走这里也能命中）
            for (_, v) in c.iter() {
                if let Some(s) = v.as_str() {
                    return s.to_string();
                }
            }
            "minecraft:air".to_string()
        }
        _ => "minecraft:air".to_string(),
    }
}

/// 一个 section 的方块调色板 + 索引。
pub struct SectionView {
    pub y: i8,
    pub palette: Vec<String>,
    /// 空表示整节都是 palette[0]。
    indices: Option<Vec<u16>>,
    pub bits: u32,
}

impl SectionView {
    /// 取 section 内局部坐标 (x,y,z)（各 0..16）的方块名。
    pub fn block_at(&self, x: usize, y: usize, z: usize) -> &str {
        if self.palette.is_empty() {
            return "minecraft:air";
        }
        match &self.indices {
            None => &self.palette[0],
            Some(idx) => {
                let i = y * 256 + z * 16 + x;
                let pi = idx.get(i).copied().unwrap_or(0) as usize;
                self.palette.get(pi).map(|s| s.as_str()).unwrap_or("minecraft:air")
            }
        }
    }

    /// 该 section 是否整节只有一种方块（通常是空气）。
    pub fn is_uniform(&self) -> bool {
        self.palette.len() <= 1
    }

    /// 判断 section 内某个水平层（ly ∈ 0..16）是否全为空气，用于快速跳过整层。
    pub fn layer_is_air(&self, ly: usize) -> bool {
        if self.palette.is_empty() {
            return true;
        }
        // 调色板里没有任何空气 → 该层必然非空
        let any_air = self.palette.iter().any(|p| colors::is_air_like(p));
        if !any_air {
            return false;
        }
        let Some(idx) = &self.indices else {
            return colors::is_air_like(&self.palette[0]);
        };
        let base = ly * 256;
        (0..256).all(|i| {
            let pi = idx.get(base + i).copied().unwrap_or(0) as usize;
            self.palette
                .get(pi)
                .map(|s| colors::is_air_like(s))
                .unwrap_or(true)
        })
    }
}

/// 解析单个 section。
pub fn parse_section(tag: &Tag) -> Option<SectionView> {
    let c = tag.as_compound()?;
    let y = c.get("Y").and_then(|t| t.as_i8())?;
    let bs = c.get("block_states")?;
    let bs = bs.as_compound()?;
    let palette_tag = bs.get("palette")?;
    let palette = read_palette(palette_tag);

    let bits = {
        let n = palette.len().max(1);
        let need = (usize::BITS - (n - 1).leading_zeros()).max(4);
        need.min(16)
    };

    let indices = match bs.get("data") {
        None => None, // 单一方块，整节填充
        Some(t) => t
            .as_long_array()
            .map(|data| unpack_indices(data, bits, 4096)),
    };

    Some(SectionView { y, palette, indices, bits })
}

/// 解析区块的所有 section（按 Y 升序）。
pub fn parse_sections(chunk: &Tag) -> Vec<SectionView> {
    let mut out = Vec::new();
    if let Some(c) = chunk.as_compound() {
        if let Some((elem, items)) = c.get("sections").and_then(|t| t.as_list()) {
            if *elem == TagType::Compound || items.iter().any(|t| t.as_compound().is_some()) {
                for it in items {
                    if let Some(s) = parse_section(it) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| s.y);
    out
}

/// 区块坐标范围常量。
pub const CHUNK_MIN_Y: i32 = -64;
pub const CHUNK_MAX_Y: i32 = 320;

/// 读取某个高度图（9 bit × 256，每 long 存 7 项）。
///
/// 返回值是**世界坐标 Y**（已加上世界底部）。缺失时返回 None。
pub fn heightmap(c: &Compound, name: &str) -> Option<Vec<i32>> {
    let arr = c.get("Heightmaps")?.as_compound()?.get(name)?.as_long_array()?;
    let mut out = Vec::with_capacity(256);
    let values_per_long = 7usize;
    let mask = 0x1FFu64;
    for v in arr.iter() {
        let uv = *v as u64;
        for k in 0..values_per_long {
            if out.len() >= 256 {
                break;
            }
            let h = ((uv >> (k * 9)) & mask) as i32;
            out.push(h);
        }
    }
    out.truncate(256);
    if out.len() < 256 {
        return None;
    }
    Some(out)
}

/// 在地表采样时使用的图层策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SliceMode {
    /// 从顶向下找第一个非空气（穿透基岩等封面方块）。适合主世界。
    #[default]
    Surface,
    /// 固定取指定 Y 层上的方块。适合下界（顶部有基岩天花板）。
    Fixed(i32),
}

/// 对 256 列采样方块（俯视图用）。
///
/// **不使用 `Heightmaps`**：实测该存档中有约 24% 的列高度图与实际方块不符
/// （被外部工具改过方块后未刷新），用它渲染会画出错误地形。
///
/// - `SliceMode::Surface`：从顶层 section 向下扫描，穿透空气与基岩，
///   取第一个"真实地表"方块；
/// - `SliceMode::Fixed(y)`：直接取 `y` 这一层的方块（该层是空气则视为空）。
///   下界默认用它（`y = 65`），因为顶部有基岩天花板。
pub fn surface_columns(sections: &[SectionView], mode: SliceMode) -> Vec<Column> {
    let mut out: Vec<Column> = (0..256)
        .map(|_| Column { y: -1, block: "minecraft:air".into(), color: [0, 0, 0], biome: None })
        .collect();

    match mode {
        SliceMode::Fixed(target_y) => {
            let sec_y = target_y.div_euclid(16) as i8;
            let ly = target_y.rem_euclid(16) as usize;
            let Some(sec) = sections.iter().find(|s| s.y == sec_y) else {
                return out;
            };
            for col in 0..256 {
                let x = col % 16;
                let z = col / 16;
                let b = sec.block_at(x, ly, z);
                // 指定层是空气 / 封面方块时按空处理，避免整片基岩色
                if colors::is_air_like(b) || colors::is_cover_block(b) {
                    continue;
                }
                out[col] = Column {
                    y: target_y,
                    block: b.to_string(),
                    color: colors::block_color(b),
                    biome: None,
                };
            }
            out
        }
        SliceMode::Surface => {
            // 语义参考 SwiftMap 的 ClientChunkCapture：
            //
            //   "地表" = 上方紧邻空气的非空气方块
            //
            // 这一条与维度无关，并天然处理三种容易出错的情况：
            //   - 下界顶盖：顶盖上方还是岩石 -> 不算地表，继续往下找真正的岩面
            //   - 树叶顶冠：叶子上方是空气 -> 命中树冠（而不是树下的地面）
            //   - 水面：水面上方是空气 -> 命中水面
            //
            // 关键：**不能**简单的"取最上面的非空气方块"，那会在洞穴 / 深板岩层里
            // 停在被封住的实心方块上，画出大片灰块（26.x 存档的灰块正是这个原因）。
            //
            // 每列维护自己的 above_is_air 状态；扫描起点之上按"非空气"处理，
            // 因此整列最顶上那层不会被误判成地表。
            // 扫描起点**之上**按"非空气"处理：最顶层那格如果本身是实心，
            // 它的上方就是"世界之上"，应算作露天（SwiftMap 同样这么处理）。
            let mut above = [true; 256];
            let mut done = [false; 256];
            let mut remaining = 256usize;

            for sec in sections.iter().rev() {
                if remaining == 0 {
                    break;
                }

                // 整段空气：下一段（更靠下）最上层方块的上方就是空气
                if sec.is_uniform() {
                    let only = sec
                        .palette
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("minecraft:air");
                    if colors::is_air_like(only) {
                        for c in above.iter_mut() {
                            *c = true;
                        }
                        continue;
                    }
                }

                for ly in (0..16).rev() {
                    if remaining == 0 {
                        break;
                    }
                    let layer_air = sec.layer_is_air(ly);
                    for col in 0..256 {
                        if done[col] {
                            continue;
                        }
                        let x = col % 16;
                        let z = col / 16;
                        let b = if layer_air {
                            "minecraft:air"
                        } else {
                            sec.block_at(x, ly, z)
                        };
                        let air = colors::is_air_like(b);

                        if !air && above[col] && !colors::is_cover_block(b) {
                            let y = sec.y as i32 * 16 + ly as i32;
                            out[col] = Column {
                                y,
                                block: b.to_string(),
                                color: colors::block_color(b),
                                biome: None,
                            };
                            done[col] = true;
                            remaining -= 1;
                        }
                        // 更新本列的"上方是否空气"，供下一层（更靠下）判断
                        above[col] = air;
                    }
                }
            }

            // 兜底：扫到底都没找到"上方是空气"的列（整列实心，如泡世界），
            // 用最上面那个非空气方块，避免整片空白（空白比基岩灰更让人困惑）。
            if remaining > 0 {
                for sec in sections.iter().rev() {
                    if remaining == 0 {
                        break;
                    }
                    for ly in (0..16).rev() {
                        for col in 0..256 {
                            if done[col] {
                                continue;
                            }
                            let b = sec.block_at(col % 16, ly, col / 16);
                            if colors::is_air_like(b) || colors::is_cover_block(b) {
                                continue;
                            }
                            let y = sec.y as i32 * 16 + ly as i32;
                            out[col] = Column {
                                y,
                                block: b.to_string(),
                                color: colors::block_color(b),
                                biome: None,
                            };
                            done[col] = true;
                            remaining -= 1;
                        }
                        if remaining == 0 {
                            break;
                        }
                    }
                }
            }
            out
        }
    }
}

/// 读取区块内某个方块坐标的群系 id。
///
/// 1.18+ 的群系按 4×4×4 的单元格存储：每个 section 的 `biomes.palette` + `data`，
/// 索引位宽 `max(0, ceil(log2(n)))`（注意是 **0**，不是方块那种最小值 4），
/// 每个 long 存 `64/bits` 项，索引顺序同样是 YZX 但每轴只有 4 格。
pub fn biome_at(sections: &[SectionView], biomes: &[BiomeSection], x: i32, y: i32, z: i32) -> Option<String> {
    let sec_y = y.div_euclid(16) as i8;
    let sec = biomes.iter().find(|b| b.y == sec_y)?;
    if sec.palette.is_empty() {
        return None;
    }
    // section 内坐标 → 群系单元格坐标（0..4）
    let cx = (x.rem_euclid(16) / 4) as usize;
    let cy = (y.rem_euclid(16) / 4) as usize;
    let cz = (z.rem_euclid(16) / 4) as usize;
    let _ = sections;
    let idx = cy * 16 + cz * 4 + cx;
    let bi = sec.index_at(idx) as usize;
    sec.palette.get(bi).cloned()
}

/// 一个 section 的生物群系调色板与索引。
pub struct BiomeSection {
    pub y: i8,
    pub palette: Vec<String>,
    indices: Option<Vec<u16>>,
}

impl BiomeSection {
    fn index_at(&self, i: usize) -> u16 {
        match &self.indices {
            None => 0,
            Some(v) => v.get(i).copied().unwrap_or(0),
        }
    }
}

/// 解析区块的生物群系数据。
pub fn parse_biomes(chunk: &Tag) -> Vec<BiomeSection> {
    let mut out = Vec::new();
    let Some(c) = chunk.as_compound() else { return out };
    let Some((_, items)) = c.get("sections").and_then(|t| t.as_list()) else {
        return out;
    };
    for it in items {
        let Some(sec) = it.as_compound() else { continue };
        let Some(y) = sec.get("Y").and_then(|t| t.as_i8()) else { continue };
        let Some(bio) = sec.get("biomes").and_then(|t| t.as_compound()) else {
            continue;
        };
        let Some(palette_tag) = bio.get("palette") else { continue };
        let palette = read_palette(palette_tag);
        let bits = {
            let n = palette.len().max(1);
            let need = (usize::BITS - (n - 1).leading_zeros()).max(1);
            need.min(16)
        };
        let indices = bio
            .get("data")
            .and_then(|t| t.as_long_array())
            .map(|d| unpack_indices(d, bits, 64));
        out.push(BiomeSection { y, palette, indices });
    }
    out.sort_by_key(|b| b.y);
    out
}

/// 统计区块内每种方块的出现次数（调试 / 分析用，遍历全部 section，较慢）。
pub fn count_blocks(sections: &[SectionView]) -> std::collections::HashMap<String, u64> {
    let mut m = std::collections::HashMap::new();
    for sec in sections {
        if sec.is_uniform() {
            *m.entry(sec.palette.first().cloned().unwrap_or_else(|| "minecraft:air".into()))
                .or_insert(0) += 4096;
            continue;
        }
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let b = sec.block_at(x, y, z);
                    *m.entry(b.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    m
}

/// 区块是否已完全生成（可安全视为"真实地形"）。
pub fn is_full_status(chunk: &Tag) -> bool {
    chunk
        .as_compound()
        .and_then(|c| c.get("Status").or_else(|| c.get("status")))
        .and_then(|t| t.as_str())
        .map(|s| s == "minecraft:full")
        .unwrap_or(false)
}

/// 读取区块的 xPos/zPos。
pub fn chunk_pos(chunk: &Tag) -> Option<(i32, i32)> {
    let c = chunk.as_compound()?;
    Some((c.get("xPos")?.as_i32()?, c.get("zPos")?.as_i32()?))
}

/// 读区块 DataVersion。
pub fn data_version(chunk: &Tag) -> Option<i32> {
    chunk.as_compound()?.get("DataVersion")?.as_i32()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::Compound;

    fn make_section(y: i8, palette: Vec<&str>, data: Option<Vec<i64>>) -> Tag {
        let mut bs = Compound::new();
        let pal_items: Vec<Tag> = palette
            .iter()
            .map(|n| {
                let mut c = Compound::new();
                c.insert("Name".into(), Tag::String((*n).into()));
                Tag::Compound(c)
            })
            .collect();
        bs.insert("palette".into(), Tag::new_list(TagType::Compound, pal_items));
        if let Some(d) = data {
            bs.insert("data".into(), Tag::LongArray(d));
        }
        let mut s = Compound::new();
        s.insert("Y".into(), Tag::Byte(y));
        s.insert("block_states".into(), Tag::Compound(bs));
        Tag::Compound(s)
    }

    #[test]
    fn single_block_palette_fills_section() {
        let sec = parse_section(&make_section(0, vec!["minecraft:stone"], None)).unwrap();
        assert_eq!(sec.block_at(0, 0, 0), "minecraft:stone");
        assert_eq!(sec.block_at(15, 15, 15), "minecraft:stone");
        assert!(sec.is_uniform());
        assert_eq!(sec.bits, 4, "最小位宽应为 4");
    }

    #[test]
    fn unpacking_does_not_span_longs() {
        // bits=4 → 每 long 存 16 项，低位在前
        // 构造：项 0 = 0, 项 1 = 1, 项 2 = 2, 项 3 = 3
        let first: u64 = 0 | (1 << 4) | (2 << 8) | (3 << 12);
        let d: Vec<i64> = vec![first as i64, -1]; // 第二个 long 全是 0xF
        let out = unpack_indices(&d, 4, 32);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 1);
        assert_eq!(out[2], 2);
        assert_eq!(out[3], 3);
        assert_eq!(out[15], 0);
        // 第 16 项来自第二个 long 的最低位，说明没有跨 long 拼接
        assert_eq!(out[16], 15);
    }

    #[test]
    fn index_order_is_yzx() {
        // 构造一个只有两个方块的调色板，把 (x=1,y=0,z=0) 设为第二项
        // bits=4, 每 long 16 项, idx = y*256+z*16+x = 1
        let mut longs = vec![0i64; 4096 / 16];
        longs[0] = 1 << 4; // 第 1 项 = 1
        let sec = parse_section(&make_section(
            0,
            vec!["minecraft:air", "minecraft:stone"],
            Some(longs),
        ))
        .unwrap();
        assert_eq!(sec.block_at(1, 0, 0), "minecraft:stone");
        assert_eq!(sec.block_at(0, 0, 0), "minecraft:air");
    }

    #[test]
    fn string_palette_is_supported() {
        let mut bs = Compound::new();
        bs.insert(
            "palette".into(),
            Tag::new_list(
                TagType::String,
                vec![Tag::String("minecraft:dirt".into())],
            ),
        );
        let mut s = Compound::new();
        s.insert("Y".into(), Tag::Byte(0));
        s.insert("block_states".into(), Tag::Compound(bs));
        let sec = parse_section(&Tag::Compound(s)).unwrap();
        assert_eq!(sec.block_at(5, 5, 5), "minecraft:dirt");
    }

    #[test]
    fn heightmap_decodes_7_per_long() {
        // 全 0 的高度图 → 所有列高度 = 世界底部
        let mut c = Compound::new();
        let mut hm = Compound::new();
        hm.insert("WORLD_SURFACE".into(), Tag::LongArray(vec![0i64; 37]));
        c.insert("Heightmaps".into(), Tag::Compound(hm));
        let h = heightmap(&c, "WORLD_SURFACE").unwrap();
        assert_eq!(h.len(), 256);
        assert!(h.iter().all(|v| *v == 0));
    }

    #[test]
    fn surface_ignores_stale_heightmap() {
        // Y=0 全是 air，Y=1 全是 stone
        let air = parse_section(&make_section(0, vec!["minecraft:air"], None)).unwrap();
        let stone = parse_section(&make_section(1, vec!["minecraft:stone"], None)).unwrap();
        let sections = vec![air, stone];

        // 故意给一个「陈旧且错误」的高度图（声称顶层在 y=200）
        let mut packed = vec![0i64; 37];
        let bogus = 264u64;
        for i in 0..256 {
            packed[i / 7] |= (bogus << ((i % 7) * 9)) as i64;
        }
        let mut c = Compound::new();
        let mut hm = Compound::new();
        hm.insert("WORLD_SURFACE".into(), Tag::LongArray(packed));
        c.insert("Heightmaps".into(), Tag::Compound(hm));

        let cols = surface_columns(&sections, SliceMode::Surface);
        assert_eq!(cols.len(), 256);
        // 必须来自真实方块扫描（section 1 的顶层 = 31），而不是陈旧高度图
        assert_eq!(cols[0].y, 31, "不能用陈旧高度图");
        assert_eq!(cols[0].block, "minecraft:stone");
        assert_eq!(cols[0].color, colors::block_color("minecraft:stone"));
    }

    #[test]
    fn surface_finds_topmost_across_sections() {
        let stone = parse_section(&make_section(1, vec!["minecraft:stone"], None)).unwrap();
        let dirt = parse_section(&make_section(2, vec!["minecraft:dirt"], None)).unwrap();
        let air_top = parse_section(&make_section(3, vec!["minecraft:air"], None)).unwrap();
        let c = Compound::new();
        let cols = surface_columns(&[stone, dirt, air_top], SliceMode::Surface);
        assert!(cols.iter().all(|col| col.y == 47 && col.block == "minecraft:dirt"));
    }

    #[test]
    fn parses_empty_key_palette_26x() {
        // 26.x 的调色板项：复合物但键名是空字符串
        //   Compound({ "": String("minecraft:stone") })
        // 实测 26.3 存档就是这种形式；只按 "Name" 查会把所有方块读成 air，
        // 导致地图大面积空白/掉色。
        let mut entry = Compound::new();
        entry.insert("".into(), Tag::String("minecraft:stone".into()));

        let mut bs = Compound::new();
        bs.insert(
            "palette".into(),
            Tag::new_list(TagType::Compound, vec![Tag::Compound(entry)]),
        );
        let mut s = Compound::new();
        s.insert("Y".into(), Tag::Byte(0));
        s.insert("block_states".into(), Tag::Compound(bs));

        let sec = parse_section(&Tag::Compound(s)).unwrap();
        assert_eq!(sec.palette.len(), 1);
        assert_eq!(sec.palette[0], "minecraft:stone");
        assert_eq!(sec.block_at(0, 0, 0), "minecraft:stone");
    }

    #[test]
    fn parses_named_palette_121() {
        // 1.21.1 的形式：{ Name: "minecraft:dirt", Properties: {...} }
        let mut props = Compound::new();
        props.insert("snowy".into(), Tag::String("false".into()));
        let mut entry = Compound::new();
        entry.insert("Name".into(), Tag::String("minecraft:dirt".into()));
        entry.insert("Properties".into(), Tag::Compound(props));

        let mut bs = Compound::new();
        bs.insert(
            "palette".into(),
            Tag::new_list(TagType::Compound, vec![Tag::Compound(entry)]),
        );
        let mut s = Compound::new();
        s.insert("Y".into(), Tag::Byte(0));
        s.insert("block_states".into(), Tag::Compound(bs));

        let sec = parse_section(&Tag::Compound(s)).unwrap();
        assert_eq!(sec.palette[0], "minecraft:dirt");
    }

    #[test]
    fn parses_plain_string_palette() {
        // 裸字符串形式
        let mut bs = Compound::new();
        bs.insert(
            "palette".into(),
            Tag::new_list(
                TagType::String,
                vec![Tag::String("minecraft:grass_block".into())],
            ),
        );
        let mut s = Compound::new();
        s.insert("Y".into(), Tag::Byte(0));
        s.insert("block_states".into(), Tag::Compound(bs));

        let sec = parse_section(&Tag::Compound(s)).unwrap();
        assert_eq!(sec.palette[0], "minecraft:grass_block");
    }

    #[test]
    fn surface_means_block_with_air_above() {
        // 关键语义（参考 SwiftMap）：地表 = 上方紧邻空气的非空气方块。
        //
        // section 顺序（Y 升序传入）：[空气, 石头, 空气]
        //   Y 0..15  = 空气
        //   Y 16..31 = 石头
        //   Y 32..47 = 空气
        // 正确结果应是 Y=31（石头的最上面一层，其上方是空气）。
        let air_bottom = parse_section(&make_section(0, vec!["minecraft:air"], None)).unwrap();
        let stone = parse_section(&make_section(1, vec!["minecraft:stone"], None)).unwrap();
        let air_top = parse_section(&make_section(2, vec!["minecraft:air"], None)).unwrap();

        let cols = surface_columns(&[air_bottom, stone, air_top], SliceMode::Surface);
        assert!(
            cols.iter().all(|c| c.y == 31),
            "应命中石头层顶部 Y=31，实际 {:?}",
            cols.iter().map(|c| c.y).collect::<std::collections::HashSet<_>>()
        );
        assert!(cols.iter().all(|c| c.block == "minecraft:stone"));
    }

    #[test]
    fn surface_skips_buried_solid_block() {
        // "上方紧邻空气"的判定：顶层实心方块（上方是世界之上）就是地表。
        //   Y 0..15  = 深板岩
        //   Y 16..31 = 空气（洞穴）
        //   Y 32..47 = 石头
        //   Y 48..63 = 深板岩（最顶层，上方是世界之上 → 命中）
        //
        // 所以正确结果是 Y=63 的深板岩；石头虽然露在洞穴里，
        // 但它上方是深板岩，不算地表。
        let deep = parse_section(&make_section(0, vec!["minecraft:deepslate"], None)).unwrap();
        let cave = parse_section(&make_section(1, vec!["minecraft:air"], None)).unwrap();
        let stone = parse_section(&make_section(2, vec!["minecraft:stone"], None)).unwrap();
        let cap = parse_section(&make_section(3, vec!["minecraft:deepslate"], None)).unwrap();

        let cols = surface_columns(&[deep, cave, stone, cap], SliceMode::Surface);
        assert!(
            cols.iter().all(|c| c.y == 63),
            "顶层深板岩上方是空气，应命中 Y=63，实际 {:?}",
            cols.iter().map(|c| c.y).collect::<std::collections::HashSet<_>>()
        );
        assert!(cols.iter().all(|c| c.block == "minecraft:deepslate"));
    }

    #[test]
    fn surface_picks_exposed_stone_under_cave() {
        // 反过来验证：如果顶层是空气，洞穴里暴露的石头就是地表。
        //   Y 0..15  = 深板岩
        //   Y 16..31 = 石头
        //   Y 32..47 = 空气（上方全空）
        let deep = parse_section(&make_section(0, vec!["minecraft:deepslate"], None)).unwrap();
        let stone = parse_section(&make_section(1, vec!["minecraft:stone"], None)).unwrap();
        let air_top = parse_section(&make_section(2, vec!["minecraft:air"], None)).unwrap();

        let cols = surface_columns(&[deep, stone, air_top], SliceMode::Surface);
        assert!(cols.iter().all(|c| c.y == 31), "应命中石头顶部 Y=31");
        assert!(cols.iter().all(|c| c.block == "minecraft:stone"));
    }

    #[test]
    fn surface_picks_tree_canopy_over_ground() {
        // 树叶上方是空气 → 应命中树冠，而不是树下的地面。
        let grass = parse_section(&make_section(0, vec!["minecraft:grass_block"], None)).unwrap();
        let air = parse_section(&make_section(1, vec!["minecraft:air"], None)).unwrap();
        let leaves = parse_section(&make_section(2, vec!["minecraft:oak_leaves"], None)).unwrap();

        let cols = surface_columns(&[grass, air, leaves], SliceMode::Surface);
        assert!(cols.iter().all(|c| c.y == 47), "应命中树冠 Y=47");
        assert!(cols.iter().all(|c| c.block == "minecraft:oak_leaves"));
    }

    #[test]
    fn surface_skips_bedrock_ceiling() {
        // 模拟下界：Y=8 是 netherrack，Y=9 是基岩天花板
        let netherrack = parse_section(&make_section(0, vec!["minecraft:netherrack"], None)).unwrap();
        let bedrock = parse_section(&make_section(1, vec!["minecraft:bedrock"], None)).unwrap();
        let c = Compound::new();
        let cols = surface_columns(&[netherrack, bedrock], SliceMode::Surface);
        // 必须穿透基岩，取到下面的 netherrack（Y=15），而不是 Y=31 的基岩
        assert!(cols.iter().all(|col| col.y == 15), "应穿透基岩天花板");
        assert!(cols.iter().all(|col| col.block == "minecraft:netherrack"));
    }

    #[test]
    fn surface_all_bedrock_column_becomes_empty() {
        // 整列只有基岩（主世界底部那种情况）→ 该列应视为无地表
        let bedrock = parse_section(&make_section(0, vec!["minecraft:bedrock"], None)).unwrap();
        let c = Compound::new();
        let cols = surface_columns(&[bedrock], SliceMode::Surface);
        assert!(cols.iter().all(|col| col.y == -1), "纯基岩列不应被当成地表");
    }

    #[test]
    fn empty_chunk_yields_air_columns() {
        let air = parse_section(&make_section(0, vec!["minecraft:air"], None)).unwrap();
        let cols = surface_columns(&[air], SliceMode::Surface);
        assert!(cols.iter().all(|col| col.y == -1));
    }

    #[test]
    fn fixed_slice_reads_exact_layer() {
        // section 4 覆盖 Y=64..79。取 Y=70 时该节是 air → 空列
        let air = parse_section(&make_section(4, vec!["minecraft:air"], None)).unwrap();
        let cols = surface_columns(&[air], SliceMode::Fixed(70));
        assert!(cols.iter().all(|col| col.y == -1), "空层应视为无方块");
    }

    #[test]
    fn fixed_slice_returns_target_y() {
        // section 4 = Y 64..79，全 stone；取 Y=65 应记录 65 而不是 section 顶
        let stone = parse_section(&make_section(4, vec!["minecraft:stone"], None)).unwrap();
        let cols = surface_columns(&[stone], SliceMode::Fixed(65));
        assert!(cols.iter().all(|col| col.y == 65), "应记录目标高度 65");
        assert!(cols.iter().all(|col| col.block == "minecraft:stone"));
    }

    #[test]
    fn fixed_slice_skips_bedrock_layer() {
        // 固定层若正好是基岩，应视为空，而不是涂成灰色
        let bedrock = parse_section(&make_section(4, vec!["minecraft:bedrock"], None)).unwrap();
        let cols = surface_columns(&[bedrock], SliceMode::Fixed(65));
        assert!(cols.iter().all(|col| col.y == -1), "固定层是基岩时应视为空");
    }

    #[test]
    fn fixed_slice_missing_section_is_empty() {
        let stone = parse_section(&make_section(0, vec!["minecraft:stone"], None)).unwrap();
        let cols = surface_columns(&[stone], SliceMode::Fixed(200));
        assert!(cols.iter().all(|col| col.y == -1), "不存在的 section 应为空");
    }
}
