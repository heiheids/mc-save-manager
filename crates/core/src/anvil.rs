//! Anvil（.mca）区域文件读写。
//!
//! 文件结构：
//! ```text
//! [0x0000, 0x1000)  定位表  1024 × u32 = offset(3B, 扇区) + sectors(1B)
//! [0x1000, 0x2000)  时间戳表 1024 × u32 (Unix 秒)
//! [0x2000, ...)     区块体    4B 长度 + 1B 压缩方式 + 数据，按 4KiB 对齐
//! ```
//!
//! 关键约束：整个文件必须是 4096 的整数倍；最后一块也要补齐。
//! 本模块的 `save` 会**重新紧凑排布**所有区块，顺带消除碎片与空洞。
//!
//! **重要**：`Slot::data` 始终保存**解压后**的 NBT 字节。
//! 早期版本在此处误存了压缩数据，导致「打开→保存」会把数据压缩两遍而损坏存档；
//! 现已改为惰性解压，并由 `open_then_save_is_lossless` 测试守护。

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;

use crate::nbt::{self, Tag};

pub const SECTOR: usize = 4096;
pub const HEADER_SECTORS: usize = 2;
/// 单区块最大占用扇区数（offset 3 字节 + sectors 1 字节，故上限 255）。
pub const MAX_SECTORS_PER_CHUNK: u8 = 255;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("region 文件过小，不足 8KiB 头: {0}")]
    TooSmall(PathBuf),
    #[error("区块 ({x}, {z}) 压缩方式 {comp} 不支持")]
    UnsupportedCompression { x: i32, z: i32, comp: u8 },
    #[error("区块 ({x}, {z}) 长度字段非法: {len}")]
    BadLength { x: i32, z: i32, len: u32 },
    #[error("区块 ({x}, {z}) 数据越界")]
    Truncated { x: i32, z: i32 },
    #[error("压缩/解压失败: {0}")]
    Compression(String),
    #[error("NBT 解析失败: {0}")]
    Nbt(#[from] nbt::Error),
    #[error("IO 错误 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io { path: path.to_path_buf(), source }
}

// ---------------------------------------------------------------------------
// 坐标换算
// ---------------------------------------------------------------------------

/// 区块坐标 → 该区块所属 region 文件坐标（向负无穷取整）。
///
/// 必须用 `div_euclid`：Rust 的 `/` 对负数是向零截断，会把 chunk -1 错算进 region 0。
#[inline]
pub fn region_coord(chunk: i32) -> i32 {
    chunk.div_euclid(32)
}

/// 区块坐标 → region 内的局部索引 0..1024。
#[inline]
pub fn local_index(chunk_x: i32, chunk_z: i32) -> usize {
    let lx = chunk_x.rem_euclid(32) as usize;
    let lz = chunk_z.rem_euclid(32) as usize;
    lx + lz * 32
}

#[inline]
pub fn local_coords(chunk_x: i32, chunk_z: i32) -> (usize, usize) {
    (chunk_x.rem_euclid(32) as usize, chunk_z.rem_euclid(32) as usize)
}

/// 由 region 坐标与局部坐标还原绝对区块坐标。
#[inline]
pub fn abs_chunk(region_x: i32, region_z: i32, local_x: usize, local_z: usize) -> (i32, i32) {
    (region_x * 32 + local_x as i32, region_z * 32 + local_z as i32)
}

/// region 文件路径（如 `r.0.-1.mca`）。
pub fn region_file_name(rx: i32, rz: i32) -> String {
    format!("r.{rx}.{rz}.mca")
}

/// 外置区块 `.mcc` 路径（区块超 255 扇区时使用）。
pub fn external_chunk_name(chunk_x: i32, chunk_z: i32) -> String {
    format!("c.{chunk_x}.{chunk_z}.mcc")
}

// ---------------------------------------------------------------------------
// 压缩
// ---------------------------------------------------------------------------

pub const COMPRESSION_GZIP: u8 = 1;
pub const COMPRESSION_ZLIB: u8 = 2;
pub const COMPRESSION_NONE: u8 = 3;
pub const COMPRESSION_LZ4: u8 = 4;
/// 高位置 1 表示数据在外部 `.mcc` 文件里。
pub const COMPRESSION_EXTERNAL_FLAG: u8 = 128;

fn decompress(comp: u8, data: &[u8]) -> Result<Vec<u8>> {
    match comp {
        COMPRESSION_GZIP => {
            let mut out = Vec::new();
            GzDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| Error::Compression(format!("gzip: {e}")))?;
            Ok(out)
        }
        COMPRESSION_ZLIB => {
            let mut out = Vec::new();
            ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| Error::Compression(format!("zlib: {e}")))?;
            Ok(out)
        }
        COMPRESSION_NONE => Ok(data.to_vec()),
        COMPRESSION_LZ4 => {
            // Minecraft 1.20.5+ 用的是 LZ4 **frame** 格式（非 block 格式）
            let mut out = Vec::new();
            let mut decoder = lz4_flex::frame::FrameDecoder::new(data);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::Compression(format!("lz4: {e}")))?;
            Ok(out)
        }
        other => Err(Error::Other(format!("未知压缩方式 {other}"))),
    }
}

fn compress(comp: u8, data: &[u8]) -> Result<Vec<u8>> {
    match comp {
        COMPRESSION_GZIP => {
            let mut enc = GzEncoder::new(Vec::new(), Compression::new(6));
            enc.write_all(data).map_err(|e| Error::Compression(e.to_string()))?;
            enc.finish().map_err(|e| Error::Compression(e.to_string()))
        }
        COMPRESSION_ZLIB => {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
            enc.write_all(data).map_err(|e| Error::Compression(e.to_string()))?;
            enc.finish().map_err(|e| Error::Compression(e.to_string()))
        }
        COMPRESSION_NONE => Ok(data.to_vec()),
        other => Err(Error::Other(format!("不支持写入的压缩方式 {other}"))),
    }
}

// ---------------------------------------------------------------------------
// RegionFile
// ---------------------------------------------------------------------------

/// 一个区块的槽位。
#[derive(Clone, Debug)]
struct Slot {
    /// 磁盘上的扇区偏移。
    offset: u32,
    /// 磁盘占用扇区数。
    sectors: u8,
    timestamp: u32,
    /// 压缩方式（含 external 标志位）。
    compression: u8,
    /// **解压后**的 NBT 体；`None` 表示尚未从磁盘加载。
    data: Option<Vec<u8>>,
}

/// 一个 region 文件的内存表示。
///
/// 采用「惰性加载 + 脏标记」：只有被读取或修改的区块才会解压到内存，
/// 保存时重排所有区块并紧凑写盘。
#[derive(Debug)]
pub struct RegionFile {
    path: PathBuf,
    region_x: i32,
    region_z: i32,
    slots: HashMap<usize, Slot>,
    dirty: bool,
}

impl RegionFile {
    /// 打开（或新建）一个 region 文件。文件不存在时返回空表格，便于后续写入。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (region_x, region_z) = parse_region_name(&path)
            .ok_or_else(|| Error::Other(format!("无法从文件名解析 region 坐标: {}", path.display())))?;

        let mut rf = RegionFile {
            path: path.clone(),
            region_x,
            region_z,
            slots: HashMap::new(),
            dirty: false,
        };

        if !path.exists() {
            return Ok(rf);
        }

        let meta = fs::metadata(&path).map_err(|e| io_err(&path, e))?;
        // 0 字节文件是常见的"空 region"，按空表处理
        if meta.len() == 0 {
            return Ok(rf);
        }
        if meta.len() < (HEADER_SECTORS * SECTOR) as u64 {
            return Err(Error::TooSmall(path));
        }

        let mut buf = Vec::with_capacity(meta.len() as usize);
        fs::File::open(&path)
            .map_err(|e| io_err(&path, e))?
            .read_to_end(&mut buf)
            .map_err(|e| io_err(&path, e))?;

        for i in 0..1024usize {
            let b = &buf[i * 4..i * 4 + 4];
            let offset = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
            let sectors = b[3];
            if offset == 0 && sectors == 0 {
                continue;
            }
            if offset < HEADER_SECTORS as u32 {
                continue; // 与头部重叠，视为损坏槽位，忽略
            }
            let ts_off = SECTOR + i * 4;
            let timestamp = u32::from_be_bytes([
                buf[ts_off],
                buf[ts_off + 1],
                buf[ts_off + 2],
                buf[ts_off + 3],
            ]);
            let body_start = offset as usize * SECTOR;
            if body_start + 5 > buf.len() {
                continue; // 越界槽位：丢弃而不是报错，保证能救回其余数据
            }
            let comp = buf[body_start + 4];
            rf.slots.insert(
                i,
                Slot { offset, sectors, timestamp, compression: comp, data: None },
            );
        }
        Ok(rf)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn region_pos(&self) -> (i32, i32) {
        (self.region_x, self.region_z)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 已存在的区块索引（0..1024）。
    pub fn indices(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.slots.keys().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn contains(&self, chunk_x: i32, chunk_z: i32) -> bool {
        self.slots.contains_key(&local_index(chunk_x, chunk_z))
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 从磁盘读取并解压一个槽位的数据。
    fn load_slot(&self, idx: usize) -> Result<Vec<u8>> {
        let slot = self
            .slots
            .get(&idx)
            .ok_or_else(|| Error::Other(format!("槽位 {idx} 不存在")))?;
        let comp = slot.compression & !COMPRESSION_EXTERNAL_FLAG;
        let (cx, cz) = abs_chunk(self.region_x, self.region_z, idx % 32, idx / 32);

        if slot.compression & COMPRESSION_EXTERNAL_FLAG != 0 {
            // 外置：真正的数据在 c.x.z.mcc，压缩方式用于该文件内容
            let ext_path = self
                .path
                .parent()
                .map(|p| p.join(external_chunk_name(cx, cz)))
                .unwrap_or_else(|| PathBuf::from(external_chunk_name(cx, cz)));
            let mut ext = Vec::new();
            fs::File::open(&ext_path)
                .map_err(|e| io_err(&ext_path, e))?
                .read_to_end(&mut ext)
                .map_err(|e| io_err(&ext_path, e))?;
            return if comp == COMPRESSION_NONE { Ok(ext) } else { decompress(comp, &ext) };
        }

        let mut f = fs::File::open(&self.path).map_err(|e| io_err(&self.path, e))?;
        let mut head = [0u8; 5];
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(slot.offset as u64 * SECTOR as u64))
            .map_err(|e| io_err(&self.path, e))?;
        f.read_exact(&mut head).map_err(|e| io_err(&self.path, e))?;
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let data_len = (len as usize).saturating_sub(1);
        if data_len > slot.sectors as usize * SECTOR {
            return Err(Error::Truncated { x: cx, z: cz });
        }
        let mut data = vec![0u8; data_len];
        f.read_exact(&mut data).map_err(|e| io_err(&self.path, e))?;
        if comp == COMPRESSION_NONE {
            Ok(data)
        } else {
            decompress(comp, &data)
        }
    }

    /// 读取区块解压后的 NBT 字节（必要时从磁盘解压并缓存）。
    pub fn raw(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Option<Vec<u8>>> {
        let idx = local_index(chunk_x, chunk_z);
        let Some(slot) = self.slots.get(&idx) else {
            return Ok(None);
        };
        if let Some(d) = &slot.data {
            return Ok(Some(d.clone()));
        }
        let data = self.load_slot(idx)?;
        if let Some(s) = self.slots.get_mut(&idx) {
            s.data = Some(data.clone());
        }
        Ok(Some(data))
    }

    /// 读取并解析区块 NBT。
    pub fn chunk(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Option<Tag>> {
        match self.raw(chunk_x, chunk_z)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(nbt::read(&bytes)?)),
        }
    }

    /// 写入一个区块（覆盖或新增）。传入的是已解析好的 NBT 根标签。
    pub fn put_chunk(&mut self, chunk_x: i32, chunk_z: i32, tag: &Tag) -> Result<()> {
        let bytes = nbt::write(tag)?;
        self.put_raw(chunk_x, chunk_z, bytes)
    }

    /// 写入一个区块的 NBT 字节（**未压缩**）。
    pub fn put_raw(&mut self, chunk_x: i32, chunk_z: i32, nbt_bytes: Vec<u8>) -> Result<()> {
        let idx = local_index(chunk_x, chunk_z);
        self.slots.insert(
            idx,
            Slot {
                offset: 0,
                sectors: 1,
                timestamp: now_unix(),
                compression: COMPRESSION_ZLIB,
                data: Some(nbt_bytes),
            },
        );
        self.dirty = true;
        Ok(())
    }

    /// 删除区块。返回是否真的删掉了东西。
    pub fn remove_chunk(&mut self, chunk_x: i32, chunk_z: i32) -> bool {
        let idx = local_index(chunk_x, chunk_z);
        let existed = self.slots.remove(&idx).is_some();
        if existed {
            self.dirty = true;
        }
        existed
    }

    pub fn timestamp(&self, chunk_x: i32, chunk_z: i32) -> Option<u32> {
        self.slots.get(&local_index(chunk_x, chunk_z)).map(|s| s.timestamp)
    }

    /// 紧凑写盘：重排所有区块、回收碎片、修正头部。
    ///
    /// 采用「临时文件 + 原子替换」，避免中途崩溃留下半截存档。
    /// 写入后整个文件长度严格是 4096 的整数倍，且不留空洞。
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let indices = self.indices();
        let mut externals: Vec<(i32, i32, Vec<u8>)> = Vec::new();

        // 先把所有槽位的数据准备好（惰性的从磁盘加载）
        let mut prepared: Vec<(usize, u32, Option<Vec<u8>>)> = Vec::with_capacity(indices.len());
        for idx in indices {
            let ts = self.slots[&idx].timestamp;
            if self.slots[&idx].data.is_none() {
                let data = self.load_slot(idx)?;
                self.slots.get_mut(&idx).unwrap().data = Some(data);
            }
            let raw = self.slots[&idx].data.clone().unwrap_or_default();
            prepared.push((idx, ts, Some(raw)));
        }

        // 计算布局
        let mut header = vec![0u8; SECTOR * HEADER_SECTORS];
        let mut payload: Vec<u8> = Vec::new();
        let mut next_sector = HEADER_SECTORS as u32;

        for (idx, ts, raw) in &prepared {
            let raw = raw.as_deref().unwrap_or(&[]);
            let compressed = compress(COMPRESSION_ZLIB, raw)?;
            let total_len = compressed.len() + 5;
            let needed_sectors = total_len.div_ceil(SECTOR);

            let (body, sectors) = if needed_sectors > MAX_SECTORS_PER_CHUNK as usize {
                // 超限 → 转外置 .mcc
                let (cx, cz) = abs_chunk(self.region_x, self.region_z, idx % 32, idx / 32);
                externals.push((cx, cz, compressed));
                let mut b = Vec::with_capacity(5);
                b.extend_from_slice(&1u32.to_be_bytes());
                b.push(COMPRESSION_ZLIB | COMPRESSION_EXTERNAL_FLAG);
                (b, 1u8)
            } else {
                let mut b = Vec::with_capacity(total_len);
                b.extend_from_slice(&((compressed.len() + 1) as u32).to_be_bytes());
                b.push(COMPRESSION_ZLIB);
                b.extend_from_slice(&compressed);
                (b, needed_sectors as u8)
            };

            let off = next_sector;
            header[idx * 4..idx * 4 + 3].copy_from_slice(&[
                ((off >> 16) & 0xFF) as u8,
                ((off >> 8) & 0xFF) as u8,
                (off & 0xFF) as u8,
            ]);
            header[idx * 4 + 3] = sectors;
            header[SECTOR + idx * 4..SECTOR + idx * 4 + 4].copy_from_slice(&ts.to_be_bytes());

            payload.extend_from_slice(&body);
            payload.resize(payload.len().div_ceil(SECTOR) * SECTOR, 0);
            next_sector += sectors as u32;
        }

        // 写临时文件再原子替换
        let tmp = self.path.with_extension("mca.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
            f.write_all(&header).map_err(|e| io_err(&tmp, e))?;
            f.write_all(&payload).map_err(|e| io_err(&tmp, e))?;
            f.sync_all().map_err(|e| io_err(&tmp, e))?;
        }
        fs::rename(&tmp, &self.path).map_err(|e| io_err(&self.path, e))?;

        // 外置区块文件
        if let Some(dir) = self.path.parent() {
            for (cx, cz, data) in &externals {
                let p = dir.join(external_chunk_name(*cx, *cz));
                fs::write(&p, data).map_err(|e| io_err(&p, e))?;
            }
            // 清理已删除区块遗留的 .mcc
            for idx in 0..1024usize {
                if self.slots.contains_key(&idx) {
                    continue;
                }
                let (cx, cz) = abs_chunk(self.region_x, self.region_z, idx % 32, idx / 32);
                let p = dir.join(external_chunk_name(cx, cz));
                if p.exists() {
                    let _ = fs::remove_file(p);
                }
            }
        }

        self.dirty = false;
        Ok(())
    }
}

fn now_unix() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// 从 `r.<x>.<z>.mca` 解析 region 坐标。
pub fn parse_region_name(path: &Path) -> Option<(i32, i32)> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".mca")?;
    let mut it = stem.split('.');
    if it.next()? != "r" {
        return None;
    }
    let x = it.next()?.parse::<i32>().ok()?;
    let z = it.next()?.parse::<i32>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((x, z))
}

/// 扫描目录下所有 region 文件坐标。
pub fn list_region_files(dir: &Path) -> std::io::Result<Vec<(i32, i32, PathBuf)>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("mca") {
            continue;
        }
        if let Some((x, z)) = parse_region_name(&path) {
            out.push((x, z, path));
        }
    }
    out.sort_by_key(|(x, z, _)| (*x, *z));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::{Compound, Tag, TagType};

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcworld-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample_chunk(x: i32, z: i32) -> Tag {
        let mut c = Compound::new();
        c.insert("xPos".into(), Tag::Int(x));
        c.insert("zPos".into(), Tag::Int(z));
        c.insert("Name".into(), Tag::String("测试".into()));
        c.insert(
            "sections".into(),
            Tag::new_list(TagType::Compound, vec![Tag::Compound(Compound::new())]),
        );
        Tag::Compound(c)
    }

    #[test]
    fn negative_coords_use_euclid() {
        assert_eq!(region_coord(-1), -1);
        assert_eq!(region_coord(-32), -1);
        assert_eq!(region_coord(-33), -2);
        assert_eq!(region_coord(31), 0);
        assert_eq!(region_coord(32), 1);

        assert_eq!(local_index(-1, -1), 31 + 31 * 32);
        assert_eq!(local_coords(-1, -1), (31, 31));
        assert_eq!(abs_chunk(-1, -1, 31, 31), (-1, -1));
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let path = dir.join("r.0.0.mca");

        let mut rf = RegionFile::open(&path).unwrap();
        assert!(rf.is_empty());

        rf.put_chunk(3, 5, &sample_chunk(3, 5)).unwrap();
        rf.save().unwrap();

        let mut back = RegionFile::open(&path).unwrap();
        let tag = back.chunk(3, 5).unwrap().unwrap();
        let comp = tag.as_compound().unwrap();
        assert_eq!(comp["xPos"].as_i32(), Some(3));
        assert_eq!(comp["Name"].as_str(), Some("测试"));

        assert!(back.chunk(4, 5).unwrap().is_none());

        let size = fs::metadata(&path).unwrap().len();
        assert_eq!(size % SECTOR as u64, 0, "文件未按 4KiB 对齐");
    }

    /// **回归测试**：打开一个已有 region 文件后不做任何修改就保存，
    /// 内容必须仍然可读且完全一致。
    ///
    /// 早期实现把压缩数据当成"已解压数据"缓存，保存时又压缩一次，
    /// 会静默损坏每一个区块——这个测试守护该行为。
    #[test]
    fn open_then_save_is_lossless() {
        let dir = tmp_dir("lossless");
        let path = dir.join("r.0.0.mca");

        // 先写入多个区块
        let mut rf = RegionFile::open(&path).unwrap();
        for i in 0..5 {
            rf.put_chunk(i, 0, &sample_chunk(i, 0)).unwrap();
        }
        rf.save().unwrap();
        let original: Vec<Vec<u8>> = {
            let mut r = RegionFile::open(&path).unwrap();
            (0..5).map(|i| r.raw(i, 0).unwrap().unwrap()).collect()
        };

        // 打开 → 标记脏 → 保存（不改变任何数据）
        let mut rf = RegionFile::open(&path).unwrap();
        rf.dirty = true;
        rf.save().unwrap();

        let mut r = RegionFile::open(&path).unwrap();
        for i in 0..5 {
            let now = r.raw(i, 0).unwrap().unwrap();
            assert_eq!(now, original[i as usize], "区块 {i} 在无改动保存后发生了变化");
            assert!(r.chunk(i, 0).unwrap().is_some(), "区块 {i} 应仍可解析");
        }
    }

    /// 回归测试：对真实存档中已存在的文件打开后原样保存，必须仍可解析。
    #[test]
    fn save_without_modification_keeps_all_chunks_readable() {
        let dir = tmp_dir("noclobber");
        let path = dir.join("r.-1.-1.mca");
        let mut rf = RegionFile::open(&path).unwrap();
        for (i, (x, z)) in [(0, 0), (5, 3), (31, 31), (12, 7)].iter().enumerate() {
            let mut c = Compound::new();
            c.insert("xPos".into(), Tag::Int(*x));
            c.insert("zPos".into(), Tag::Int(*z));
            c.insert("idx".into(), Tag::Int(i as i32));
            rf.put_chunk(*x, *z, &Tag::Compound(c)).unwrap();
        }
        rf.save().unwrap();
        let count_before = RegionFile::open(&path).unwrap().len();

        let mut rf = RegionFile::open(&path).unwrap();
        rf.dirty = true;
        rf.save().unwrap();

        let mut back = RegionFile::open(&path).unwrap();
        assert_eq!(back.len(), count_before, "区块数量不应变化");
        for (x, z) in [(0, 0), (5, 3), (31, 31), (12, 7)] {
            assert!(back.chunk(x, z).unwrap().is_some(), "区块 ({x},{z}) 应仍存在");
        }
    }

    #[test]
    fn remove_chunk_rewrites_compactly() {
        let dir = tmp_dir("remove");
        let path = dir.join("r.0.0.mca");
        let mut rf = RegionFile::open(&path).unwrap();
        for i in 0..10 {
            rf.put_chunk(i, 0, &sample_chunk(i, 0)).unwrap();
        }
        rf.save().unwrap();
        let size_before = fs::metadata(&path).unwrap().len();

        let mut rf = RegionFile::open(&path).unwrap();
        for i in 0..10 {
            assert!(rf.remove_chunk(i, 0));
        }
        rf.save().unwrap();
        let size_after = fs::metadata(&path).unwrap().len();
        assert!(size_after < size_before);
        assert_eq!(size_after, (HEADER_SECTORS * SECTOR) as u64);

        let back = RegionFile::open(&path).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn zero_byte_file_is_empty_region() {
        let dir = tmp_dir("zerobyte");
        let path = dir.join("r.1.1.mca");
        fs::write(&path, b"").unwrap();
        let rf = RegionFile::open(&path).unwrap();
        assert!(rf.is_empty());
    }

    #[test]
    fn large_chunk_goes_external() {
        let dir = tmp_dir("external");
        let path = dir.join("r.0.0.mca");
        let mut rf = RegionFile::open(&path).unwrap();

        // 构造一个**不可压缩**的区块：伪随机序列，压缩后仍需超过 255 扇区（约 1MB）
        let mut state = 0x1234_5678u32;
        let blob: Vec<u8> = (0..1_400_000u32)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();
        let mut c = Compound::new();
        c.insert("Blob".into(), Tag::ByteArray(blob.clone()));
        rf.put_chunk(1, 1, &Tag::Compound(c)).unwrap();
        rf.save().unwrap();

        assert!(dir.join("c.1.1.mcc").exists(), "超限区块应转为外置 .mcc");

        let mut back = RegionFile::open(&path).unwrap();
        let tag = back.chunk(1, 1).unwrap().unwrap();
        let stored = tag.as_compound().unwrap()["Blob"].as_byte_array().unwrap();
        assert_eq!(stored, &blob[..], "外置区块内容往返不一致");
    }

    #[test]
    fn parse_names() {
        assert_eq!(parse_region_name(Path::new("r.-3.7.mca")), Some((-3, 7)));
        assert_eq!(parse_region_name(Path::new("r.0.0.mcr")), None);
        assert_eq!(parse_region_name(Path::new("entities/r.0.0.mca")), Some((0, 0)));
        assert_eq!(parse_region_name(Path::new("x.0.0.mca")), None);
    }

    #[test]
    fn corrupt_offset_is_skipped_not_fatal() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("r.0.0.mca");
        let mut rf = RegionFile::open(&path).unwrap();
        rf.put_chunk(0, 0, &sample_chunk(0, 0)).unwrap();
        rf.save().unwrap();

        // 把索引 1 的指针指向文件外
        let mut bytes = fs::read(&path).unwrap();
        bytes[4] = 0xFF;
        bytes[5] = 0xFF;
        bytes[6] = 0xFF;
        bytes[7] = 1;
        fs::write(&path, &bytes).unwrap();

        let rf = RegionFile::open(&path).unwrap();
        // 损坏槽位被忽略，正常的仍在
        assert_eq!(rf.len(), 1);
    }
}
