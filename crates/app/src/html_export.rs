//! 全图导出为 HTML。
//!
//! 为什么不内置「全图」视图：实测 26.3 存档主世界有 82419 个区块，
//! 全图缩放下每区块不足 1 像素，程序内既看不清也拖不动（每次拖动都要
//! 重新排队上万个区块）。改成导出一个静态 HTML：
//!   - 浏览器里可以用滚轮缩放、右键拖动，性能远好于即时渲染
//!   - 可以把鼠标处的坐标、群系直接标出来
//!   - 一次导出存档，之后不依赖本程序
//!
//! 产物是**单个自包含 HTML**：贴图以 base64 内嵌，不产生外部依赖。
//!
//! # 性能
//!
//! 导出优先复用缩略图缓存（见 `crate::cache`）。缓存命中时直接拿每区块
//! 16×16 的像素贴图，跳过"解压 + 解析 NBT + 扫 24 个 section"这一整套。
//! 只有 region 文件变过、或缓存里没有的区块才回退到读盘。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use mcworld_core::chunk::SliceMode;
use mcworld_core::colors;
use mcworld_core::world::{Dimension, RegionCache, RegionKind, World};

/// 每个区块在导出的贴图里占的像素数。
///
/// 16 的好处是 1 像素 = 1 方块，浏览器里放大也不会糊；
/// 缺点是 8 万区块 = 8 万 × 256 像素，RGBA 内存 80 MB 左右。
/// 因此按总量自适应。
fn tile_px_for(total_chunks: usize) -> usize {
    if total_chunks <= 20_000 {
        16
    } else if total_chunks <= 60_000 {
        8
    } else {
        4
    }
}

/// 缓存里可复用的数据：区块 → (16×16 RGBA, 群系)。
pub type CachedChunks = HashMap<(i32, i32), (Vec<u8>, Option<String>)>;

/// 单个维度的导出结果。
pub struct DimImage {
    pub dim_id: String,
    pub dim_display: String,
    /// 覆盖的区块范围（闭区间）。
    pub bounds: (i32, i32, i32, i32),
    pub width: u32,
    pub height: u32,
    /// 每区块像素数。
    pub tile_px: usize,
    /// RGBA 像素。
    pub rgba: Vec<u8>,
    pub chunk_count: usize,
    /// 区块坐标 → 群系。
    pub biomes: BTreeMap<(i32, i32), String>,
    /// 本维度复用 / 采样数量。
    pub reused: usize,
    pub sampled: usize,
}

/// 把某个维度渲染成一张大图 + 群系表。
pub fn render_dimension(
    world: &World,
    dim: &Dimension,
    mode: SliceMode,
    cached: &CachedChunks,
    progress: &mut dyn FnMut(&str, usize, usize),
) -> Result<DimImage, String> {
    // 扫描阶段也要报进度：8 万区块的存档光 scan_chunks 就要几分钟，
    // 期间不给反馈的话界面上就是个不动的"准备中…"。
    let metas = {
        let mut sc = |done: usize, total: usize| progress("扫描区块", done, total);
        world
            .scan_chunks(dim, &mut sc)
            .map_err(|e| e.to_string())?
    };
    if metas.is_empty() {
        return Err(format!("{} 没有区块", dim.display));
    }

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

    let pixels = width as u64 * height as u64;
    if pixels > 80_000_000 {
        return Err(format!(
            "{} 太大（{width}×{height} 像素），请分区域导出",
            dim.display
        ));
    }

    let mut rgba = vec![0u8; pixels as usize * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 0x1A;
        px[1] = 0x1C;
        px[2] = 0x20;
        px[3] = 0xFF;
    }

    let mut biomes: BTreeMap<(i32, i32), String> = BTreeMap::new();
    let mut cache = RegionCache::new(64);
    let total = metas.len();
    let mut reused = 0usize;
    let mut sampled = 0usize;

    // 每像素代表区块内的第几列（tile=16 → 1，tile=8 → 2，tile=4 → 4）
    let step = 16 / tile;

    for (i, m) in metas.iter().enumerate() {
        if i % 32 == 0 {
            progress("渲染区块", i, total);
        }

        let bx = ((m.x - min_x) as u32) * tile as u32;
        let bz = ((m.z - min_z) as u32) * tile as u32;

        // ---- 优先走缓存 ----
        let mut done = false;
        if let Some((px, biome)) = cached.get(&(m.x, m.z)) {
            if px.len() == 16 * 16 * 4 {
                for ty in 0..tile {
                    for tx in 0..tile {
                        // 缓存是 16×16，按 step 取样降采样到 tile×tile
                        let sx = tx * step;
                        let sy = ty * step;
                        let si = (sy * 16 + sx) * 4;
                        let di = ((bz + ty as u32) * width + (bx + tx as u32)) as usize * 4;
                        rgba[di] = px[si];
                        rgba[di + 1] = px[si + 1];
                        rgba[di + 2] = px[si + 2];
                        rgba[di + 3] = 0xFF;
                    }
                }
                if let Some(b) = biome {
                    biomes.insert((m.x, m.z), b.clone());
                }
                reused += 1;
                done = true;
            }
        }

        if done {
            continue;
        }

        // ---- 缓存未命中：读盘 ----
        let Some(surf) = world.chunk_surface(dim, m.x, m.z, &mut cache, mode) else {
            continue;
        };
        sampled += 1;

        for ty in 0..tile {
            for tx in 0..tile {
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
                let di = ((bz + ty as u32) * width + (bx + tx as u32)) as usize * 4;
                rgba[di] = c[0];
                rgba[di + 1] = c[1];
                rgba[di + 2] = c[2];
                rgba[di + 3] = 0xFF;
            }
        }

        // 群系：取区块中心列的高度作为采样层
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

    progress("渲染区块", total, total);

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
        reused,
        sampled,
    })
}

// ---------------------------------------------------------------------------
// PNG 编码
// ---------------------------------------------------------------------------

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_chunk(&mut out, b"IHDR", &ihdr);

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

        let mut biome_json = String::from("{");
        for (k, (c, v)) in img.biomes.iter().enumerate() {
            if k > 0 {
                biome_json.push(',');
            }
            biome_json.push_str(&format!(
                "\"{},{}\":\"{}\"",
                c.0,
                c.1,
                html_escape(v.strip_prefix("minecraft:").unwrap_or(v))
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
    <span>每区块 {tile} px</span>
    <span class="muted">缓存复用 {reused} / 读盘 {sampled}</span>
  </div>
  <div class="stage">
    <div class="canvas" style="width:{w}px;height:{h}px">
      <img id="img{i}" src="data:image/png;base64,{b64}" width="{w}" height="{h}"
           data-bx="{x0}" data-bz="{z0}" data-tile="{tile}"
           onmousemove="hover(event,{i})" onmouseleave="clearHud()" draggable="false">
      <div class="grid" id="grid{i}" style="--g:{tile}px"></div>
    </div>
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
            reused = img.reused,
            sampled = img.sampled,
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
  html, body {{ height: 100%; }}
  body {{
    margin: 0; background: var(--bg); color: var(--text);
    font: 14px/1.5 system-ui, "Microsoft YaHei", sans-serif;
    display: flex; flex-direction: column;
  }}
  header {{
    flex: 0 0 auto;
    display: flex; align-items: center; gap: 14px; flex-wrap: wrap;
    padding: 9px 16px; background: var(--panel);
    border-bottom: 1px solid var(--line);
  }}
  h1 {{ font-size: 15px; margin: 0; font-weight: 600; }}
  .hint {{ color: var(--dim); font-size: 12px; }}
  .tabs {{ display: flex; gap: 6px; margin-left: auto; }}
  .tab {{
    background: #2a2e35; color: var(--text); border: 1px solid var(--line);
    padding: 5px 14px; border-radius: 6px; cursor: pointer; font-size: 13px;
  }}
  .tab.active {{ background: var(--accent); color: #14170f; border-color: var(--accent); }}
  .pane {{ display: none; flex: 1 1 auto; min-height: 0; flex-direction: column; }}
  .pane.active {{ display: flex; }}
  .hud {{
    flex: 0 0 auto;
    padding: 6px 16px; color: var(--dim); font-size: 12.5px;
    display: flex; gap: 16px; flex-wrap: wrap;
  }}
  .hud .dim {{ color: var(--accent); font-weight: 600; }}
  .hud .muted {{ opacity: .75; }}

  .stage {{
    flex: 1 1 auto; min-height: 0;
    overflow: auto; padding: 0 16px 16px;
    cursor: grab;
    user-select: none;
  }}
  .stage.dragging {{ cursor: grabbing; }}
  .canvas {{ position: relative; }}
  .canvas img {{
    display: block; image-rendering: pixelated;
    background: #101216; border: 1px solid var(--line);
    cursor: crosshair;
    -webkit-user-drag: none;
  }}

  /* 区块网格：纯 CSS 重复渐变，间距由 --g 控制（每区块一条线） */
  .grid {{
    position: absolute; inset: 1px; pointer-events: none;
    display: none;
    background-image:
      repeating-linear-gradient(to right, rgba(255,255,255,.20) 0 1px, transparent 1px var(--g)),
      repeating-linear-gradient(to bottom, rgba(255,255,255,.20) 0 1px, transparent 1px var(--g));
  }}
  .grid.on {{ display: block; }}

  #cursor {{
    position: fixed; pointer-events: none; z-index: 100;
    background: rgba(20,22,26,.95); border: 1px solid var(--accent);
    border-radius: 6px; padding: 6px 10px; font-size: 13px;
    white-space: nowrap; display: none;
  }}
  #cursor b {{ color: var(--accent); }}
</style>
</head>
<body>
<header>
  <h1>{title}</h1>
  <span class="hint">右键拖动画布 · Ctrl+滚轮缩放</span>
  <label class="hint" style="cursor:pointer">
    <input type="checkbox" id="gridToggle" onchange="toggleGrid(this.checked)"> 区块网格
  </label>
  <div class="tabs">{tabs}</div>
</header>
<div id="cursor"></div>
{bodies}
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
function toggleGrid(on) {{
  document.querySelectorAll('.grid').forEach(function (g) {{
    g.classList.toggle('on', on);
  }});
}}
function hover(e, i) {{
  if (dragState.active) return;
  var img = e.currentTarget;
  var rect = img.getBoundingClientRect();
  var px = (e.clientX - rect.left) * (img.naturalWidth / rect.width);
  var py = (e.clientY - rect.top) * (img.naturalHeight / rect.height);

  var tile = +img.dataset.tile;
  var bx = +img.dataset.bx;
  var bz = +img.dataset.bz;

  var cx = bx + Math.floor(px / tile);
  var cz = bz + Math.floor(py / tile);
  var step = 16 / tile;
  var wx = cx * 16 + Math.floor((px % tile) * step);
  var wz = cz * 16 + Math.floor((py % tile) * step);

  var box = document.getElementById('cursor');
  var b = BIOMES[i] && BIOMES[i][cx + ',' + cz];
  box.innerHTML = '<b>X ' + wx + ' Z ' + wz + '</b><br>区块 ' + cx + ', ' + cz +
                  '<br>群系 ' + (b || '未知');
  box.style.display = 'block';
  box.style.left = Math.min(e.clientX + 16, window.innerWidth - 190) + 'px';
  box.style.top = (e.clientY + 16) + 'px';
}}
function clearHud() {{
  document.getElementById('cursor').style.display = 'none';
}}

// ---- 右键拖动画布 ----
// 左键留给"看坐标"，右键才是平移（与程序内手感一致）。
// 拖动期间屏蔽悬停提示，避免它跟着乱跳。
var dragState = {{ active: false, x: 0, y: 0, sl: 0, st: 0, stage: null }};

document.querySelectorAll('.stage').forEach(function (stage) {{
  stage.addEventListener('contextmenu', function (e) {{ e.preventDefault(); }});

  stage.addEventListener('mousedown', function (e) {{
    if (e.button !== 2) return;
    e.preventDefault();
    dragState.active = true;
    dragState.x = e.clientX;
    dragState.y = e.clientY;
    dragState.sl = stage.scrollLeft;
    dragState.st = stage.scrollTop;
    dragState.stage = stage;
    stage.classList.add('dragging');
    clearHud();
  }});
}});

window.addEventListener('mousemove', function (e) {{
  if (!dragState.active || !dragState.stage) return;
  dragState.stage.scrollLeft = dragState.sl - (e.clientX - dragState.x);
  dragState.stage.scrollTop = dragState.st - (e.clientY - dragState.y);
}});

window.addEventListener('mouseup', function (e) {{
  if (e.button !== 2) return;
  dragState.active = false;
  if (dragState.stage) dragState.stage.classList.remove('dragging');
  dragState.stage = null;
}});
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

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 从磁盘加载某个维度的缩略图缓存，并剔除"region 已变化"的区块。
///
/// 返回可复用的区块表；缓存不存在 / 格式不符 / 变化太大时返回空表
/// （调用方会全部走读盘，行为与没有缓存时一致）。
pub fn load_usable_cache(
    save_root: &Path,
    dim: &Dimension,
    slice: &str,
    data_version: i32,
) -> CachedChunks {
    let dir = crate::cache::save_cache_dir(save_root).join(sanitize(&dim.id));

    let Some((meta, tiles, biomes)) = crate::cache::load(&dir) else {
        return CachedChunks::new();
    };
    if meta.slice != slice || meta.data_version != data_version {
        return CachedChunks::new();
    }

    let region_dir = dim.dir(RegionKind::Terrain);
    let now = crate::cache::collect_stamps(&region_dir);
    let changed = meta.changed_regions(&now);

    let mut out = CachedChunks::new();
    for (i, (cx, cz)) in meta.chunks.iter().enumerate() {
        let rx = cx.div_euclid(32);
        let rz = cz.div_euclid(32);
        if changed.iter().any(|(a, b)| *a == rx && *b == rz) {
            continue; // 这个 region 变过，重新采样
        }
        let Some(px) = tiles.get(i) else { continue };
        let b = biomes.get(i).cloned().flatten();
        out.insert((*cx, *cz), (px.clone(), b));
    }
    out
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 一次性导出：`dims` 为空表示导出全部维度。
///
/// `progress` 的第一个参数是给用户看的阶段描述。
pub fn export_html(
    world_root: &Path,
    dims: &[String],
    out_path: &Path,
    mode: SliceMode,
    progress: &mut dyn FnMut(&str, usize, usize),
) -> Result<String, String> {
    let world = World::open(world_root).map_err(|e| e.to_string())?;
    let world_name = world.level.level_name();
    let data_version = world.level.data_version();
    let slice = match mode {
        SliceMode::Surface => "surface".to_string(),
        SliceMode::Fixed(y) => format!("fixed:{y}"),
    };

    let selected: Vec<Dimension> = world
        .dimensions
        .iter()
        .filter(|d| dims.is_empty() || dims.iter().any(|s| s == &d.id))
        .filter(|d| d.exists(RegionKind::Terrain))
        .cloned()
        .collect();

    if selected.is_empty() {
        return Err("没有可导出的维度".into());
    }

    let mut images = Vec::new();
    let mut total_reused = 0usize;
    let mut total_sampled = 0usize;

    for dim in &selected {
        progress(&format!("读取缓存 · {}", dim.display), 0, 1);
        let cached = load_usable_cache(world_root, dim, &slice, data_version);

        let mut cb = |phase: &str, done: usize, total: usize| {
            progress(&format!("{} {}", dim.display, phase), done, total);
        };
        let img = render_dimension(&world, dim, mode, &cached, &mut cb)?;
        total_reused += img.reused;
        total_sampled += img.sampled;
        images.push(img);
    }

    progress("生成网页", 0, 1);
    let html = build_html(&images, &world_name);
    std::fs::write(out_path, html.as_bytes()).map_err(|e| e.to_string())?;

    let total_chunks: usize = images.iter().map(|i| i.chunk_count).sum();
    let mut msg = format!(
        "已导出 {} 个维度、{} 个区块 → {}\n缓存复用 {} 个，读盘 {} 个",
        images.len(),
        total_chunks,
        out_path.display(),
        total_reused,
        total_sampled
    );
    if total_reused == 0 && total_chunks > 2000 {
        msg.push_str(
            "\n提示：这个维度还没有缩略图缓存，全部重新读盘了。\n\
             在程序里点「保存缓存」后再导出会快得多。",
        );
    }
    Ok(msg)
}
