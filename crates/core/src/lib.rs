//! 存档内核：NBT 编解码、Anvil 区域文件、世界模型。
//!
//! 只做纯数据层，不涉及 UI，便于独立测试与复用。

pub mod anvil;
pub mod chunk;
pub mod colors;
pub mod edit;
pub mod nbt;
pub mod regions;
pub mod safety;
pub mod smclip;
pub mod swiftmap;
pub mod world;

pub use anvil::RegionFile;
pub use chunk::Column;
pub use colors::Rgb;
pub use nbt::{Compound, Tag, TagType};
pub use world::{
    ChunkMeta, ChunkSurface, Dimension, EntityMeta, LevelDat, RegionKind, World,
};

/// 本工具支持的 Minecraft 数据版本（1.21.1）。
pub const SUPPORTED_DATA_VERSION: i32 = 3955;

/// 1.21.1 的方块状态用复合物表示（`{Name, Properties}`）。
pub const BLOCK_PALETTE_IS_COMPOUND: bool = true;
