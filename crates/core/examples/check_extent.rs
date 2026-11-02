// 检查大存档的真实区块范围，判断「全图」应有的缩放级别。
use mcworld_core::world::{RegionKind, World};

fn main() {
    let Some(path) = std::env::var("MCWORLD_TEST_SAVE").ok() else {
        println!("跳过：未设置 MCWORLD_TEST_SAVE");
        return;
    };
    let world = match World::open(std::path::Path::new(&path)) {
        Ok(w) => w,
        Err(e) => {
            println!("打开失败: {e}");
            return;
        }
    };

    for dim in &world.dimensions {
        if !dim.exists(RegionKind::Terrain) {
            continue;
        }
        let all = world.scan_chunks(dim, &mut |_, _| {}).unwrap_or_default();
        if all.is_empty() {
            continue;
        }
        let (mut x0, mut x1, mut z0, mut z1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for c in &all {
            x0 = x0.min(c.x);
            x1 = x1.max(c.x);
            z0 = z0.min(c.z);
            z1 = z1.max(c.z);
        }
        let span_cx = x1 - x0 + 1;
        let span_cz = z1 - z0 + 1;
        let span_max = span_cx.max(span_cz);
        let span_blocks = span_max as f32 * 16.0;

        // fit_all 的算法
        let scale = (1500.0 / span_blocks).clamp(0.002, 4.0);

        println!("维度: {}", dim.display);
        println!("  区块 {} 个", all.len());
        println!("  范围 X {x0}..{x1} ({span_cx} 个), Z {z0}..{z1} ({span_cz} 个)");
        println!("  最大跨度 {span_max} 区块 = {span_blocks:.0} 方块");
        println!("  全图应缩放 = {scale:.4}");
        // 每个区块在屏幕上占多少像素
        println!("  每区块 {:.2} 像素", 16.0 * scale);
        println!();
    }
}
