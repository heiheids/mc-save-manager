//! 全图导出为 HTML。
//!
//! 为什么不内置「全图」视图：实测 26.3 存档主世界有 82419 个区块，
//! 全图缩放下每区块不足 1 像素，程序内既看不清也拖不动（每次拖动都要
//! 重新排队上万个区块）。改成导出一个静态 HTML：
//!   - 浏览器里可以用鼠标滚轮/拖动自由缩放，性能远好于即时渲染
//!   - 可以把鼠标处的坐标、群系直接标出来
//!   - 一次导出存档，之后不依赖本程序
//!
//! 产物是**单个自包含 HTML**：贴图以 base64 内嵌，不产生外部依赖，
//! 方便发给别人或者存档留档。

use std::collections::BTreeMap;
use std::path::Path;

use mcworld_core::chunk::{self, SliceMode};
use mcworld_core::colors;
use mcworld_core::world::{Dimension, RegionCache, World};

/// 每个区块在导出的贴图里占的像素数。
///
/// 16 的好处是 1 像素 = 1 方块，浏览器里放大也不会糊；
/// 缺点是 8 万区块 = 8 万 × 256 像素 = 2000 万像素，PNG 约 10-20 MB。
///
/// 因此按总量自适应：区块多时降到 8（1 像素 = 2 方块）甚至 4。
fn tile_px_for(total_chunks: usize) -> usize {
    if total_chunks <= 20_000 {
        16
    } else if total_chunks <= 60_000 {
        8
    } else {
        4
    }
}

/// 单个维度的导出结果。
pub struct DimImage {
    pub dim_id: String,
    pub dim_display: String,
    /// 覆盖的区块范围（闭区间）。
    pub bounds: (i32, i32, i32, i32),
    /// 贴图边长（像素）。
    pub width: u32,
    pub height: u32,
    /// 每区块像素数。
    pub tile_px: usize,
    /// RGBA 像素。
    pub rgba: Vec<u8>,
    /// 采样到的区块数。
    pub chunk_count: usize,
    /// 区块坐标 → 群系（只记录有数据的）。
    pub biomes: BTreeMap<(i32, i32), String>,
}

/// 把某个维度渲染成一张大图 + 群系表。
pub fn render_dimension(
    world: &World,
    dim: &Dimension,
    mode: SliceMode,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<DimImage, String> {
    let metas = world
        .scan_chunks(dim, &mut |_, _| {})
        .map_err(|e| e.to_string())?;
    if metas.is_empty() {
        return Err(format!("{} 没有区块", dim.display));
    }

    // 范围
    let (mut min_x, mut max_x, mut min_z, mut max_z) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for m in &metas {
        min_x = min_x.min(m.x);
        max_x = max_x.max(m.x);
        min_z = min_z.min(m.z);
        max_z = max_z.max(m.z);
    }

    let tile = tile_px_for(metas.len());
    let cols = (max_x - min_x + 1) as u32;
    let rows = (max_z - min_z + 1) as u32;
    let width = cols * tile as u32;
    let height = rows * tile as u32;

    // 上限保护：RGBA 超过 ~320 MB 就拒绝（10000×8000 像素左右）
    let pixels = width as u64 * height as u64;
    if pixels > 80_000_000 {
        return Err(format!(
            "{} 太大（{width}×{height} 像素），请先在程序里用「保存缓存」减少重算，\
             或分区域导出",
            dim.display
        ));
    }

    let mut rgba = vec![0u8; pixels as usize * 4];
    // 底色：深灰蓝，与程序内一致，避免透明区域看起来像"漏了"
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 0x1A;
        px[1] = 0x1C;
        px[2] = 0x20;
        px[3] = 0xFF;
    }

    let mut biomes: BTreeMap<(i32, i32), String> = BTreeMap::new();
    let mut cache = RegionCache::new(64);
    let total = metas.len();
    let mut done = 0usize;

    for m in &metas {
        done += 1;
        if done % 64 == 0 {
            progress(done, total);
        }

        let Some(surf) = world.chunk_surface(dim, m.x, m.z, &mut cache, mode) else {
            continue;
        };

        let bx = ((m.x - min_x) as u32) * tile as u32;
        let bz = ((m.z - min_z) as u32) * tile as u32;
        // 每格多少方块：16 / tile。tile=16 → 1；tile=8 → 2；tile=4 → 4
        let step = 16 / tile;

        for ty in 0..tile {
            for tx in 0..tile {
                // 该像素代表区块内的第几列
                let lx = tx * step;
                let lz = ty * step;
                let idx = lz * 16 + lx;
                let Some(col) = surf.columns.get(idx) else {
                    continue;
                };
                let c = if col.y < 0 {
                    [0x2A, 0x2E, 0x38]
                } else {
                    colors::block_color(&col.block)
                };

                let px = ((bz + ty as u32) * width + (bx + tx as u32)) as usize * 4;
                rgba[px] = c[0];
                rgba[px + 1] = c[1];
                rgba[px + 2] = c[2];
                rgba[px + 3] = 0xFF;
            }
        }

        // 群系：取区块中心高度的群系
        let cx_world = m.x * 16 + 8;
        let cz_world = m.z * 16 + 8;
        let y = surf
            .columns
            .get(8 * 16 + 8)
            .map(|c| if c.y < 0 { 64 } else { c.y })
            .unwrap_or(64);
        if let Some(b) = world.biome_at(dim, cx_world, y, cz_world, &mut cache) {
            biomes.insert((m.x, m.z), b);
        }
    }

    progress(total, total);

    Ok(DimImage {
        dim_id: dim.id.clone(),
        dim_display: dim.display.clone(),
        bounds: (min_x, min_z, max_x, max_z),
        width,
        height,
        tile_px: tile,
        rgba,
        chunk_count: metas.len(),
        biomes,
    })
}

// ---------------------------------------------------------------------------
// PNG 编码
// ---------------------------------------------------------------------------

/// 极简 PNG 编码（RGBA，无滤波）。
///
/// 不引入 image crate：它体积大，而我们只需要"把 RGBA 压成 PNG"这一件事。
/// deflate 用 flate2（已经是依赖）。
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT：每行前加一个 filter byte(0)
    let row_bytes = width as usize * 4;
    let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * row_bytes..(y + 1) * row_bytes]);
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).map_err(|e| e.to_string())?;
    let compressed = enc.finish().map_err(|e| e.to_string())?;
    write_chunk(&mut out, b"IDAT", &compressed);

    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC-32（PNG 用的那种）。
struct Crc32 {
    table: [u32; 256],
    value: u32,
}

impl Crc32 {
    fn new() -> Self {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        Crc32 { table, value: 0xFFFF_FFFF }
    }

    fn update(&mut self, data: &[u8]) {
        for &b in data {
            let idx = ((self.value ^ b as u32) & 0xFF) as usize;
            self.value = self.table[idx] ^ (self.value >> 8);
        }
    }

    fn finish(&self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}

// ---------------------------------------------------------------------------
// HTML 生成
// ---------------------------------------------------------------------------

/// 把若干维度的渲染结果打包成单个自包含 HTML。
pub fn build_html(images: &[DimImage], world_name: &str) -> String {
    let mut tabs = String::new();
    let mut bodies = String::new();

    for (i, img) in images.iter().enumerate() {
        let (x0, z0, x1, z1) = img.bounds;
        let active = if i == 0 { "active" } else { "" };

        tabs.push_str(&format!(
            "<button class=\"tab {active}\" onclick=\"show({i})\">{}</button>",
            html_escape(&img.dim_display)
        ));

        // 群系表：JSON，键是 "cx,cz"
        let mut biome_json = String::from("{");
        for (k, (c, v)) in img.biomes.iter().enumerate() {
            if k > 0 {
                biome_json.push(',');
            }
            biome_json.push_str(&format!(
                "\"{},{}\":\"{}\"",
                c.0,
                c.1,
                html_escape(v)
            ));
        }
        biome_json.push('}');

        let png = encode_png(img.width, img.height, &img.rgba).unwrap_or_default();
        let b64 = base64(&png);

        bodies.push_str(&format!(
            r#"<div class="pane {active}" id="pane{i}">
  <div class="hud">
    <span class="dim">{dim}</span>
    <span>区块 {x0}, {z0} → {x1}, {z1}</span>
    <span>共 {n} 个区块</span>
    <span>每区块 {tile} 像素</span>
  </div>
  <div class="stage">
    <img id="img{i}" src="data:image/png;base64,{b64}" width="{w}" height="{h}"
         data-bx="{x0}" data-bz="{z0}" data-tile="{tile}"
         onmousemove="hover(event,{i})" onmouseleave="clearHud({i})">
  </div>
</div>
<script>BIOMES[{i}] = {biome_json};</script>
"#,
            active = active,
            dim = html_escape(&img.dim_display),
            x0 = x0,
            z0 = z0,
            x1 = x1,
            z1 = z1,
            n = img.chunk_count,
            tile = img.tile_px,
            b64 = b64,
            w = img.width,
            h = img.height,
            biome_json = biome_json,
        ));
    }

    let title = html_escape(world_name);

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>{title} — 存档地图</title>
<style>
  :root {{
    --bg: #16181c; --panel: #202329; --line: #34383f;
    --text: #e8e4d8; --dim: #9aa0a8; --accent: #9bb06a;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; background: var(--bg); color: var(--text);
    font: 14px/1.5 system-ui, "Microsoft YaHei", sans-serif;
  }}
  header {{
    position: sticky; top: 0; z-index: 10;
    display: flex; align-items: center; gap: 16px; flex-wrap: wrap;
    padding: 10px 16px; background: var(--panel);
    border-bottom: 1px solid var(--line);
  }}
  h1 {{ font-size: 16px; margin: 0; font-weight: 600; }}
  .tabs {{ display: flex; gap: 6px; margin-left: auto; }}
  .tab {{
    background: #2a2e35; color: var(--text); border: 1px solid var(--line);
    padding: 5px 14px; border-radius: 6px; cursor: pointer; font-size: 13px;
  }}
  .tab.active {{ background: var(--accent); color: #14170f; border-color: var(--accent); }}
  .pane {{ display: none; }}
  .pane.active {{ display: block; }}
  .hud {{
    padding: 8px 16px; color: var(--dim); font-size: 13px;
    display: flex; gap: 18px; flex-wrap: wrap;
  }}
  .hud .dim {{ color: var(--accent); font-weight: 600; }}
  .stage {{ overflow: auto; padding: 0 16px 24px; }}
  .stage img {{
    display: block; image-rendering: pixelated;
    background: #101216; border: 1px solid var(--line);
    cursor: crosshair;
  }}
  #cursor {{
    position: fixed; pointer-events: none; z-index: 100;
    background: rgba(20,22,26,.94); border: 1px solid var(--accent);
    border-radius: 6px; padding: 6px 10px; font-size: 13px;
    white-space: nowrap; display: none;
  }}
  #cursor b {{ color: var(--accent); }}
  footer {{
    padding: 10px 16px 28px; color: var(--dim); font-size: 12px;
  }}
</style>
</head>
<body>
<header>
  <h1>{title}</h1>
  <span style="color:var(--dim);font-size:13px">滚轮缩放由浏览器负责（Ctrl+滚轮）</span>
  <div class="tabs">{tabs}</div>
</header>
<div id="cursor"></div>
{bodies}
<footer>
  坐标说明：把鼠标放到地图上即可看到该处的方块坐标与群系。<br>
  区块坐标 × 16 = 方块坐标；本文件为静态快照，不包含实体与方块实体信息。
</footer>
<script>
var BIOMES = [];
function show(i) {{
  document.querySelectorAll('.pane').forEach(function (p, k) {{
    p.classList.toggle('active', k === i);
  }});
  document.querySelectorAll('.tab').forEach(function (t, k) {{
    t.classList.toggle('active', k === i);
  }});
}}
function hover(e, i) {{
  var img = e.target;
  var rect = img.getBoundingClientRect();
  // 屏幕像素 → 图片像素（考虑浏览器缩放）
  var px = (e.clientX - rect.left) * (img.naturalWidth / rect.width);
  var py = (e.clientY - rect.top) * (img.naturalHeight / rect.height);

  var tile = +img.dataset.tile;
  var bx = +img.dataset.bx;
  var bz = +img.dataset.bz;

  var cx = bx + Math.floor(px / tile);
  var cz = bz + Math.floor(py / tile);
  // 图片像素 → 方块：每图片像素 = 16/tile 方块
  var step = 16 / tile;
  var wx = cx * 16 + Math.floor((px % tile) * step);
  var wz = cz * 16 + Math.floor((py % tile) * step);

  var box = document.getElementById('cursor');
  var biome = (BIOMES[i] && BIOMES[i][cx + ',' + cz]) || '未知';
  box.innerHTML = '<b>X ' + wx + ' Z ' + wz + '</b><br>区块 ' + cx + ', ' + cz +
                  '<br>群系 ' + biome;
  box.style.display = 'block';
  box.style.left = (e.clientX + 16) + 'px';
  box.style.top = (e.clientY + 16) + 'px';
}}
function clearHud(i) {{
  document.getElementById('cursor').style.display = 'none';
}}
</script>
</body>
</html>
"#,
        title = title,
        tabs = tabs,
        bodies = bodies
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 一次性导出：`dims` 为空表示导出全部维度。
pub fn export_html(
    world_root: &Path,
    dims: &[String],
    out_path: &Path,
    mode: SliceMode,
    progress: &mut dyn FnMut(&str, usize, usize),
) -> Result<String, String> {
    let world = World::open(world_root).map_err(|e| e.to_string())?;
    let world_name = world.level.level_name();

    let selected: Vec<Dimension> = world
        .dimensions
        .iter()
        .filter(|d| dims.is_empty() || dims.iter().any(|s| s == &d.id))
        .filter(|d| d.exists(mcworld_core::world::RegionKind::Terrain))
        .cloned()
        .collect();

    if selected.is_empty() {
        return Err("没有可导出的维度".into());
    }

    let mut images = Vec::new();
    for dim in &selected {
        let mut cb = |done: usize, total: usize| {
            progress(&dim.display, done, total);
        };
        let img = render_dimension(&world, dim, mode, &mut cb)?;
        images.push(img);
    }

    let html = build_html(&images, &world_name);
    std::fs::write(out_path, html.as_bytes()).map_err(|e| e.to_string())?;

    let total_chunks: usize = images.iter().map(|i| i.chunk_count).sum();
    Ok(format!(
        "已导出 {} 个维度、{} 个区块 → {}",
        images.len(),
        total_chunks,
        out_path.display()
    ))
}
