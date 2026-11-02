//! 缩略图缓存。
//!
//! # 为什么需要
//!
//! 超大存档（实测 26.3 主世界 82419 区块、末地 121616 区块）每次打开都要
//! 重新逐区块读盘采样，单区块约 5 ms，全量下来好几分钟。而大多数区块
//! 根本没过变化 —— 重复计算完全是浪费。
//!
//! # 怎么工作
//!
//! 缓存以**维度**为单位落盘，一个维度一个目录：
//!
//! ```text
//! <exe 同级>/地图缓存/<存档名>_<存档路径hash>/<维度>/
//!     meta.json      维度元信息 + 每个 region 的指纹
//!     tiles.bin      缩略图数据（每区块 16×16 的 RGBA）
//! ```
//!
//! `meta.json` 里记录每个 region 文件的 (路径, 大小, 修改时间)。
//! 下次打开时对比这三项：
//!   - 完全一致 → 该 region 内所有区块直接复用缓存
//!   - 有变化   → 只重新采样这个 region 覆盖的区块（最多 32×32）
//!
//! 用 region 而不是区块做指纹，是因为 82419 个区块逐个 stat 太慢，
//! 而 region 文件只有几百个；代价是"一个区块变了就要重算整个 region"，
//! 但一个 region 最多 1024 区块，重算也就 5 秒左右，可以接受。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 一个 region 文件的指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionStamp {
    pub rx: i32,
    pub rz: i32,
    /// 文件字节数。
    pub size: u64,
    /// 修改时间（Unix 秒）。
    pub mtime: i64,
}

/// 缓存的元信息。
#[derive(Clone, Debug, Default)]
pub struct CacheMeta {
    pub dim_id: String,
    /// 采样时用的切片方式（Surface / Fixed(y)）。换了方式缓存就作废。
    pub slice: String,
    /// 存档的 DataVersion，版本变了配色/方块可能都变了。
    pub data_version: i32,
    /// 各 region 的指纹。
    pub regions: Vec<RegionStamp>,
    /// 缓存里包含的区块（cx, cz）。
    pub chunks: Vec<(i32, i32)>,
}

impl CacheMeta {
    /// 与当前磁盘状态对比，返回"哪些 region 变了"。
    pub fn changed_regions(&self, current: &[RegionStamp]) -> Vec<(i32, i32)> {
        let old: HashMap<(i32, i32), &RegionStamp> =
            self.regions.iter().map(|s| ((s.rx, s.rz), s)).collect();

        // 新的 region 或者指纹变了
        let mut out: Vec<(i32, i32)> = current
            .iter()
            .filter(|s| match old.get(&(s.rx, s.rz)) {
                Some(prev) => *prev != *s,
                None => true,
            })
            .map(|s| (s.rx, s.rz))
            .collect();

        // 被删掉的 region 也要标记（让调用方知道这些区块已经不存在了）
        for s in &self.regions {
            if !current.iter().any(|c| c.rx == s.rx && c.rz == s.rz) {
                out.push((s.rx, s.rz));
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// 缓存根目录：<exe 同级>/地图缓存/
///
/// 用 exe 所在目录而不是用户目录，符合"绿色软件、拷走就能用"的预期。
/// 找不到 exe 路径（比如开发时）就退回当前工作目录。
pub fn cache_root() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("地图缓存")
}

/// 某个存档的缓存目录：<cache_root>/<存档名>_<路径hash>/
///
/// 带上路径 hash，避免两个同名存档互相覆盖。
pub fn save_cache_dir(save_root: &Path) -> PathBuf {
    let name = save_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("world");
    // 简单 FNV-1a，避免引入额外依赖
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in save_root.to_string_lossy().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    cache_root().join(format!("{name}_{:08x}", (h >> 32) as u32))
}

/// 采集某个维度目录下所有 region 文件的指纹。
pub fn collect_stamps(region_dir: &Path) -> Vec<RegionStamp> {
    let Ok(entries) = mcworld_core::anvil::list_region_files(region_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (rx, rz, path) in entries {
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        let size = md.len();
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(RegionStamp { rx, rz, size, mtime });
    }
    out.sort_unstable_by_key(|s| (s.rz, s.rx));
    out
}

// ---------------------------------------------------------------------------
// 磁盘格式
// ---------------------------------------------------------------------------

const TILE: usize = 16;
const TILE_BYTES: usize = TILE * TILE * 4;

/// 保存缓存：meta.json + tiles.bin。
///
/// tiles.bin 的布局：按 `meta.chunks` 的顺序，每区块 TILE*TILE*4 字节 RGBA。
pub fn save(
    dir: &Path,
    meta: &CacheMeta,
    tiles: &[Vec<u8>],
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;

    // tiles.bin
    let mut bin = Vec::with_capacity(tiles.len() * TILE_BYTES);
    for t in tiles {
        if t.len() != TILE_BYTES {
            return Err(format!("缩略图尺寸不对: {} != {TILE_BYTES}", t.len()));
        }
        bin.extend_from_slice(t);
    }
    std::fs::write(dir.join("tiles.bin"), &bin)
        .map_err(|e| format!("写入 tiles.bin 失败: {e}"))?;

    // meta.json（手写，不引 serde）
    let mut js = String::new();
    js.push_str("{\n");
    js.push_str(&format!("  \"dim\": {},\n", json_str(&meta.dim_id)));
    js.push_str(&format!("  \"slice\": {},\n", json_str(&meta.slice)));
    js.push_str(&format!("  \"data_version\": {},\n", meta.data_version));
    js.push_str("  \"regions\": [\n");
    for (i, r) in meta.regions.iter().enumerate() {
        js.push_str(&format!(
            "    {{\"rx\":{},\"rz\":{},\"size\":{},\"mtime\":{}}}{}\n",
            r.rx,
            r.rz,
            r.size,
            r.mtime,
            if i + 1 < meta.regions.len() { "," } else { "" }
        ));
    }
    js.push_str("  ],\n");
    js.push_str("  \"chunks\": [\n");
    for (i, (cx, cz)) in meta.chunks.iter().enumerate() {
        js.push_str(&format!(
            "    [{},{}]{}\n",
            cx,
            cz,
            if i + 1 < meta.chunks.len() { "," } else { "" }
        ));
    }
    js.push_str("  ]\n}\n");

    std::fs::write(dir.join("meta.json"), js)
        .map_err(|e| format!("写入 meta.json 失败: {e}"))?;
    Ok(())
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 读取缓存。返回 (meta, 每区块的缩略图)。
///
/// 任何一项对不上就返回 None（当作没有缓存，重新采样）。
pub fn load(dir: &Path) -> Option<(CacheMeta, Vec<Vec<u8>>)> {
    let js = std::fs::read_to_string(dir.join("meta.json")).ok()?;
    let meta = parse_meta(&js)?;

    let bin = std::fs::read(dir.join("tiles.bin")).ok()?;
    if bin.len() != meta.chunks.len() * TILE_BYTES {
        // 尺寸对不上说明缓存损坏，放弃
        return None;
    }
    let tiles: Vec<Vec<u8>> = bin
        .chunks_exact(TILE_BYTES)
        .map(|c| c.to_vec())
        .collect();
    Some((meta, tiles))
}

// ---------------------------------------------------------------------------
// 极简 JSON 解析（只认我们自己写的格式）
// ---------------------------------------------------------------------------

fn parse_meta(js: &str) -> Option<CacheMeta> {
    let mut meta = CacheMeta::default();

    meta.dim_id = extract_str(js, "\"dim\"")?;
    meta.slice = extract_str(js, "\"slice\"").unwrap_or_default();
    meta.data_version = extract_int(js, "\"data_version\"").unwrap_or(0) as i32;

    // regions: 逐个 {"rx":..,"rz":..,"size":..,"mtime":..}
    let reg_block = slice_between(js, "\"regions\"", "\"chunks\"")?;
    for obj in reg_block.split('{').skip(1) {
        let body = obj.split('}').next()?;
        let rx = extract_int(body, "\"rx\"")? as i32;
        let rz = extract_int(body, "\"rz\"")? as i32;
        let size = extract_int(body, "\"size\"")? as u64;
        let mtime = extract_int(body, "\"mtime\"")?;
        meta.regions.push(RegionStamp { rx, rz, size, mtime });
    }

    // chunks: [[cx,cz], ...]
    let ch_block = slice_between(js, "\"chunks\"", "\n}").or_else(|| Some(js))?;
    let mut i = 0;
    let bytes = ch_block.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // 解析一对数字
            let mut j = i + 1;
            let mut nums = Vec::new();
            let mut cur = String::new();
            while j < bytes.len() {
                let c = bytes[j] as char;
                if c.is_ascii_digit() || c == '-' {
                    cur.push(c);
                } else if c == ',' || c == ']' {
                    if !cur.is_empty() {
                        nums.push(cur.parse::<i32>().ok()?);
                        cur.clear();
                    }
                    if c == ']' {
                        break;
                    }
                }
                j += 1;
            }
            if nums.len() == 2 {
                meta.chunks.push((nums[0], nums[1]));
            }
            i = j;
        }
        i += 1;
    }

    Some(meta)
}

fn slice_between<'a>(s: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let a = s.find(from)?;
    let rest = &s[a..];
    let b = rest.find(to)?;
    Some(&rest[..b])
}

fn extract_str(s: &str, key: &str) -> Option<String> {
    let a = s.find(key)?;
    let rest = &s[a + key.len()..];
    let q1 = rest.find('"')?;
    let after = &rest[q1 + 1..];
    let q2 = after.find('"')?;
    Some(after[..q2].to_string())
}

fn extract_int(s: &str, key: &str) -> Option<i64> {
    let a = s.find(key)?;
    let rest = &s[a + key.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let mut out = String::new();
    for c in after.chars() {
        if c.is_ascii_digit() || c == '-' {
            out.push(c);
        } else if !out.is_empty() {
            break;
        } else if c == ' ' {
            continue;
        } else {
            break;
        }
    }
    out.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let meta = CacheMeta {
            dim_id: "minecraft:overworld".into(),
            slice: "surface".into(),
            data_version: 3955,
            regions: vec![
                RegionStamp { rx: 0, rz: 0, size: 4096, mtime: 100 },
                RegionStamp { rx: -1, rz: 2, size: 8192, mtime: 200 },
            ],
            chunks: vec![(0, 0), (1, 0), (-5, 3)],
        };
        let dir = std::env::temp_dir().join("mcworld_cache_test");
        let _ = std::fs::remove_dir_all(&dir);

        let tiles: Vec<Vec<u8>> = (0..3).map(|i| vec![i as u8; TILE_BYTES]).collect();
        save(&dir, &meta, &tiles).unwrap();

        let (got, got_tiles) = load(&dir).expect("应当能读回");
        assert_eq!(got.dim_id, meta.dim_id);
        assert_eq!(got.data_version, meta.data_version);
        assert_eq!(got.regions.len(), 2);
        assert_eq!(got.regions[1].rx, -1);
        assert_eq!(got.chunks, meta.chunks);
        assert_eq!(got_tiles.len(), 3);
        assert_eq!(got_tiles[2][0], 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_regions_detects_size_and_mtime() {
        let meta = CacheMeta {
            regions: vec![RegionStamp { rx: 0, rz: 0, size: 100, mtime: 1 }],
            ..Default::default()
        };

        // 完全一样 → 无变化
        let same = vec![RegionStamp { rx: 0, rz: 0, size: 100, mtime: 1 }];
        assert!(meta.changed_regions(&same).is_empty());

        // mtime 变了
        let touched = vec![RegionStamp { rx: 0, rz: 0, size: 100, mtime: 2 }];
        assert_eq!(meta.changed_regions(&touched), vec![(0, 0)]);

        // 多了新 region
        let added = vec![
            RegionStamp { rx: 0, rz: 0, size: 100, mtime: 1 },
            RegionStamp { rx: 1, rz: 0, size: 50, mtime: 5 },
        ];
        assert_eq!(meta.changed_regions(&added), vec![(1, 0)]);
    }
}
