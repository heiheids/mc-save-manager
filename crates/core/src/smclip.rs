//! SwiftMap `.smclip` 剪贴板格式的读写。
//!
//! 格式来源：`D:\工程-迅捷地图\src\client\java\com\heishandashu\swiftmap\clip\`
//! （`ClipFormat` / `ClipHeader` / `ClipWriter` / `CaptureTask` / `PackedArray`）。
//!
//! ## 文件结构
//!
//! ```text
//! [4]   magic "SMCP"
//! [2]   formatVersion (short)
//! [4]   dataVersion   (int)
//! [UTF] modVersion
//! [8]   createdMillis (long)
//! [UTF] note
//! [UTF] sourceWorld
//! [UTF] dimension
//! [4]   originChunkX  (int)
//! [4]   originChunkZ  (int)
//! [2]   sizeX         (short)
//! [2]   sizeZ         (short)
//! [2]   minY          (short)
//! [2]   maxY          (short)
//! [1]   flags         (byte)
//! [4]   blockPaletteCount
//! [UTF × count] blockPalette
//! [4]   biomePaletteCount
//! [UTF × count] biomePalette
//! -- 之后是 sizeX*sizeZ 个区块载荷 --
//! [4] rawLen
//! [4] compressedLen
//! [compressedLen] Deflate 压缩的载荷
//! ```
//!
//! `UTF` 是 Java 的 `DataOutputStream.writeUTF`：**2 字节无符号长度 + Modified UTF-8**。
//!
//! ## 单个区块载荷
//!
//! ```text
//! [2] sectionCount
//! [2] sectionBaseY      第一段的基准 Y（16 对齐）
//! 每段：
//!   [1] kind            0=全空气  1=有方块
//!   kind==1:
//!     [1] blockBits
//!     [ceil(4096*blockBits/8)] 位打包的方块索引
//!   若 flags 含 FLAG_BIOMES:
//!     [1] biomeBits
//!     [ceil(64*biomeBits/8)]   位打包的群系索引
//! [4] blockEntityCount + NBT × count
//! [4] entityCount      + NBT × count
//! ```
//!
//! 位打包（`PackedArray`）：**低位在前**，跨字节连续，无对齐要求。

use crate::nbt::{self, Compound, Error, Result, Tag, TagType};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 魔数 "SMCP"
pub const MAGIC: [u8; 4] = *b"SMCP";

/// 当前支持的格式版本（本程序只写到这个版本）。
pub const FORMAT_VERSION: i32 = 2;

pub const FLAG_BIOMES: i32 = 1;
pub const FLAG_ENTITIES: i32 = 1 << 1;
pub const FLAG_LIQUID: i32 = 1 << 2;
pub const FLAG_BLOCK_ENTITY_DATA: i32 = 1 << 3;

/// 选区单边区块数上限。
pub const MAX_CHUNK_SIDE: i32 = 7;

/// 一个 section 的方块数。
pub const SECTION_BLOCKS: usize = 4096;
/// 一个 section 的群系格数。
pub const SECTION_BIOMES: usize = 64;

// ---------------------------------------------------------------------------
// Modified UTF-8（Java writeUTF / readUTF）
// ---------------------------------------------------------------------------

/// 写 Java `writeUTF` 格式的字符串。
///
/// Modified UTF-8 与标准 UTF-8 的差别：
/// - 补充平面字符按 UTF-16 代理对拆成两个 3 字节序列
/// - `U+0000` 编码成两字节 `C0 80`
pub fn write_java_utf<W: Write>(w: &mut W, s: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(s.len() + 2);
    for ch in s.chars() {
        let c = ch as u32;
        if c == 0 {
            buf.push(0xC0);
            buf.push(0x80);
        } else if c < 0x80 {
            buf.push(c as u8);
        } else if c < 0x800 {
            buf.push(0xC0 | (c >> 6) as u8);
            buf.push(0x80 | (c & 0x3F) as u8);
        } else if c < 0x10000 {
            buf.push(0xE0 | (c >> 12) as u8);
            buf.push(0x80 | ((c >> 6) & 0x3F) as u8);
            buf.push(0x80 | (c & 0x3F) as u8);
        } else {
            // 补充平面 → 拆成代理对，各 3 字节
            let v = c - 0x10000;
            let hi = 0xD800 + (v >> 10);
            let lo = 0xDC00 + (v & 0x3FF);
            for u in [hi, lo] {
                buf.push(0xE0 | (u >> 12) as u8);
                buf.push(0x80 | ((u >> 6) & 0x3F) as u8);
                buf.push(0x80 | (u & 0x3F) as u8);
            }
        }
    }
    if buf.len() > u16::MAX as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "字符串超过 writeUTF 的 64KB 上限",
        ));
    }
    w.write_all(&(buf.len() as u16).to_be_bytes())?;
    w.write_all(&buf)?;
    Ok(())
}

/// 读 Java `writeUTF` 格式的字符串。
pub fn read_java_utf<R: Read>(r: &mut R) -> Result<String> {
    let mut lb = [0u8; 2];
    r.read_exact(&mut lb)?;
    let len = u16::from_be_bytes(lb) as usize;
    if len == 0 {
        return Ok(String::new());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    // 先按 Modified UTF-8 解出 UTF-16 码元，再拼成 String
    let mut units: Vec<u16> = Vec::with_capacity(len);
    let mut i = 0;
    while i < buf.len() {
        let b = buf[i];
        if b < 0x80 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            if i + 1 >= buf.len() {
                return Err(Error::Other("Modified UTF-8 截断".into()));
            }
            let c = (((b & 0x1F) as u16) << 6) | (buf[i + 1] & 0x3F) as u16;
            units.push(c);
            i += 2;
        } else if b & 0xF0 == 0xE0 {
            if i + 2 >= buf.len() {
                return Err(Error::Other("Modified UTF-8 截断".into()));
            }
            let c = (((b & 0x0F) as u16) << 12)
                | (((buf[i + 1] & 0x3F) as u16) << 6)
                | (buf[i + 2] & 0x3F) as u16;
            units.push(c);
            i += 3;
        } else {
            return Err(Error::Other(format!("非法的 Modified UTF-8 字节 0x{b:02X}")));
        }
    }
    Ok(String::from_utf16_lossy(&units))
}

// ---------------------------------------------------------------------------
// 位打包（对应 PackedArray）
// ---------------------------------------------------------------------------

/// 表达 `count` 种取值所需的最小位宽（至少 1）。
pub fn bits_for(count: usize) -> u8 {
    if count <= 1 {
        return 1;
    }
    (usize::BITS - (count - 1).leading_zeros()) as u8
}

/// 低位在前的位打包。
pub fn pack(values: &[u32], bits_per_entry: u8) -> Vec<u8> {
    assert!((1..=32).contains(&bits_per_entry));
    let mask = if bits_per_entry == 32 {
        0xFFFF_FFFFu64
    } else {
        (1u64 << bits_per_entry) - 1
    };
    let total_bits = values.len() as u64 * bits_per_entry as u64;
    let mut out = vec![0u8; ((total_bits + 7) / 8) as usize];
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    let mut out_index = 0usize;
    for &v in values {
        buffer |= ((v as u64) & mask) << bits;
        bits += bits_per_entry as u32;
        while bits >= 8 {
            out[out_index] = (buffer & 0xFF) as u8;
            out_index += 1;
            buffer >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 && out_index < out.len() {
        out[out_index] = (buffer & 0xFF) as u8;
    }
    out
}

/// 解包；数据不足时补 0（与 Java 实现一致，宁可读出空方块也不失败）。
pub fn unpack(src: &[u8], count: usize, bits_per_entry: u8) -> Vec<u32> {
    assert!((1..=32).contains(&bits_per_entry));
    let mask = if bits_per_entry == 32 {
        0xFFFF_FFFFu64
    } else {
        (1u64 << bits_per_entry) - 1
    };
    let mut out = vec![0u32; count];
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    let mut in_index = 0usize;
    for item in out.iter_mut() {
        while bits < bits_per_entry as u32 {
            let byte_value = if in_index < src.len() {
                src[in_index] as u64
            } else {
                0
            };
            buffer |= byte_value << bits;
            bits += 8;
            in_index += 1;
        }
        *item = (buffer & mask) as u32;
        buffer >>= bits_per_entry as u32;
        bits -= bits_per_entry as u32;
    }
    out
}

// ---------------------------------------------------------------------------
// 文件头
// ---------------------------------------------------------------------------

/// `.smclip` 文件头。
#[derive(Clone, Debug, Default)]
pub struct ClipHeader {
    pub format_version: i32,
    pub data_version: i32,
    pub mod_version: String,
    pub created_millis: i64,
    pub note: String,
    pub source_world: String,
    pub dimension: String,
    pub origin_chunk_x: i32,
    pub origin_chunk_z: i32,
    pub size_x: i32,
    pub size_z: i32,
    pub min_y: i32,
    pub max_y: i32,
    pub flags: i32,
    pub block_palette: Vec<String>,
    pub biome_palette: Vec<String>,
}

impl ClipHeader {
    pub fn has_biomes(&self) -> bool {
        self.flags & FLAG_BIOMES != 0
    }
    pub fn has_entities(&self) -> bool {
        self.flags & FLAG_ENTITIES != 0
    }
    pub fn has_block_entity_data(&self) -> bool {
        self.flags & FLAG_BLOCK_ENTITY_DATA != 0
    }
    pub fn chunk_count(&self) -> usize {
        (self.size_x.max(0) * self.size_z.max(0)) as usize
    }
    pub fn height_blocks(&self) -> i32 {
        self.max_y - self.min_y + 1
    }

    /// 读出头部（消耗 reader 到 body 起点）。
    pub fn read<R: Read>(r: &mut R) -> Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(Error::Other(format!(
                "不是 .smclip 文件（魔数 {:?}，期望 SMCP）",
                String::from_utf8_lossy(&magic)
            )));
        }

        let mut b2 = [0u8; 2];
        let mut b4 = [0u8; 4];
        let mut b8 = [0u8; 8];

        r.read_exact(&mut b2)?;
        let format_version = u16::from_be_bytes(b2) as i32;
        if format_version > FORMAT_VERSION {
            return Err(Error::Other(format!(
                "文件格式版本 {format_version} 高于本程序支持的 {FORMAT_VERSION}"
            )));
        }

        r.read_exact(&mut b4)?;
        let data_version = i32::from_be_bytes(b4);
        let mod_version = read_java_utf(r)?;
        r.read_exact(&mut b8)?;
        let created_millis = i64::from_be_bytes(b8);
        let note = read_java_utf(r)?;
        let source_world = read_java_utf(r)?;
        let dimension = read_java_utf(r)?;

        r.read_exact(&mut b4)?;
        let origin_chunk_x = i32::from_be_bytes(b4);
        r.read_exact(&mut b4)?;
        let origin_chunk_z = i32::from_be_bytes(b4);

        r.read_exact(&mut b2)?;
        let size_x = i16::from_be_bytes(b2) as i32;
        r.read_exact(&mut b2)?;
        let size_z = i16::from_be_bytes(b2) as i32;
        r.read_exact(&mut b2)?;
        let min_y = i16::from_be_bytes(b2) as i32;
        r.read_exact(&mut b2)?;
        let max_y = i16::from_be_bytes(b2) as i32;

        let mut b1 = [0u8; 1];
        r.read_exact(&mut b1)?;
        let flags = b1[0] as i8 as i32;

        let mut read_palette = |r: &mut R| -> Result<Vec<String>> {
            r.read_exact(&mut b4)?;
            let n = i32::from_be_bytes(b4);
            if !(0..=65536).contains(&n) {
                return Err(Error::Other(format!("调色板条目数异常: {n}")));
            }
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_java_utf(r)?);
            }
            Ok(v)
        };

        let block_palette = read_palette(r)?;
        let biome_palette = read_palette(r)?;

        Ok(ClipHeader {
            format_version,
            data_version,
            mod_version,
            created_millis,
            note,
            source_world,
            dimension,
            origin_chunk_x,
            origin_chunk_z,
            size_x,
            size_z,
            min_y,
            max_y,
            flags,
            block_palette,
            biome_palette,
        })
    }

    /// 写出头部。
    pub fn write<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&MAGIC)?;
        w.write_all(&(self.format_version as u16).to_be_bytes())?;
        w.write_all(&self.data_version.to_be_bytes())?;
        write_java_utf(w, &self.mod_version)?;
        w.write_all(&self.created_millis.to_be_bytes())?;
        write_java_utf(w, &self.note)?;
        write_java_utf(w, &self.source_world)?;
        write_java_utf(w, &self.dimension)?;
        w.write_all(&self.origin_chunk_x.to_be_bytes())?;
        w.write_all(&self.origin_chunk_z.to_be_bytes())?;
        w.write_all(&(self.size_x as i16).to_be_bytes())?;
        w.write_all(&(self.size_z as i16).to_be_bytes())?;
        w.write_all(&(self.min_y as i16).to_be_bytes())?;
        w.write_all(&(self.max_y as i16).to_be_bytes())?;
        w.write_all(&[self.flags as i8 as u8])?;
        w.write_all(&(self.block_palette.len() as i32).to_be_bytes())?;
        for e in &self.block_palette {
            write_java_utf(w, e)?;
        }
        w.write_all(&(self.biome_palette.len() as i32).to_be_bytes())?;
        for e in &self.biome_palette {
            write_java_utf(w, e)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 单个区块载荷
// ---------------------------------------------------------------------------

/// 一个 section 的方块数据。
#[derive(Clone, Debug)]
pub enum SectionBlocks {
    /// 整段空气。
    Empty,
    /// 4096 个调色板索引（Y→Z→X 顺序）。
    Indexed(Vec<u32>),
}

/// 解码后的一个区块。
#[derive(Clone, Debug, Default)]
pub struct ClipChunk {
    /// 第一段的基准 Y（16 对齐）。
    pub section_base_y: i32,
    /// 每段的方块数据（自下而上）。
    pub sections: Vec<SectionBlocks>,
    /// 每段的群系索引（64 个 4×4×4 格）；`None` 表示文件里没有群系数据。
    pub biomes: Vec<Option<Vec<u32>>>,
    pub block_entities: Vec<Compound>,
    pub entities: Vec<Compound>,
}

impl ClipChunk {
    /// 该段起始 Y。
    pub fn section_y(&self, index: usize) -> i32 {
        self.section_base_y + (index as i32) * 16
    }
}

/// 从 reader 读一个区块载荷（未压缩）。
pub fn read_chunk_payload<R: Read>(r: &mut R, header: &ClipHeader) -> Result<ClipChunk> {
    let mut b1 = [0u8; 1];
    let mut b2 = [0u8; 2];
    let mut b4 = [0u8; 4];

    r.read_exact(&mut b2)?;
    let section_count = i16::from_be_bytes(b2) as i32;
    if !(0..=4096).contains(&section_count) {
        return Err(Error::Other(format!("section 数异常: {section_count}")));
    }
    r.read_exact(&mut b2)?;
    let section_base_y = i16::from_be_bytes(b2) as i32;

    let mut sections = Vec::with_capacity(section_count as usize);
    let mut biomes = Vec::with_capacity(section_count as usize);

    for _ in 0..section_count {
        r.read_exact(&mut b1)?;
        match b1[0] {
            0 => sections.push(SectionBlocks::Empty),
            1 => {
                r.read_exact(&mut b1)?;
                let bits = b1[0];
                if !(1..=32).contains(&bits) {
                    return Err(Error::Other(format!("blockBits 越界: {bits}")));
                }
                let nbytes = (SECTION_BLOCKS * bits as usize + 7) / 8;
                let mut raw = vec![0u8; nbytes];
                r.read_exact(&mut raw)?;
                sections.push(SectionBlocks::Indexed(unpack(&raw, SECTION_BLOCKS, bits)));
            }
            k => return Err(Error::Other(format!("未知的 section 类型: {k}"))),
        }

        if header.has_biomes() {
            r.read_exact(&mut b1)?;
            let bits = b1[0];
            if !(1..=32).contains(&bits) {
                return Err(Error::Other(format!("biomeBits 越界: {bits}")));
            }
            let nbytes = (SECTION_BIOMES * bits as usize + 7) / 8;
            let mut raw = vec![0u8; nbytes];
            r.read_exact(&mut raw)?;
            biomes.push(Some(unpack(&raw, SECTION_BIOMES, bits)));
        } else {
            biomes.push(None);
        }
    }

    // 方块实体
    r.read_exact(&mut b4)?;
    let be_count = i32::from_be_bytes(b4);
    if !(0..=1_000_000).contains(&be_count) {
        return Err(Error::Other(format!("方块实体数异常: {be_count}")));
    }
    let mut block_entities = Vec::with_capacity(be_count as usize);
    for _ in 0..be_count {
        block_entities.push(read_raw_nbt(r)?);
    }

    // 实体
    r.read_exact(&mut b4)?;
    let ent_count = i32::from_be_bytes(b4);
    if !(0..=1_000_000).contains(&ent_count) {
        return Err(Error::Other(format!("实体数异常: {ent_count}")));
    }
    let mut entities = Vec::with_capacity(ent_count as usize);
    for _ in 0..ent_count {
        entities.push(read_raw_nbt(r)?);
    }

    Ok(ClipChunk {
        section_base_y,
        sections,
        biomes,
        block_entities,
        entities,
    })
}

/// 读一个不带长度前缀的裸 NBT 复合物（`NbtIo.write` 的格式）。
///
/// 需要先瞥一下类型字节，所以内部自己缓冲。
fn read_raw_nbt<R: Read>(r: &mut R) -> Result<Compound> {
    // 用一层包装把"已读字节"记下来，读到完整复合物为止。
    // 简单做法：读类型字节后，把剩余内容整体读进内存再解析。
    let mut buf = Vec::new();
    let mut one = [0u8; 1];
    r.read_exact(&mut one)?;

    // 顶层必须是 TAG_Compound(10)
    if one[0] != 10 {
        // 有些实现会跳过根复合物名，这里也容忍裸着的 compound
        return Err(Error::Other(format!(
            "期望实体/方块实体是 Compound，读到 tag 类型 {}",
            one[0]
        )));
    }
    // 读根名（通常空）
    let mut lb = [0u8; 2];
    r.read_exact(&mut lb)?;
    let name_len = u16::from_be_bytes(lb) as usize;
    let mut name = vec![0u8; name_len];
    r.read_exact(&mut name)?;

    // 现在读 body，直到对应的 TAG_End。
    // 用栈计数“嵌套 compound/list”不好做，这里改用：边读边拼字节，再交给 nbt::read。
    // 为了让 nbt::read 能解析，需要把 "10 + 名字 + body" 补全。
    let body = read_compound_body(r)?;

    buf.push(10u8);
    buf.extend_from_slice(&(name_len as u16).to_be_bytes());
    buf.extend_from_slice(&name);
    buf.extend_from_slice(&body);

    let tag = nbt::read(&buf)?;
    match tag {
        Tag::Compound(c) => Ok(c),
        _ => Err(Error::Other("不是 Compound".into())),
    }
}

/// 读一个 compound 的 body（到 TAG_End 为止，不含 TAG_End）。
fn read_compound_body<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let mut tb = [0u8; 1];
        r.read_exact(&mut tb)?;
        let ty = tb[0];
        out.push(ty);
        if ty == 0 {
            return Ok(out);
        }
        // 名字
        let name = read_modified_utf_bytes(r)?;
        out.extend_from_slice(&name);
        // 负载
        let payload = read_payload_bytes(r, ty)?;
        out.extend_from_slice(&payload);
    }
}

/// 读 Modified UTF-8 的原始字节（长度 + 内容），原样返回。
fn read_modified_utf_bytes<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut lb = [0u8; 2];
    r.read_exact(&mut lb)?;
    let n = u16::from_be_bytes(lb) as usize;
    let mut v = Vec::with_capacity(2 + n);
    v.extend_from_slice(&lb);
    let mut s = vec![0u8; n];
    r.read_exact(&mut s)?;
    v.extend_from_slice(&s);
    Ok(v)
}

/// 读一个指定类型的负载，返回原始字节（含其内部结构）。
fn read_payload_bytes<R: Read>(r: &mut R, ty: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match ty {
        1 | 2 | 3 | 4 | 5 | 6 => {
            let n = match ty {
                1 => 1,
                2 => 2,
                3 => 4,
                4 => 8,
                5 => 4,
                _ => 8,
            };
            let mut b = vec![0u8; n];
            r.read_exact(&mut b)?;
            out.extend_from_slice(&b);
        }
        7 => {
            // ByteArray
            let mut lb = [0u8; 4];
            r.read_exact(&mut lb)?;
            let n = i32::from_be_bytes(lb);
            if n < 0 || n > 64 * 1024 * 1024 {
                return Err(Error::Other(format!("ByteArray 长度异常: {n}")));
            }
            out.extend_from_slice(&lb);
            let mut b = vec![0u8; n as usize];
            r.read_exact(&mut b)?;
            out.extend_from_slice(&b);
        }
        8 => out.extend_from_slice(&read_modified_utf_bytes(r)?),
        9 => {
            // List
            let mut tb = [0u8; 1];
            r.read_exact(&mut tb)?;
            out.push(tb[0]);
            let mut lb = [0u8; 4];
            r.read_exact(&mut lb)?;
            let n = i32::from_be_bytes(lb);
            if n < 0 || n > 16_000_000 {
                return Err(Error::Other(format!("List 长度异常: {n}")));
            }
            out.extend_from_slice(&lb);
            // 空 list 的元素类型可能是 0，此时没有负载
            if n > 0 {
                let elem = tb[0];
                for _ in 0..n {
                    let p = read_payload_bytes(r, elem)?;
                    out.extend_from_slice(&p);
                }
            }
        }
        10 => {
            let p = read_compound_body(r)?;
            out.extend_from_slice(&p);
        }
        11 => {
            // IntArray
            let mut lb = [0u8; 4];
            r.read_exact(&mut lb)?;
            let n = i32::from_be_bytes(lb);
            if n < 0 || n > 16_000_000 {
                return Err(Error::Other(format!("IntArray 长度异常: {n}")));
            }
            out.extend_from_slice(&lb);
            let mut b = vec![0u8; n as usize * 4];
            r.read_exact(&mut b)?;
            out.extend_from_slice(&b);
        }
        12 => {
            // LongArray
            let mut lb = [0u8; 4];
            r.read_exact(&mut lb)?;
            let n = i32::from_be_bytes(lb);
            if n < 0 || n > 16_000_000 {
                return Err(Error::Other(format!("LongArray 长度异常: {n}")));
            }
            out.extend_from_slice(&lb);
            let mut b = vec![0u8; n as usize * 8];
            r.read_exact(&mut b)?;
            out.extend_from_slice(&b);
        }
        other => {
            return Err(Error::Other(format!("未知的 NBT 类型: {other}")));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 整个文件
// ---------------------------------------------------------------------------

/// 一个完整的 `.smclip` 文件。
#[derive(Clone, Debug)]
pub struct ClipFile {
    pub header: ClipHeader,
    /// 按行优先（Z 外 X 内）排列的区块。
    pub chunks: Vec<ClipChunk>,
    /// 每块的压缩元信息（可选，写出时重建）。
    pub raw_lengths: Vec<i32>,
}

impl ClipFile {
    /// 从文件读。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let f = std::fs::File::open(path.as_ref())
            .map_err(|e| Error::Other(format!("打开 {} 失败: {e}", path.as_ref().display())))?;
        let mut r = std::io::BufReader::new(f);
        let header = ClipHeader::read(&mut r)?;
        let count = header.chunk_count();
        if count == 0 {
            return Err(Error::Other("文件里区块数为 0".into()));
        }
        let mut chunks = Vec::with_capacity(count);
        let mut raw_lengths = Vec::with_capacity(count);
        for _ in 0..count {
            let mut b4 = [0u8; 4];
            r.read_exact(&mut b4)?;
            let raw_len = i32::from_be_bytes(b4);
            r.read_exact(&mut b4)?;
            let comp_len = i32::from_be_bytes(b4);
            if comp_len < 0 || comp_len > 512 * 1024 * 1024 {
                return Err(Error::Other(format!("压缩长度异常: {comp_len}")));
            }
            let mut compressed = vec![0u8; comp_len as usize];
            r.read_exact(&mut compressed)?;
            // 解压（Deflate，zlib 包装）
            let mut raw = Vec::with_capacity(raw_len.max(0) as usize);
            flate2::read::ZlibDecoder::new(&compressed[..])
                .read_to_end(&mut raw)
                .map_err(|e| Error::Other(format!("解压失败: {e}")))?;
            let mut cr = std::io::Cursor::new(&raw);
            chunks.push(read_chunk_payload(&mut cr, &header)?);
            raw_lengths.push(raw_len);
        }
        Ok(ClipFile {
            header,
            chunks,
            raw_lengths,
        })
    }

    /// 写到文件（先写临时文件再原子替换）。
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)
                .map_err(|e| Error::Other(format!("建目录失败: {e}")))?;
        }
        let tmp = path.with_extension("smclip.tmp");

        let mut payloads: Vec<(i32, Vec<u8>)> = Vec::with_capacity(self.chunks.len());
        for c in &self.chunks {
            let raw = encode_chunk_payload(c, &self.header)?;
            payloads.push((raw.len() as i32, raw));
        }

        {
            let f = std::fs::File::create(&tmp)
                .map_err(|e| Error::Other(format!("创建临时文件失败: {e}")))?;
            let mut w = std::io::BufWriter::new(f);
            self.header.write(&mut w).map_err(|e| Error::Other(e.to_string()))?;
            for (raw_len, raw) in &payloads {
                let mut enc = flate2::write::ZlibEncoder::new(
                    Vec::new(),
                    flate2::Compression::new(6),
                );
                enc.write_all(raw).map_err(|e| Error::Other(e.to_string()))?;
                let compressed = enc.finish().map_err(|e| Error::Other(e.to_string()))?;
                w.write_all(&raw_len.to_be_bytes())
                    .map_err(|e| Error::Other(e.to_string()))?;
                w.write_all(&(compressed.len() as i32).to_be_bytes())
                    .map_err(|e| Error::Other(e.to_string()))?;
                w.write_all(&compressed)
                    .map_err(|e| Error::Other(e.to_string()))?;
            }
            w.flush().map_err(|e| Error::Other(e.to_string()))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| Error::Other(format!("替换文件失败: {e}")))?;
        Ok(())
    }
}

/// 把一个区块编码成载荷字节（未压缩）。
pub fn encode_chunk_payload(c: &ClipChunk, header: &ClipHeader) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(16 * 1024);
    out.extend_from_slice(&(c.sections.len() as i16).to_be_bytes());
    out.extend_from_slice(&(c.section_base_y as i16).to_be_bytes());

    for (i, s) in c.sections.iter().enumerate() {
        match s {
            SectionBlocks::Empty => out.push(0),
            SectionBlocks::Indexed(v) => {
                if v.len() != SECTION_BLOCKS {
                    return Err(Error::Other(format!(
                        "section {i} 的方块数应为 {SECTION_BLOCKS}，实际 {}",
                        v.len()
                    )));
                }
                out.push(1);
                let bits = bits_for(header.block_palette.len());
                out.push(bits);
                out.extend_from_slice(&pack(v, bits));
            }
        }
        if header.has_biomes() {
            let idx = c
                .biomes
                .get(i)
                .and_then(|b| b.as_ref())
                .cloned()
                .unwrap_or_else(|| vec![0; SECTION_BIOMES]);
            let bits = bits_for(header.biome_palette.len().max(1));
            out.push(bits);
            out.extend_from_slice(&pack(&idx, bits));
        }
    }

    out.extend_from_slice(&(c.block_entities.len() as i32).to_be_bytes());
    for be in &c.block_entities {
        let bytes = nbt::write(&Tag::Compound(be.clone()))?;
        out.extend_from_slice(&bytes);
    }

    out.extend_from_slice(&(c.entities.len() as i32).to_be_bytes());
    for e in &c.entities {
        let bytes = nbt::write(&Tag::Compound(e.clone()))?;
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 从 NBT 区块转换
// ---------------------------------------------------------------------------

/// 把存档里的区块 NBT 转成 `.smclip` 的调色板索引形式。
///
/// `block_palette` / `biome_palette` 是**整个文件共享**的调色板，
/// 这里会往里追加新条目并返回索引。
pub fn chunk_from_nbt(
    tag: &Tag,
    block_palette: &mut Vec<String>,
    block_index: &mut HashMap<String, u32>,
    biome_palette: &mut Vec<String>,
    biome_index: &mut HashMap<String, u32>,
    y_range: Option<(i32, i32)>,
) -> Result<ClipChunk> {
    let comp = tag
        .as_compound()
        .ok_or_else(|| Error::Other("区块不是 Compound".into()))?;

    let (min_y, max_y) = match y_range {
        Some(r) => r,
        None => (-64, 319), // 1.18+ 默认高度
    };

    let sections = comp
        .get("sections")
        .and_then(|t| t.as_list())
        .map(|(_, items)| items.clone())
        .unwrap_or_default();

    // 收集所有 section 的 Y
    let mut ys: Vec<i32> = sections
        .iter()
        .filter_map(|s| s.as_compound()?.get("Y")?.as_i8().map(|v| v as i32))
        .collect();
    ys.sort_unstable();
    if ys.is_empty() {
        return Ok(ClipChunk::default());
    }

    let first_y = *ys.first().unwrap();
    let last_y = *ys.last().unwrap();
    let section_base_y = first_y * 16;
    let count = (last_y - first_y + 1) as usize;

    let mut out_sections = Vec::with_capacity(count);
    let mut out_biomes = Vec::with_capacity(count);

    for i in 0..count {
        let sy = first_y + i as i32;
        let sec = sections.iter().find_map(|s| {
            let c = s.as_compound()?;
            if c.get("Y")?.as_i8().map(|v| v as i32) == Some(sy) {
                Some(c.clone())
            } else {
                None
            }
        });

        let Some(sec) = sec else {
            out_sections.push(SectionBlocks::Empty);
            out_biomes.push(Some(vec![0; SECTION_BIOMES]));
            continue;
        };

        // 方块
        let (pal, data) = match sec.get("block_states").and_then(|t| t.as_compound()) {
            Some(bs) => {
                let pal: Vec<String> = bs
                    .get("palette")
                    .and_then(|t| t.as_list())
                    .map(|(_, items)| {
                        items
                            .iter()
                            .map(|t| block_state_name(t).unwrap_or_else(|| "minecraft:air".into()))
                            .collect()
                    })
                    .unwrap_or_default();
                let data = bs
                    .get("data")
                    .and_then(|t| t.as_long_array())
                    .map(|v| v.to_vec())
                    .unwrap_or_default();
                (pal, data)
            }
            None => (vec!["minecraft:air".to_string()], Vec::new()),
        };

        // 建立本地调色板 → 全局调色板索引
        let mut local_to_global: Vec<u32> = Vec::with_capacity(pal.len());
        for name in &pal {
            let g = match block_index.get(name) {
                Some(&i) => i,
                None => {
                    let i = block_palette.len() as u32;
                    block_palette.push(name.clone());
                    block_index.insert(name.clone(), i);
                    i
                }
            };
            local_to_global.push(g);
        }

        // 解包 4096 个方块（1.16+ 用紧凑位打包：不跨 long 边界）
        let ids = unpack_block_states(&data, &local_to_global, 4096);

        // 只保留 [min_y, max_y] 范围内的方块
        let sec_base = sy * 16;
        let mut indices = vec![0u32; SECTION_BLOCKS];
        let mut any = false;
        let air_global = {
            let key = "minecraft:air".to_string();
            match block_index.get(&key) {
                Some(&i) => i,
                None => {
                    let i = block_palette.len() as u32;
                    block_palette.push(key.clone());
                    block_index.insert(key, i);
                    i
                }
            }
        };
        for y in 0..16 {
            let wy = sec_base + y;
            let in_range = wy >= min_y && wy <= max_y;
            for z in 0..16 {
                for x in 0..16 {
                    let src = ((y * 16 + z) * 16 + x) as usize;
                    let dst = src;
                    let v = if in_range {
                        ids.get(src).copied().unwrap_or(air_global)
                    } else {
                        air_global
                    };
                    indices[dst] = v;
                    if v != air_global {
                        any = true;
                    }
                }
            }
        }
        out_sections.push(if any {
            SectionBlocks::Indexed(indices)
        } else {
            SectionBlocks::Empty
        });

        // 群系
        let bsec = sec.get("biomes").and_then(|t| t.as_compound());
        match bsec {
            Some(bc) => {
                let bpal: Vec<String> = bc
                    .get("palette")
                    .and_then(|t| t.as_list())
                    .map(|(_, items)| {
                        items
                            .iter()
                            .map(|t| t.as_str().unwrap_or("minecraft:plains").to_string())
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["minecraft:plains".into()]);
                let mut l2g = Vec::with_capacity(bpal.len());
                for name in &bpal {
                    let g = match biome_index.get(name) {
                        Some(&i) => i,
                        None => {
                            let i = biome_palette.len() as u32;
                            biome_palette.push(name.clone());
                            biome_index.insert(name.clone(), i);
                            i
                        }
                    };
                    l2g.push(g);
                }
                let bdata = bc.get("data").and_then(|t| t.as_long_array()).unwrap_or(&[]);
                let ids = unpack_block_states(bdata, &l2g, SECTION_BIOMES);
                out_biomes.push(Some(ids));
            }
            None => out_biomes.push(Some(vec![0; SECTION_BIOMES])),
        }
    }

    // 方块实体
    let block_entities: Vec<Compound> = comp
        .get("block_entities")
        .and_then(|t| t.as_list())
        .map(|(_, items)| items.iter().filter_map(|t| t.as_compound().cloned()).collect())
        .unwrap_or_default();

    // 实体（1.21.1 用 "Entities"，26.x 用 "entities"）
    let entities: Vec<Compound> = comp
        .get("Entities")
        .or_else(|| comp.get("entities"))
        .and_then(|t| t.as_list())
        .map(|(_, items)| items.iter().filter_map(|t| t.as_compound().cloned()).collect())
        .unwrap_or_default();

    Ok(ClipChunk {
        section_base_y,
        sections: out_sections,
        biomes: out_biomes,
        block_entities,
        entities,
    })
}

/// 取调色板条目的方块名（兼容 26.x 的空键形式）。
fn block_state_name(tag: &Tag) -> Option<String> {
    let c = tag.as_compound()?;
    for k in ["Name", "id", "name", ""] {
        if let Some(s) = c.get(k).and_then(|t| t.as_str()) {
            return Some(s.to_string());
        }
    }
    // 兜底：第一个字符串值
    for (_, v) in c.iter() {
        if let Some(s) = v.as_str() {
            return Some(s.to_string());
        }
    }
    None
}

/// 解包 Minecraft 的紧凑位打包方块数据。
///
/// 1.16+ 的规则：**不跨 long 边界**，每个 long 里能放
/// `64 / bits` 个值（bits 是 2 的幂时直接用 bits，否则向下取整）。
fn unpack_block_states(data: &[i64], palette_map: &[u32], count: usize) -> Vec<u32> {
    let bits = {
        let n = palette_map.len().max(1);
        let b = (usize::BITS - (n - 1).leading_zeros()) as usize;
        b.max(4) // 1.16+ 最少 4 位
    };
    let per_long = 64 / bits;
    let mut out = vec![palette_map.first().copied().unwrap_or(0); count];

    if palette_map.len() <= 1 {
        return out;
    }

    for i in 0..count {
        let long_idx = i / per_long;
        if long_idx >= data.len() {
            break;
        }
        let bit_off = (i % per_long) * bits;
        let mask = (1u64 << bits) - 1;
        let v = ((data[long_idx] as u64) >> bit_off) & mask;
        let pi = v as usize;
        if pi < palette_map.len() {
            out[i] = palette_map[pi];
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 还原成存档区块 NBT
// ---------------------------------------------------------------------------

/// 把 `.smclip` 的区块还原成存档里的区块 NBT。
///
/// 生成的 NBT 与存档区块结构一致（`sections` + 1.16+ 紧凑位打包），
/// 这样可以直接交给现有的粘贴逻辑（`edit::paste_chunks`）使用。
///
/// `cx` / `cz` 是目标区块坐标，会写进 `xPos` / `zPos`。
pub fn chunk_to_nbt(
    chunk: &ClipChunk,
    header: &ClipHeader,
    cx: i32,
    cz: i32,
) -> Result<Tag> {
    let mut sections: Vec<Tag> = Vec::new();

    // 按 .smclip 的全局调色板反查方块名
    let name_of = |idx: u32| -> String {
        header
            .block_palette
            .get(idx as usize)
            .cloned()
            .unwrap_or_else(|| "minecraft:air".into())
    };
    let biome_of = |idx: u32| -> String {
        header
            .biome_palette
            .get(idx as usize)
            .cloned()
            .unwrap_or_else(|| "minecraft:plains".into())
    };

    for (si, sec) in chunk.sections.iter().enumerate() {
        let sy = (chunk.section_base_y / 16) + si as i32;

        // ---- 建本地的方块调色板 ----
        let mut local_pal: Vec<String> = Vec::new();
        let mut local_idx: HashMap<String, usize> = HashMap::new();
        let mut ids: Vec<u32> = vec![0; SECTION_BLOCKS];

        match sec {
            SectionBlocks::Empty => {
                // 空段也显式写出来（空气调色板）
                local_pal.push("minecraft:air".into());
                local_idx.insert("minecraft:air".into(), 0);
            }
            SectionBlocks::Indexed(v) => {
                if v.len() != SECTION_BLOCKS {
                    return Err(Error::Other(format!(
                        "section {si} 方块数应为 {SECTION_BLOCKS}，实际 {}",
                        v.len()
                    )));
                }
                for (i, &g) in v.iter().enumerate() {
                    let name = name_of(g);
                    let li = match local_idx.get(&name) {
                        Some(&x) => x,
                        None => {
                            let x = local_pal.len();
                            local_pal.push(name.clone());
                            local_idx.insert(name, x);
                            x
                        }
                    };
                    ids[i] = li as u32;
                }
            }
        }

        // ---- 组装 block_states ----
        let mut bs = Compound::new();
        let pal_items: Vec<Tag> = local_pal
            .iter()
            .map(|name| {
                let mut c = Compound::new();
                c.insert("Name".into(), Tag::String(name.clone()));
                Tag::Compound(c)
            })
            .collect();
        bs.insert("palette".into(), Tag::List { elem: TagType::Compound, items: pal_items });

        // 只有 1 种方块时不写 data
        if local_pal.len() > 1 {
            let bits = {
                let n = local_pal.len();
                let b = (usize::BITS - (n - 1).leading_zeros()) as usize;
                b.max(4)
            };
            let data = pack_block_states(&ids, bits);
            bs.insert("data".into(), Tag::LongArray(data));
        }

        // ---- 群系 ----
        let mut bsec = Compound::new();
        let bidx: Vec<u32> = chunk
            .biomes
            .get(si)
            .and_then(|b| b.clone())
            .unwrap_or_else(|| vec![0; SECTION_BIOMES]);
        let mut bp: Vec<String> = Vec::new();
        let mut bi: HashMap<String, usize> = HashMap::new();
        for &g in &bidx {
            let name = biome_of(g);
            if !bi.contains_key(&name) {
                bi.insert(name.clone(), bp.len());
                bp.push(name);
            }
        }
        let bpal_items: Vec<Tag> = bp.iter().map(|n| Tag::String(n.clone())).collect();
        bsec.insert("palette".into(), Tag::List { elem: TagType::String, items: bpal_items });
        if bp.len() > 1 {
            let local: Vec<u32> = bidx
                .iter()
                .map(|&g| bi.get(&biome_of(g)).copied().unwrap_or(0) as u32)
                .collect();
            let bits = {
                let n = bp.len();
                let b = (usize::BITS - (n - 1).leading_zeros()) as usize;
                b.max(1)
            };
            bsec.insert("data".into(), Tag::LongArray(pack_block_states(&local, bits)));
        }

        let mut sc = Compound::new();
        sc.insert("Y".into(), Tag::Byte(sy as i8));
        sc.insert("block_states".into(), Tag::Compound(bs));
        sc.insert("biomes".into(), Tag::Compound(bsec));
        sections.push(Tag::Compound(sc));
    }

    let mut root = Compound::new();
    root.insert("sections".into(), Tag::List { elem: TagType::Compound, items: sections });
    root.insert("xPos".into(), Tag::Int(cx));
    root.insert("zPos".into(), Tag::Int(cz));
    root.insert("yPos".into(), Tag::Int(-4));
    root.insert("Status".into(), Tag::String("minecraft:full".into()));
    root.insert("DataVersion".into(), Tag::Int(header.data_version));
    root.insert("isLightOn".into(), Tag::Byte(1));
    root.insert("LastUpdate".into(), Tag::Long(0));
    root.insert("InhabitedTime".into(), Tag::Long(0));
    root.insert("Heightmaps".into(), Tag::Compound(Compound::new()));
    root.insert("block_ticks".into(), Tag::List { elem: TagType::Compound, items: vec![] });
    root.insert("fluid_ticks".into(), Tag::List { elem: TagType::Compound, items: vec![] });
    root.insert("PostProcessing".into(), Tag::List { elem: TagType::Compound, items: vec![] });
    root.insert("structures".into(), Tag::Compound(Compound::new()));

    // 方块实体
    let be_items: Vec<Tag> = chunk
        .block_entities
        .iter()
        .cloned()
        .map(Tag::Compound)
        .collect();
    root.insert("block_entities".into(), Tag::List { elem: TagType::Compound, items: be_items });

    // 实体
    let ent_items: Vec<Tag> = chunk.entities.iter().cloned().map(Tag::Compound).collect();
    root.insert("entities".into(), Tag::List { elem: TagType::Compound, items: ent_items });

    Ok(Tag::Compound(root))
}

/// 按 1.16+ 的紧凑规则打包（不跨 long 边界）。
fn pack_block_states(values: &[u32], bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let nlongs = (values.len() + per_long - 1) / per_long;
    let mut out = vec![0i64; nlongs];
    for (i, &v) in values.iter().enumerate() {
        let li = i / per_long;
        let off = (i % per_long) * bits;
        out[li] |= (((v as u64) & mask) << off) as i64;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_utf_roundtrip() {
        for s in [
            "",
            "minecraft:stone",
            "中文测试",
            "🎮 emoji",
            "a\u{0}b",
        ] {
            let mut buf = Vec::new();
            write_java_utf(&mut buf, s).unwrap();
            let mut c = std::io::Cursor::new(&buf);
            assert_eq!(read_java_utf(&mut c).unwrap(), s, "往返失败: {s:?}");
        }
    }

    #[test]
    fn packed_array_bits() {
        assert_eq!(bits_for(0), 1);
        assert_eq!(bits_for(1), 1);
        assert_eq!(bits_for(2), 1);
        assert_eq!(bits_for(3), 2);
        assert_eq!(bits_for(4), 2);
        assert_eq!(bits_for(5), 3);
        assert_eq!(bits_for(256), 8);
        assert_eq!(bits_for(257), 9);
    }

    #[test]
    fn packed_array_roundtrip() {
        for bits in [1u8, 2, 3, 5, 8, 12, 16] {
            let max = (1u32 << bits) - 1;
            let vals: Vec<u32> = (0..1000).map(|i| (i as u32) & max).collect();
            let packed = pack(&vals, bits);
            let back = unpack(&packed, vals.len(), bits);
            assert_eq!(vals, back, "bits={bits} 往返失败");
        }
    }

    #[test]
    fn packed_array_matches_java_layout() {
        // 手算验证：3 位，值 [1,2,3,4]
        // buffer = 1 | 2<<3 | 3<<6 | 4<<9 = 1 + 16 + 192 + 2048 = 2257 = 0x8D1
        // 低字节 0xD1，高字节 0x08
        let vals = [1u32, 2, 3, 4];
        let packed = pack(&vals, 3);
        assert_eq!(packed, vec![0xD1, 0x08]);
        assert_eq!(unpack(&packed, 4, 3), vals.to_vec());
    }

    #[test]
    fn header_roundtrip() {
        let h = ClipHeader {
            format_version: 2,
            data_version: 5126,
            mod_version: "1.0.6".into(),
            created_millis: 1_700_000_000_000,
            note: "测试选区".into(),
            source_world: "新的世界".into(),
            dimension: "minecraft:overworld".into(),
            origin_chunk_x: -5,
            origin_chunk_z: 12,
            size_x: 3,
            size_z: 2,
            min_y: -64,
            max_y: 319,
            flags: FLAG_BIOMES | FLAG_ENTITIES,
            block_palette: vec!["minecraft:stone".into(), "minecraft:air".into()],
            biome_palette: vec!["minecraft:plains".into()],
        };
        let mut buf = Vec::new();
        h.write(&mut buf).unwrap();
        let mut c = std::io::Cursor::new(&buf);
        let back = ClipHeader::read(&mut c).unwrap();

        assert_eq!(back.format_version, 2);
        assert_eq!(back.data_version, 5126);
        assert_eq!(back.note, "测试选区");
        assert_eq!(back.origin_chunk_x, -5);
        assert_eq!(back.origin_chunk_z, 12);
        assert_eq!(back.size_x, 3);
        assert_eq!(back.size_z, 2);
        assert_eq!(back.chunk_count(), 6);
        assert_eq!(back.height_blocks(), 384);
        assert_eq!(back.block_palette.len(), 2);
        assert!(back.has_biomes());
        assert!(back.has_entities());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XXXX");
        buf.extend_from_slice(&[0u8; 60]);
        let mut c = std::io::Cursor::new(&buf);
        assert!(ClipHeader::read(&mut c).is_err());
    }

    #[test]
    fn rejects_newer_version() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&99u16.to_be_bytes());
        buf.extend_from_slice(&[0u8; 60]);
        let mut c = std::io::Cursor::new(&buf);
        let err = ClipHeader::read(&mut c).unwrap_err().to_string();
        assert!(err.contains("高于"), "错误信息应说明版本过高: {err}");
    }

    #[test]
    fn chunk_payload_roundtrip() {
        let header = ClipHeader {
            format_version: 2,
            block_palette: vec![
                "minecraft:air".into(),
                "minecraft:stone".into(),
                "minecraft:oak_planks".into(),
            ],
            biome_palette: vec!["minecraft:plains".into(), "minecraft:forest".into()],
            flags: FLAG_BIOMES,
            ..Default::default()
        };

        let mut indices = vec![0u32; SECTION_BLOCKS];
        for (i, v) in indices.iter_mut().enumerate() {
            *v = (i % 3) as u32;
        }
        let chunk = ClipChunk {
            section_base_y: -64,
            sections: vec![SectionBlocks::Indexed(indices), SectionBlocks::Empty],
            biomes: vec![Some(vec![1u32; SECTION_BIOMES]), Some(vec![0u32; SECTION_BIOMES])],
            block_entities: vec![],
            entities: vec![],
        };

        let raw = encode_chunk_payload(&chunk, &header).unwrap();
        let mut c = std::io::Cursor::new(&raw);
        let back = read_chunk_payload(&mut c, &header).unwrap();

        assert_eq!(back.section_base_y, -64);
        assert_eq!(back.sections.len(), 2);
        match &back.sections[0] {
            SectionBlocks::Indexed(v) => {
                assert_eq!(v.len(), SECTION_BLOCKS);
                for (i, x) in v.iter().enumerate() {
                    assert_eq!(*x, (i % 3) as u32, "第 {i} 个方块不一致");
                }
            }
            _ => panic!("第 1 段应为 Indexed"),
        }
        assert!(matches!(back.sections[1], SectionBlocks::Empty));
        assert_eq!(back.biomes[0].as_ref().unwrap()[0], 1);
    }

    #[test]
    fn unpack_block_states_no_cross_boundary() {
        // 4 位 → 每个 long 放 16 个（64/4=16）
        // 手造两个 long：第一个放 16 个值 1，第二个放 16 个值 2
        let mut data = vec![0i64; 2];
        for i in 0..16 {
            data[0] |= 1i64 << (i * 4);
            data[1] |= 2i64 << (i * 4);
        }
        let map = [0u32, 10, 20];
        let out = unpack_block_states(&data, &map, 32);
        assert_eq!(out[0], 10);
        assert_eq!(out[15], 10);
        assert_eq!(out[16], 20);
        assert_eq!(out[31], 20);
    }

    #[test]
    fn unpack_block_states_min_four_bits() {
        // 调色板只有 2 项时，1.16+ 仍用 4 位（不是 1 位）
        let mut data = vec![0i64; 1];
        data[0] |= 1i64 << (5 * 4); // 第 5 个值为 1
        let map = [0u32, 7];
        let out = unpack_block_states(&data, &map, 16);
        assert_eq!(out[5], 7);
        assert_eq!(out[4], 0);
        assert_eq!(out[6], 0);
    }
}
