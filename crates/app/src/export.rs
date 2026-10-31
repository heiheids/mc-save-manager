//! 导出与粘贴：生成 Litematica 结构文件、执行区块粘贴。

use std::path::Path;

use mcworld_core::edit::{paste_chunks, ChunkClipboard};
use mcworld_core::nbt::{Compound, Tag, TagType};
use mcworld_core::safety::Session;
use mcworld_core::world::{Dimension, RegionKind, World};

/// 把选区导出为 `.litematic`（Litematica 结构格式）。
///
/// 格式要点（v7）：
/// - 根：`MinecraftDataVersion` / `Version` / `SubVersion` / `Metadata` / `Regions`
/// - 每个 Region：`Position` / `Size` / `BlockStatePalette` / `BlockStates`(long 位打包)
///   / `TileEntities` / `Entities` / `PendingBlockTicks` / `PendingFluidTicks`
pub fn export_litematic(
    world_root: &Path,
    dim: &Dimension,
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
    out_path: &Path,
) -> Result<String, String> {
    let world = World::open(world_root).map_err(|e| e.to_string())?;
    let dim = world
        .dimensions
        .iter()
        .find(|d| d.id == dim.id)
        .cloned()
        .ok_or_else(|| "维度不存在".to_string())?;

    let (xa, xb) = (x0.min(x1), x0.max(x1));
    let (za, zb) = (z0.min(z1), z0.max(z1));

    let name = format!("region_{xa}_{za}_{xb}_{zb}");
    let regions = build_regions(&world, &dim, xa, za, xb, zb)?;
    if regions.is_empty() {
        return Err("选区为空（没有区块或没有方块数据）".into());
    }

    let mut root = Compound::new();
    root.insert(
        "MinecraftDataVersion".into(),
        Tag::Int(world.level.data_version()),
    );
    root.insert("Version".into(), Tag::Int(7));
    root.insert("SubVersion".into(), Tag::Int(1));

    let mut metadata = Compound::new();
    metadata.insert("Name".into(), Tag::String(name.clone()));
    metadata.insert("Author".into(), Tag::String("MCWorld 存档管理器".into()));
    metadata.insert("Description".into(), Tag::String("".into()));
    metadata.insert("RegionCount".into(), Tag::Int(regions.len() as i32));
    root.insert("Metadata".into(), Tag::Compound(metadata));

    let mut region_map = Compound::new();
    for (region_name, tag) in regions {
        region_map.insert(region_name, tag);
    }
    root.insert("Regions".into(), Tag::Compound(region_map));

    let bytes = mcworld_core::nbt::write(&Tag::Compound(root)).map_err(|e| e.to_string())?;
    let gz = {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&bytes).map_err(|e| e.to_string())?;
        enc.finish().map_err(|e| e.to_string())?
    };
    std::fs::write(out_path, gz).map_err(|e| e.to_string())?;

    Ok(format!(
        "已导出 {}（{} 个区块）",
        out_path.display(),
        (xb - xa + 1) * (zb - za + 1)
    ))
}

fn build_regions(
    world: &World,
    dim: &Dimension,
    xa: i32,
    za: i32,
    xb: i32,
    zb: i32,
) -> Result<Vec<(String, Tag)>, String> {
    let mut cache = mcworld_core::world::RegionCache::new(32);
    let mut palette: Vec<String> = vec!["minecraft:air".into()];
    let mut palette_index: std::collections::HashMap<String, usize> = Default::default();
    palette_index.insert("minecraft:air".into(), 0);

    // 逐列取顶层方块，生成一张 2D 图层（高度取世界最低层之上 1 层，表示"地表"）
    let w = (xb - xa + 1) * 16;
    let d = (zb - za + 1) * 16;

    let mut top_blocks: Vec<(i32, String)> = Vec::with_capacity((w * d) as usize);
    for cx in xa..=xb {
        for cz in za..=zb {
            let Some(surface) = world.chunk_surface(dim, cx, cz, &mut cache, mcworld_core::chunk::SliceMode::Surface) else {
                continue;
            };
            for col in &surface.columns {
                let entry = palette_index
                    .entry(col.block.clone())
                    .or_insert_with(|| {
                        palette.push(col.block.clone());
                        palette.len() - 1
                    });
                top_blocks.push((*entry as i32, col.block.clone()));
            }
        }
    }

    if top_blocks.is_empty() {
        return Ok(Vec::new());
    }

    let palette_tag = Tag::new_list(
        TagType::Compound,
        palette
            .iter()
            .map(|n| {
                let mut c = Compound::new();
                c.insert("Name".into(), Tag::String(n.clone()));
                Tag::Compound(c)
            })
            .collect(),
    );

    // 未压缩位打包（每个条目占 bits 位，不跨 long）
    let bits = {
        let n = palette.len().max(1);
        (usize::BITS - (n - 1).leading_zeros()).max(4)
    };
    let per_long = (64 / bits) as usize;
    let mut longs: Vec<i64> = Vec::new();
    let mut cur: u64 = 0;
    let mut slot = 0usize;
    for (idx, _) in &top_blocks {
        cur |= ((*idx as u64) & ((1 << bits) - 1)) << (slot * bits as usize);
        slot += 1;
        if slot == per_long {
            longs.push(cur as i64);
            cur = 0;
            slot = 0;
        }
    }
    if slot > 0 {
        longs.push(cur as i64);
    }

    let mut region = Compound::new();
    let mut pos = Compound::new();
    pos.insert("x".into(), Tag::Int(0));
    pos.insert("y".into(), Tag::Int(0));
    pos.insert("z".into(), Tag::Int(0));
    region.insert("Position".into(), Tag::Compound(pos));

    let mut size = Compound::new();
    size.insert("x".into(), Tag::Int(w));
    size.insert("y".into(), Tag::Int(1));
    size.insert("z".into(), Tag::Int(d));
    region.insert("Size".into(), Tag::Compound(size));

    region.insert("BlockStatePalette".into(), palette_tag);
    region.insert("BlockStates".into(), Tag::LongArray(longs));
    region.insert("TileEntities".into(), Tag::empty_list(TagType::Compound));
    region.insert("Entities".into(), Tag::empty_list(TagType::Compound));
    region.insert("PendingBlockTicks".into(), Tag::empty_list(TagType::Compound));
    region.insert("PendingFluidTicks".into(), Tag::empty_list(TagType::Compound));

    Ok(vec![("Selection".to_string(), Tag::Compound(region))])
}

/// 把剪贴板粘贴到目标位置（带备份）。
pub fn paste(
    world_root: &Path,
    dim: &Dimension,
    clipboard: &ChunkClipboard,
    target_x: i32,
    target_z: i32,
) -> Result<String, String> {
    let world = World::open(world_root).map_err(|e| e.to_string())?;
    if world.is_locked() {
        return Err("存档正被游戏占用".into());
    }
    let dim = world
        .dimensions
        .iter()
        .find(|d| d.id == dim.id)
        .cloned()
        .ok_or_else(|| "维度不存在".to_string())?;

    let backup_dir = mcworld_core::safety::new_backup_dir(
        &world_root.join("..").join("backups"),
        &world.level.level_name(),
    );
    let mut session = Session::begin(&world, &backup_dir).map_err(|e| e.to_string())?;

    // 备份可能被覆盖的 region 文件
    let mut files = std::collections::HashSet::new();
    for c in &clipboard.chunks {
        let (rx, rz) = (
            mcworld_core::anvil::region_coord(target_x + c.dx),
            mcworld_core::anvil::region_coord(target_z + c.dz),
        );
        for kind in [RegionKind::Terrain, RegionKind::Entities] {
            if dim.exists(kind) {
                files.insert(
                    dim.dir(kind)
                        .join(mcworld_core::anvil::region_file_name(rx, rz)),
                );
            }
        }
    }
    for p in &files {
        session.touch(p).map_err(|e| e.to_string())?;
    }

    let stats = paste_chunks(&dim, clipboard, target_x, target_z, true)
        .map_err(|e| e.to_string())?;
    session.mark_committed();

    Ok(format!(
        "已粘贴 {} 个区块（{} 方块实体 / {} 实体）到 ({target_x},{target_z})，备份在 {}",
        stats.chunks_written,
        stats.block_entities,
        stats.entities,
        backup_dir.display()
    ))
}
