//! NBT（Named Binary Tag）编解码。
//!
//! 设计目标：
//! - **字节级无损往返**：读出再写出必须与原文件逐字节一致，否则改存档会引入隐性破坏。
//!   因此 List 保留空列表的元素类型，Compound 保留插入顺序，浮点保留原始位模式。
//! - 正确处理 Java 的 **Modified UTF-8**（U+0000 编为 C0 80，增补平面字符按代理对编码），
//!   否则带 emoji 的世界名会读坏。
//! - 所有长度字段做上限校验，防止损坏文件导致巨量分配。

use indexmap::IndexMap;
use std::fmt;

pub type Compound = IndexMap<String, Tag>;

/// NBT 标签类型编号（与磁盘字节一致）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum TagType {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl TagType {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => TagType::End,
            1 => TagType::Byte,
            2 => TagType::Short,
            3 => TagType::Int,
            4 => TagType::Long,
            5 => TagType::Float,
            6 => TagType::Double,
            7 => TagType::ByteArray,
            8 => TagType::String,
            9 => TagType::List,
            10 => TagType::Compound,
            11 => TagType::IntArray,
            12 => TagType::LongArray,
            other => return Err(Error::UnknownTagType(other)),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            TagType::End => "End",
            TagType::Byte => "Byte",
            TagType::Short => "Short",
            TagType::Int => "Int",
            TagType::Long => "Long",
            TagType::Float => "Float",
            TagType::Double => "Double",
            TagType::ByteArray => "ByteArray",
            TagType::String => "String",
            TagType::List => "List",
            TagType::Compound => "Compound",
            TagType::IntArray => "IntArray",
            TagType::LongArray => "LongArray",
        }
    }
}

/// 一个 NBT 标签。类型信息随值走，因此 Compound 可以存放异构子标签。
#[derive(Clone, Debug)]
pub enum Tag {
    End,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List { elem: TagType, items: Vec<Tag> },
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn type_of(&self) -> TagType {
        match self {
            Tag::End => TagType::End,
            Tag::Byte(_) => TagType::Byte,
            Tag::Short(_) => TagType::Short,
            Tag::Int(_) => TagType::Int,
            Tag::Long(_) => TagType::Long,
            Tag::Float(_) => TagType::Float,
            Tag::Double(_) => TagType::Double,
            Tag::ByteArray(_) => TagType::ByteArray,
            Tag::String(_) => TagType::String,
            Tag::List { .. } => TagType::List,
            Tag::Compound(_) => TagType::Compound,
            Tag::IntArray(_) => TagType::IntArray,
            Tag::LongArray(_) => TagType::LongArray,
        }
    }

    pub fn new_list(elem: TagType, items: Vec<Tag>) -> Self {
        Tag::List { elem, items }
    }

    pub fn empty_list(elem: TagType) -> Self {
        Tag::List { elem, items: Vec::new() }
    }

    pub fn compound() -> Self {
        Tag::Compound(Compound::new())
    }

    /// 取整数族（Byte/Short/Int/Long）的值。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Tag::Byte(v) => Some(*v as i64),
            Tag::Short(v) => Some(*v as i64),
            Tag::Int(v) => Some(*v as i64),
            Tag::Long(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        self.as_i64().map(|v| v as i32)
    }

    pub fn as_i8(&self) -> Option<i8> {
        self.as_i64().map(|v| v as i8)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Tag::Float(v) => Some(*v as f64),
            Tag::Double(v) => Some(*v),
            Tag::Byte(v) => Some(*v as f64),
            Tag::Short(v) => Some(*v as f64),
            Tag::Int(v) => Some(*v as f64),
            Tag::Long(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_compound_mut(&mut self) -> Option<&mut Compound> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<(&TagType, &[Tag])> {
        match self {
            Tag::List { elem, items } => Some((elem, items)),
            _ => None,
        }
    }

    pub fn as_list_mut(&mut self) -> Option<(&TagType, &mut Vec<Tag>)> {
        match self {
            Tag::List { elem, items } => Some((elem, items)),
            _ => None,
        }
    }

    pub fn as_byte_array(&self) -> Option<&[u8]> {
        match self {
            Tag::ByteArray(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_int_array(&self) -> Option<&[i32]> {
        match self {
            Tag::IntArray(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Tag::LongArray(v) => Some(v),
            _ => None,
        }
    }

    /// 便捷构造：整数自动选 Int。
    pub fn int(v: i32) -> Self {
        Tag::Int(v)
    }

    pub fn long(v: i64) -> Self {
        Tag::Long(v)
    }

    pub fn string(s: impl Into<String>) -> Self {
        Tag::String(s.into())
    }

    /// 递归字节数估算（用于 UI 显示体积，不做精确序列化）。
    pub fn approx_size(&self) -> usize {
        match self {
            Tag::End => 1,
            Tag::Byte(_) => 2,
            Tag::Short(_) => 3,
            Tag::Int(_) => 5,
            Tag::Long(_) => 9,
            Tag::Float(_) => 5,
            Tag::Double(_) => 9,
            Tag::ByteArray(v) => 5 + v.len(),
            Tag::String(s) => 3 + s.len(),
            Tag::List { items, .. } => 5 + items.iter().map(|t| t.approx_size()).sum::<usize>(),
            Tag::Compound(c) => {
                1 + c.iter().map(|(k, v)| 3 + k.len() + v.approx_size()).sum::<usize>()
            }
            Tag::IntArray(v) => 5 + v.len() * 4,
            Tag::LongArray(v) => 5 + v.len() * 8,
        }
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("未知的标签类型: {0}")]
    UnknownTagType(u8),
    #[error("数据在偏移 {pos} 处意外结束（需要 {need} 字节）")]
    UnexpectedEof { pos: usize, need: usize },
    #[error("长度字段异常: {0}")]
    BadLength(u64),
    #[error("非法 UTF-8 序列: {0}")]
    BadUtf8(String),
    #[error("嵌套层数超过上限 {0}")]
    TooDeep(usize),
    #[error("列表元素类型不一致：声明 {declared:?}，实际 {actual:?}")]
    ListTypeMismatch { declared: TagType, actual: TagType },
    #[error("压缩数据错误: {0}")]
    Compression(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

const MAX_DEPTH: usize = 512;
/// 单个数组/字符串的长度上限（256 MiB），防止损坏文件撑爆内存。
const MAX_ALLOC: u64 = 256 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Modified UTF-8
// ---------------------------------------------------------------------------

/// 解码 Java Modified UTF-8。
fn mutf8_decode(bytes: &[u8]) -> Result<String> {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == 0xC0 && i + 1 < bytes.len() && bytes[i + 1] == 0x80 {
            out.push('\0');
            i += 2;
            continue;
        }
        if b & 0xE0 == 0xC0 {
            if i + 1 >= bytes.len() {
                return Err(Error::BadUtf8("截断的双字节序列".into()));
            }
            let cp = (((b & 0x1F) as u32) << 6) | ((bytes[i + 1] & 0x3F) as u32);
            let ch = char::from_u32(cp)
                .ok_or_else(|| Error::BadUtf8(format!("非法码点 U+{cp:04X}")))?;
            out.push(ch);
            i += 2;
            continue;
        }
        if b & 0xF0 == 0xE0 {
            if i + 2 >= bytes.len() {
                return Err(Error::BadUtf8("截断的三字节序列".into()));
            }
            let cp = (((b & 0x0F) as u32) << 12)
                | (((bytes[i + 1] & 0x3F) as u32) << 6)
                | ((bytes[i + 2] & 0x3F) as u32);
            i += 3;
            // 代理对（CESU-8）：D800-DBFF 之后紧跟 DC00-DFFF
            if (0xD800..=0xDBFF).contains(&cp) {
                if i + 2 < bytes.len() {
                    let b1 = bytes[i];
                    if b1 & 0xF0 == 0xE0 {
                        let cp2 = (((b1 & 0x0F) as u32) << 12)
                            | (((bytes[i + 1] & 0x3F) as u32) << 6)
                            | ((bytes[i + 2] & 0x3F) as u32);
                        if (0xDC00..=0xDFFF).contains(&cp2) {
                            let combined =
                                0x10000 + ((cp - 0xD800) << 10) + (cp2 - 0xDC00);
                            if let Some(ch) = char::from_u32(combined) {
                                out.push(ch);
                                i += 3;
                                continue;
                            }
                        }
                    }
                }
                // 孤立代理项：替换为 U+FFFD，避免整体失败
                out.push('\u{FFFD}');
                continue;
            }
            let ch = char::from_u32(cp)
                .ok_or_else(|| Error::BadUtf8(format!("非法码点 U+{cp:04X}")))?;
            out.push(ch);
            continue;
        }
        // 4 字节 UTF-8 出现在标准 UTF-8 文件里，容错处理
        if b & 0xF8 == 0xF0 && i + 3 < bytes.len() {
            let cp = (((b & 0x07) as u32) << 18)
                | (((bytes[i + 1] & 0x3F) as u32) << 12)
                | (((bytes[i + 2] & 0x3F) as u32) << 6)
                | ((bytes[i + 3] & 0x3F) as u32);
            if let Some(ch) = char::from_u32(cp) {
                out.push(ch);
                i += 4;
                continue;
            }
        }
        return Err(Error::BadUtf8(format!("字节 0x{b:02X} 处无法解码")));
    }
    Ok(out)
}

/// 编码为 Java Modified UTF-8。
fn mutf8_encode(s: &str, out: &mut Vec<u8>) {
    for ch in s.chars() {
        let c = ch as u32;
        if c == 0 {
            out.push(0xC0);
            out.push(0x80);
        } else if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.push(0xC0 | (c >> 6) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else if c < 0x10000 {
            out.push(0xE0 | (c >> 12) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else {
            // 增补平面：拆成代理对，各 3 字节（CESU-8）
            let v = c - 0x10000;
            let hi = 0xD800 + (v >> 10);
            let lo = 0xDC00 + (v & 0x3FF);
            for cp in [hi, lo] {
                out.push(0xE0 | (cp >> 12) as u8);
                out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                out.push(0x80 | (cp & 0x3F) as u8);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 读取
// ---------------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.buf.len() {
            return Err(Error::UnexpectedEof { pos: self.pos, need: n });
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    fn i16(&mut self) -> Result<i16> {
        self.need(2)?;
        let v = i16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(self.i16()? as u16)
    }

    fn i32(&mut self) -> Result<i32> {
        self.need(4)?;
        let v = i32::from_be_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(self.i32()? as u32)
    }

    fn i64(&mut self) -> Result<i64> {
        self.need(8)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(i64::from_be_bytes(b))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.i64()? as u64))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        self.need(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn string(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        let bytes = self.take(n)?;
        mutf8_decode(bytes)
    }

    fn payload(&mut self, t: TagType, depth: usize) -> Result<Tag> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep(MAX_DEPTH));
        }
        Ok(match t {
            TagType::End => Tag::End,
            TagType::Byte => Tag::Byte(self.i8()?),
            TagType::Short => Tag::Short(self.i16()?),
            TagType::Int => Tag::Int(self.i32()?),
            TagType::Long => Tag::Long(self.i64()?),
            TagType::Float => Tag::Float(self.f32()?),
            TagType::Double => Tag::Double(self.f64()?),
            TagType::ByteArray => {
                let n = self.i32()?;
                if n < 0 || n as u64 > MAX_ALLOC {
                    return Err(Error::BadLength(n as u64));
                }
                Tag::ByteArray(self.take(n as usize)?.to_vec())
            }
            TagType::String => Tag::String(self.string()?),
            TagType::List => {
                let elem = TagType::from_u8(self.u8()?)?;
                let n = self.i32()?;
                if n < 0 || n as u64 > MAX_ALLOC {
                    return Err(Error::BadLength(n as u64));
                }
                let mut items = Vec::with_capacity((n as usize).min(4096));
                for _ in 0..n {
                    items.push(self.payload(elem, depth + 1)?);
                }
                Tag::List { elem, items }
            }
            TagType::Compound => {
                let mut map = Compound::new();
                loop {
                    let t = TagType::from_u8(self.u8()?)?;
                    if t == TagType::End {
                        break;
                    }
                    let name = self.string()?;
                    let v = self.payload(t, depth + 1)?;
                    map.insert(name, v);
                }
                Tag::Compound(map)
            }
            TagType::IntArray => {
                let n = self.i32()?;
                if n < 0 || (n as u64) * 4 > MAX_ALLOC {
                    return Err(Error::BadLength(n as u64));
                }
                let mut items = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    items.push(self.i32()?);
                }
                Tag::IntArray(items)
            }
            TagType::LongArray => {
                let n = self.i32()?;
                if n < 0 || (n as u64) * 8 > MAX_ALLOC {
                    return Err(Error::BadLength(n as u64));
                }
                let mut items = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    items.push(self.i64()?);
                }
                Tag::LongArray(items)
            }
        })
    }
}

/// 解析一段完整的命名 NBT 文档，返回 `(根名字, 根标签)`。
///
/// 体数据（chunk / level.dat / playerdata）都是这种结构；根标签名通常为空串。
pub fn read_named(buf: &[u8]) -> Result<(String, Tag)> {
    let mut r = Reader::new(buf);
    let t = TagType::from_u8(r.u8()?)?;
    if t == TagType::End {
        return Ok((String::new(), Tag::End));
    }
    let name = r.string()?;
    let value = r.payload(t, 0)?;
    Ok((name, value))
}

/// 只取根标签（忽略名字）。
pub fn read(buf: &[u8]) -> Result<Tag> {
    Ok(read_named(buf)?.1)
}

// ---------------------------------------------------------------------------
// 写入
// ---------------------------------------------------------------------------

fn write_string(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let mut tmp = Vec::with_capacity(s.len());
    mutf8_encode(s, &mut tmp);
    if tmp.len() > u16::MAX as usize {
        return Err(Error::BadLength(tmp.len() as u64));
    }
    out.extend_from_slice(&(tmp.len() as u16).to_be_bytes());
    out.extend_from_slice(&tmp);
    Ok(())
}

fn write_payload(out: &mut Vec<u8>, t: &Tag) -> Result<()> {
    match t {
        Tag::End => {}
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::ByteArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            out.extend_from_slice(v);
        }
        Tag::String(s) => write_string(out, s)?,
        Tag::List { elem, items } => {
            // 空列表保留原始元素类型；非空时若类型为 End 则按首元素推导（容错）
            let actual = if items.is_empty() {
                *elem
            } else if *elem == TagType::End {
                items[0].type_of()
            } else {
                *elem
            };
            out.push(actual as u8);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for it in items {
                if it.type_of() != actual && actual != TagType::End {
                    return Err(Error::ListTypeMismatch { declared: actual, actual: it.type_of() });
                }
                write_payload(out, it)?;
            }
        }
        Tag::Compound(map) => {
            for (k, v) in map {
                let ty = v.type_of();
                if ty == TagType::End {
                    continue;
                }
                out.push(ty as u8);
                write_string(out, k)?;
                write_payload(out, v)?;
            }
            out.push(TagType::End as u8);
        }
        Tag::IntArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
        Tag::LongArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for x in v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
    }
    Ok(())
}

/// 序列化为「类型字节 + 名字 + 体」的完整文档。
pub fn write_named(name: &str, tag: &Tag) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(tag.approx_size() + 8);
    out.push(tag.type_of() as u8);
    write_string(&mut out, name)?;
    write_payload(&mut out, tag)?;
    Ok(out)
}

/// 序列化匿名根（名字为空），用于 chunk 体。
pub fn write(tag: &Tag) -> Result<Vec<u8>> {
    write_named("", tag)
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tag::End => write!(f, "<END>"),
            Tag::Byte(v) => write!(f, "{v}b"),
            Tag::Short(v) => write!(f, "{v}s"),
            Tag::Int(v) => write!(f, "{v}"),
            Tag::Long(v) => write!(f, "{v}L"),
            Tag::Float(v) => write!(f, "{v}f"),
            Tag::Double(v) => write!(f, "{v}d"),
            Tag::ByteArray(v) => write!(f, "[B; {}]", v.len()),
            Tag::String(s) => write!(f, "\"{s}\""),
            Tag::List { items, .. } => write!(f, "[{} 项]", items.len()),
            Tag::Compound(c) => write!(f, "{{{}}}", c.len()),
            Tag::IntArray(v) => write!(f, "[I; {}]", v.len()),
            Tag::LongArray(v) => write!(f, "[L; {}]", v.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutf8_roundtrip() {
        for s in ["hello", "新的世界", "a\0b", "emoji 😀", "𝔘𝔫𝔦𝔠𝔬𝔡𝔢"] {
            let mut buf = Vec::new();
            mutf8_encode(s, &mut buf);
            assert_eq!(mutf8_decode(&buf).unwrap(), s, "roundtrip failed for {s:?}");
        }
    }

    #[test]
    fn mutf8_null_is_c080() {
        let mut buf = Vec::new();
        mutf8_encode("\0", &mut buf);
        assert_eq!(buf, vec![0xC0, 0x80]);
    }

    #[test]
    fn mutf8_supplementary_is_cesu8() {
        // U+1F600 应编码为 6 字节代理对，而非标准 UTF-8 的 4 字节
        let mut buf = Vec::new();
        mutf8_encode("😀", &mut buf);
        assert_eq!(buf.len(), 6);
        assert_eq!(mutf8_decode(&buf).unwrap(), "😀");
    }

    #[test]
    fn byte_roundtrip_preserves_empty_list_type() {
        let mut c = Compound::new();
        c.insert("Entities".into(), Tag::empty_list(TagType::Compound));
        c.insert("ScheduledEvents".into(), Tag::empty_list(TagType::End));
        c.insert("Time".into(), Tag::Long(2950));
        c.insert("Name".into(), Tag::String("新的世界".into()));
        let tag = Tag::Compound(c);

        let bytes = write(&tag).unwrap();
        let back = read(&bytes).unwrap();
        let bytes2 = write(&back).unwrap();
        assert_eq!(bytes, bytes2, "二次序列化结果不一致");

        let (et, items) = back.as_compound().unwrap()["Entities"].as_list().unwrap();
        assert_eq!(*et, TagType::Compound);
        assert!(items.is_empty());
        let (et2, _) = back.as_compound().unwrap()["ScheduledEvents"].as_list().unwrap();
        assert_eq!(*et2, TagType::End);
    }

    #[test]
    fn preserves_float_bit_patterns() {
        let mut c = Compound::new();
        c.insert("f".into(), Tag::Float(f32::from_bits(0x7FC0_0001))); // NaN payload
        c.insert("d".into(), Tag::Double(f64::from_bits(0x7FF8_0000_0000_0001)));
        c.insert("neg".into(), Tag::Double(-0.0));
        let bytes = write(&Tag::Compound(c)).unwrap();
        let back = read(&bytes).unwrap();
        let c2 = back.as_compound().unwrap();
        // 必须逐类型比较位模式：f32 经 f64 转换会重排 NaN payload
        match &c2["f"] {
            Tag::Float(v) => assert_eq!(v.to_bits(), 0x7FC0_0001),
            other => panic!("期望 Float，实际 {:?}", other.type_of()),
        }
        match &c2["d"] {
            Tag::Double(v) => assert_eq!(v.to_bits(), 0x7FF8_0000_0000_0001),
            other => panic!("期望 Double，实际 {:?}", other.type_of()),
        }
        match &c2["neg"] {
            Tag::Double(v) => assert!(v.is_sign_negative() && *v == 0.0),
            other => panic!("期望 Double，实际 {:?}", other.type_of()),
        }
    }

    #[test]
    fn rejects_truncated_input() {
        let bytes = write(&Tag::Compound(Compound::new())).unwrap();
        let r = read(&bytes[..bytes.len() - 1]);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_giant_allocation() {
        // 构造一个声称为极大数组的头部
        let mut buf = vec![TagType::IntArray as u8, 0, 0];
        buf.extend_from_slice(&i32::MAX.to_be_bytes());
        assert!(matches!(read(&buf), Err(Error::BadLength(_))));
    }
}
