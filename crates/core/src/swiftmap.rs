//! SwiftMap 小地图模组的数据读取（`<存档>/swiftmap/`）。
//!
//! 只读，不改写——SwiftMap 自己的格式里有 CRC 和双索引代际，
//! 第三方改它很容易把数据搞坏。这里的目标是：**让存档管理器能直接
//! 显示 SwiftMap 已经采集好的地图**，省去重新扫描存档的时间。
//!
//! 格式来源：`D:\工程-迅捷地图\DATA_FORMAT.md` 与对应 Java 源码。
//!
//! ## 目录
//! ```text
//! <存档>/swiftmap/
//!   identity.json
//!   dimensions/<ns>/<path>/
//!     format.json
//!     lod0/r.<cx>.<cz>.smr          ← 颜色瓦片容器
//!     structures.jsonl
//! ```
//!
//! ## `.smr` 容器
//! ```text
//! 0x00  header        64 B
//! 0x40  index copy A  slotCount × 32 B
//! 0x840 index copy B  slotCount × 32 B
//! 0x1040 data area    只追加
//! ```
//!
//! ## 瓦片负载（大端）
//! ```text
//! short paletteSize
//! int[paletteSize] palette    0xRRGGBB
//! byte[4096] indices          索引 0 = 无数据
//! short[4096] heights         （可选）
//! short biomeCount + 群系段    （可选）
//! ```

use crate::nbt::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 瓦片边长（像素 = 方块，LOD0）。
pub const TILE_SIZE: usize = 64;
/// 每瓦片覆盖的区块数（4×4）。
pub const CHUNKS_PER_TILE: i32 = 4;
/// 每容器的瓦片数（8×8）。
pub const TILES_PER_CONTAINER: i32 = 8;
/// 每容器的槽位数。
pub const SLOTS_PER_CONTAINER: usize = 64;

const HEADER_BYTES: usize = 64;
const SLOT_BYTES: usize = 32;
const MAGIC: u32 = 0x534D_5231; // "SMR1"

const STATE_PRESENT: u8 = 1;
const FLAG_COMPRESSED: u8 = 0x01;
const FLAG_CODEC_SHIFT: u32 = 1;
const FLAG_CODEC_MASK: u32 = 0x07;

// header 偏移
const H_MAGIC: usize = 0;
const H_VERSION: usize = 4;
const H_GEN_A: usize = 8;
const H_GEN_B: usize = 16;
const H_APPEND_OFFSET: usize = 24;
const H_SLOT_COUNT: usize = 36;
const H_LOD: usize = 44;

// slot 偏移
const S_STATE: usize = 0;
const S_FLAGS: usize = 1;
const S_OFFSET: usize = 4;
const S_STORED_LEN: usize = 8;
const S_RAW_LEN: usize = 12;
const S_CRC_STORED: usize = 16;

fn u16be(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn u32be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn i32be(b: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

// ---------------------------------------------------------------------------
// 一张瓦片
// ---------------------------------------------------------------------------

/// 解码后的一张 64×64 颜色瓦片。
#[derive(Clone, Debug)]
pub struct Tile {
    /// 调色板（0xRRGGBB）。
    pub palette: Vec<u32>,
    /// 4096 个调色板索引；0 表示该像素无数据。
    pub indices: Vec<u8>,
    /// 可选高度（短整型，与 indices 同序）。
    pub heights: Option<Vec<i16>>,
    /// 可选群系调色板与索引。
    pub biome_palette: Option<Vec<String>>,
    pub biome_index: Option<Vec<u8>>,
}

impl Tile {
    /// 取某个像素的 RGB；`None` 表示无数据。
    pub fn pixel(&self, x: usize, z: usize) -> Option<[u8; 3]> {
        let i = z * TILE_SIZE + x;
        let idx = *self.indices.get(i)? as usize;
        if idx == 0 || idx >= self.palette.len() {
            return None;
        }
        let c = self.palette[idx];
        Some([(c >> 16) as u8, (c >> 8) as u8, c as u8])
    }

    /// 取某个像素的高度。
    pub fn height(&self, x: usize, z: usize) -> Option<i16> {
        let i = z * TILE_SIZE + x;
        self.heights.as_ref().and_then(|h| h.get(i).copied())
    }

    /// 瓦片是否有任何有效像素。
    pub fn has_data(&self) -> bool {
        self.indices.iter().any(|&i| i != 0)
    }

    /// 解码一个瓦片负载。
    pub fn decode(payload: &[u8]) -> Result<Tile> {
        if payload.len() < 2 {
            return Err(crate::nbt::Error::Other("SwiftMap 瓦片过短".into()));
        }
        let palette_size = u16be(payload, 0) as usize;
        let base = 2 + palette_size * 4 + TILE_SIZE * TILE_SIZE;
        if palette_size == 0 || palette_size > 256 || payload.len() < base {
            return Err(crate::nbt::Error::Other(format!(
                "SwiftMap 瓦片格式错误 (paletteSize={palette_size}, len={})",
                payload.len()
            )));
        }

        let mut palette = Vec::with_capacity(palette_size);
        for i in 0..palette_size {
            palette.push(u32be(payload, 2 + i * 4));
        }
        let idx_off = 2 + palette_size * 4;
        let indices = payload[idx_off..idx_off + TILE_SIZE * TILE_SIZE].to_vec();

        let mut off = base;
        // 高度段：按剩余长度判断（旧数据没有这段）
        let heights = if payload.len() >= off + TILE_SIZE * TILE_SIZE * 2 {
            let mut h = Vec::with_capacity(TILE_SIZE * TILE_SIZE);
            for i in 0..TILE_SIZE * TILE_SIZE {
                h.push(u16be(payload, off + i * 2) as i16);
            }
            off += TILE_SIZE * TILE_SIZE * 2;
            Some(h)
        } else {
            None
        };

        // 群系段（可选）
        let (biome_palette, biome_index) = if payload.len() >= off + 2 {
            let cnt = u16be(payload, off) as usize;
            if cnt > 0 && cnt <= 256 {
                let mut p = off + 2;
                let mut names = Vec::with_capacity(cnt);
                let mut ok = true;
                for _ in 0..cnt {
                    if payload.len() < p + 2 {
                        ok = false;
                        break;
                    }
                    let n = u16be(payload, p) as usize;
                    p += 2;
                    if payload.len() < p + n {
                        ok = false;
                        break;
                    }
                    names.push(String::from_utf8_lossy(&payload[p..p + n]).to_string());
                    p += n;
                }
                if ok && payload.len() >= p + TILE_SIZE * TILE_SIZE {
                    let bi = payload[p..p + TILE_SIZE * TILE_SIZE].to_vec();
                    (Some(names), Some(bi))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        Ok(Tile {
            palette,
            indices,
            heights,
            biome_palette,
            biome_index,
        })
    }
}

// ---------------------------------------------------------------------------
// .smr 容器
// ---------------------------------------------------------------------------

/// 一个 `.smr` 容器（只读）。
pub struct SmrContainer {
    path: PathBuf,
    slot_count: usize,
    data: Vec<u8>,
    // 生效的那份索引（代际更大的一份）
    gen_a: u64,
    gen_b: u64,
    index_base: usize,
}

impl SmrContainer {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let data = fs::read(&path)
            .map_err(|e| crate::nbt::Error::Other(format!("读 {} 失败: {e}", path.display())))?;

        if data.len() < HEADER_BYTES {
            return Err(crate::nbt::Error::Other(format!(
                "{} 太小，不是 .smr",
                path.display()
            )));
        }
        let magic = u32be(&data, H_MAGIC);
        if magic != MAGIC {
            return Err(crate::nbt::Error::Other(format!(
                "{} 的 magic 不是 SMR1（读到 0x{magic:08X}）",
                path.display()
            )));
        }

        let slot_count = u32be(&data, H_SLOT_COUNT) as usize;
        if slot_count == 0 || slot_count > 4096 {
            return Err(crate::nbt::Error::Other(format!(
                "{} 的 slotCount 异常: {slot_count}",
                path.display()
            )));
        }
        let gen_a = u64::from_be_bytes(data[H_GEN_A..H_GEN_A + 8].try_into().unwrap());
        let gen_b = u64::from_be_bytes(data[H_GEN_B..H_GEN_B + 8].try_into().unwrap());

        // 两份索引：A 在 0x40，B 在 0x40 + slotCount*32。
        // 代际大的那份才是当前有效的（header 的提交点语义）。
        let base_a = HEADER_BYTES;
        let base_b = HEADER_BYTES + slot_count * SLOT_BYTES;
        let index_base = if gen_b > gen_a {
            if data.len() < base_b + slot_count * SLOT_BYTES {
                base_a
            } else {
                base_b
            }
        } else {
            base_a
        };
        if data.len() < index_base + slot_count * SLOT_BYTES {
            return Err(crate::nbt::Error::Other(format!(
                "{} 的索引区被截断",
                path.display()
            )));
        }

        Ok(SmrContainer {
            path,
            slot_count,
            data,
            gen_a,
            gen_b,
            index_base,
        })
    }

    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读出某个槽位的瓦片。
    pub fn tile(&self, slot: usize) -> Result<Option<Tile>> {
        if slot >= self.slot_count {
            return Ok(None);
        }
        let o = self.index_base + slot * SLOT_BYTES;
        let state = self.data[o + S_STATE];
        if state != STATE_PRESENT {
            return Ok(None);
        }
        let flags = self.data[o + S_FLAGS];
        let offset = i32be(&self.data, o + S_OFFSET) as usize;
        let stored_len = i32be(&self.data, o + S_STORED_LEN) as usize;
        let raw_len = i32be(&self.data, o + S_RAW_LEN) as usize;
        if stored_len == 0 || offset + stored_len > self.data.len() {
            return Ok(None);
        }

        let stored = &self.data[offset..offset + stored_len];
        let raw = if flags & FLAG_COMPRESSED != 0 {
            let codec = ((flags as u32) >> FLAG_CODEC_SHIFT) & FLAG_CODEC_MASK;
            match codec {
                1 => {
                    // Deflate（zlib 包装，去掉 2 字节头）
                    use std::io::Read;
                    let mut out = Vec::with_capacity(raw_len);
                    // Java Deflater 默认 zlib 包装；flate2 的 ZlibDecoder 对应
                    let mut d = flate2::read::ZlibDecoder::new(stored);
                    d.read_to_end(&mut out).map_err(|e| {
                        crate::nbt::Error::Other(format!("SwiftMap deflate 解压失败: {e}"))
                    })?;
                    out
                }
                0 => stored.to_vec(),
                other => {
                    return Err(crate::nbt::Error::Other(format!(
                        "不支持的 SwiftMap 压缩 codec: {other}"
                    )))
                }
            }
        } else {
            stored.to_vec()
        };

        Tile::decode(&raw).map(Some)
    }

    /// 容器里有多少个有效瓦片。
    pub fn tile_count(&self) -> usize {
        (0..self.slot_count)
            .filter(|&s| {
                let o = self.index_base + s * SLOT_BYTES;
                self.data.get(o + S_STATE) == Some(&STATE_PRESENT)
            })
            .count()
    }

    pub fn generations(&self) -> (u64, u64) {
        (self.gen_a, self.gen_b)
    }
}

// ---------------------------------------------------------------------------
// 整个 SwiftMap 数据目录
// ---------------------------------------------------------------------------

/// 一个维度的 SwiftMap 数据。
pub struct DimData {
    /// 维度 id（如 `minecraft:overworld`）。
    pub dim_id: String,
    /// 目录路径。
    pub dir: PathBuf,
    /// `.smr` 容器：容器坐标 → 路径。
    pub containers: HashMap<(i32, i32), PathBuf>,
    /// `format.json` 里的 formatVersion。
    pub format_version: Option<i64>,
    pub producer: Option<String>,
}

/// 整个存档的 SwiftMap 数据。
pub struct SwiftMapData {
    pub root: PathBuf,
    pub dimensions: Vec<DimData>,
}

/// SwiftMap 数据目录的位置：`<存档>/swiftmap`。
pub fn data_root(save_dir: &Path) -> PathBuf {
    save_dir.join("swiftmap")
}

/// 存档里是否有 SwiftMap 数据。
pub fn has_data(save_dir: &Path) -> bool {
    data_root(save_dir).join("dimensions").is_dir()
}

impl SwiftMapData {
    /// 打开存档下的 SwiftMap 数据；没有则返回 `Ok(None)`。
    pub fn open(save_dir: &Path) -> Result<Option<Self>> {
        let root = data_root(save_dir);
        if !has_data(save_dir) {
            return Ok(None);
        }

        let mut dimensions = Vec::new();
        let dims_root = root.join("dimensions");
        // 布局：dimensions/<namespace>/<path>/
        if let Ok(ns_entries) = fs::read_dir(&dims_root) {
            for ns in ns_entries.flatten() {
                if !ns.path().is_dir() {
                    continue;
                }
                let ns_name = ns.file_name().to_string_lossy().to_string();
                if let Ok(path_entries) = fs::read_dir(ns.path()) {
                    for pe in path_entries.flatten() {
                        if !pe.path().is_dir() {
                            continue;
                        }
                        let path_name = pe.file_name().to_string_lossy().to_string();
                        let dim_id = format!("{ns_name}:{path_name}");
                        let dir = pe.path();

                        // 读 format.json
                        let (format_version, producer) = read_format_json(&dir);

                        // 扫 lod0 下的容器
                        let mut containers = HashMap::new();
                        let lod0 = dir.join("lod0");
                        if let Ok(files) = fs::read_dir(&lod0) {
                            for f in files.flatten() {
                                let name = f.file_name().to_string_lossy().to_string();
                                if let Some((cx, cz)) = parse_container_name(&name) {
                                    containers.insert((cx, cz), f.path());
                                }
                            }
                        }

                        dimensions.push(DimData {
                            dim_id,
                            dir,
                            containers,
                            format_version,
                            producer,
                        });
                    }
                }
            }
        }

        Ok(Some(SwiftMapData { root, dimensions }))
    }

    /// 总瓦片数（只统计文件名，不打开容器）。
    pub fn container_count(&self) -> usize {
        self.dimensions.iter().map(|d| d.containers.len()).sum()
    }

    /// 读某个维度某个区块的像素颜色。
    pub fn pixel_at(
        &self,
        dim_id: &str,
        block_x: i32,
        block_z: i32,
    ) -> Option<[u8; 3]> {
        let dim = self.dimensions.iter().find(|d| d.dim_id == dim_id)?;

        // 方块 → 瓦片（1 格 = 1 方块，LOD0）
        let tile_x = block_x.div_euclid(TILE_SIZE as i32);
        let tile_z = block_z.div_euclid(TILE_SIZE as i32);
        let cx = tile_x.div_euclid(TILES_PER_CONTAINER);
        let cz = tile_z.div_euclid(TILES_PER_CONTAINER);
        let path = dim.containers.get(&(cx, cz))?;

        let slot = (tile_z.rem_euclid(TILES_PER_CONTAINER) * TILES_PER_CONTAINER
            + tile_x.rem_euclid(TILES_PER_CONTAINER)) as usize;

        let container = SmrContainer::open(path).ok()?;
        let tile = container.tile(slot).ok()??;
        let px = block_x.rem_euclid(TILE_SIZE as i32) as usize;
        let pz = block_z.rem_euclid(TILE_SIZE as i32) as usize;
        tile.pixel(px, pz)
    }
}

/// 从 `r.<cx>.<cz>.smr` 解析容器坐标。
fn parse_container_name(name: &str) -> Option<(i32, i32)> {
    let stem = name.strip_suffix(".smr")?;
    let rest = stem.strip_prefix("r.")?;
    let mut it = rest.split('.');
    let cx = it.next()?.parse().ok()?;
    let cz = it.next()?.parse().ok()?;
    Some((cx, cz))
}

fn read_format_json(dir: &Path) -> (Option<i64>, Option<String>) {
    let Ok(text) = fs::read_to_string(dir.join("format.json")) else {
        return (None, None);
    };
    // 只做最小解析，避免为一个可选文件引入 serde
    let fv = text
        .split("\"formatVersion\"")
        .nth(1)
        .and_then(|s| s.split(':').nth(1))
        .and_then(|s| s.trim().split(|c: char| !c.is_ascii_digit() && c != '-').next())
        .and_then(|s| s.parse().ok());
    let producer = text
        .split("\"producer\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .map(|s| s.to_string());
    (fv, producer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_container_name() {
        assert_eq!(parse_container_name("r.0.0.smr"), Some((0, 0)));
        assert_eq!(parse_container_name("r.-3.7.smr"), Some((-3, 7)));
        assert_eq!(parse_container_name("r.12.-4.smr"), Some((12, -4)));
        assert_eq!(parse_container_name("bad.smr"), None);
        assert_eq!(parse_container_name("r.1.smr"), None);
    }

    #[test]
    fn tile_grid_math_matches_swiftmap() {
        // 复刻 TileGrid：区块 → 瓦片 → 容器 → 槽位
        let t = |c: i32| c.div_euclid(CHUNKS_PER_TILE);
        assert_eq!(t(0), 0);
        assert_eq!(t(3), 0);
        assert_eq!(t(4), 1);
        assert_eq!(t(-1), -1); // Java floorDiv 语义
        assert_eq!(t(-4), -1);
        assert_eq!(t(-5), -2);

        let cont = |tile: i32| tile.div_euclid(TILES_PER_CONTAINER);
        assert_eq!(cont(0), 0);
        assert_eq!(cont(-1), -1);
        assert_eq!(cont(8), 1);

        let slot = |tx: i32, tz: i32| {
            tz.rem_euclid(TILES_PER_CONTAINER) * TILES_PER_CONTAINER
                + tx.rem_euclid(TILES_PER_CONTAINER)
        };
        assert_eq!(slot(0, 0), 0);
        assert_eq!(slot(7, 0), 7);
        assert_eq!(slot(0, 7), 56);
        assert_eq!(slot(7, 7), 63);
        assert_eq!(slot(-1, -1), 63); // floorMod 语义
    }

    #[test]
    fn decodes_minimal_tile() {
        // paletteSize=2, 调色板 [0x000000, 0xFF8000], 索引起头 1,2 然后全 0
        let mut payload = Vec::new();
        payload.extend_from_slice(&2u16.to_be_bytes());
        payload.extend_from_slice(&0x000000u32.to_be_bytes());
        payload.extend_from_slice(&0xFF8000u32.to_be_bytes());
        let mut indices = vec![0u8; TILE_SIZE * TILE_SIZE];
        // 索引 0 = 无数据；索引 1 = palette[1]
        indices[0] = 1;
        indices[1] = 1;
        payload.extend_from_slice(&indices);

        let tile = Tile::decode(&payload).unwrap();
        assert_eq!(tile.palette.len(), 2);
        // palette[1] 是 0xFF8000
        assert_eq!(tile.pixel(0, 0), Some([0xFF, 0x80, 0x00]));
        assert_eq!(tile.pixel(1, 0), Some([0xFF, 0x80, 0x00]));
        assert_eq!(tile.pixel(2, 0), None, "索引 0 应表示无数据");
        assert!(tile.heights.is_none());
        assert!(tile.has_data());
    }

    #[test]
    fn rejects_corrupt_tile() {
        assert!(Tile::decode(&[]).is_err());
        assert!(Tile::decode(&[0, 0]).is_err());
        // paletteSize 很大但数据不足
        let mut bad = Vec::new();
        bad.extend_from_slice(&200u16.to_be_bytes());
        bad.extend_from_slice(&[0u8; 16]);
        assert!(Tile::decode(&bad).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let dir = std::env::temp_dir().join(format!("smr-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("bad.smr");
        std::fs::write(&p, vec![0u8; 128]).unwrap();
        assert!(SmrContainer::open(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_data_returns_none() {
        let dir = std::env::temp_dir().join(format!("swiftmap-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(SwiftMapData::open(&dir).unwrap().is_none());
        assert!(!has_data(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
