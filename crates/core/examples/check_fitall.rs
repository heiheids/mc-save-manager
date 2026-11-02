// 复现 fit_all 的算法，检查大存档上得到的相机参数是否合理。
use mcworld_core::world::{RegionKind, World};

fn main() {
    let Some(path) = std::env::var("MCWORLD_TEST_SAVE").ok() else {
        println!("跳过");
        return;
    };
    let world = match World::open(std::path::Path::new(&path)) {
        Ok(w) => w,
        Err(e) => {
            println!("打开失败: {e}");
            return;
        }
    };

    // 模拟默认窗口下的地图区尺寸
    let (canvas_w, canvas_h) = (1200.0_f32, 800.0_f32);

    for dim in &world.dimensions {
        if !dim.exists(RegionKind::Terrain) {
            continue;
        }
        let all = world.scan_chunks(dim, &mut |_, _| {}).unwrap_or_default();
        if all.is_empty() {
            continue;
        }

        // 与 fit_all 完全一致的算法
        let (mut min_x, mut max_x, mut min_z, mut max_z) =
            (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for c in &all {
            min_x = min_x.min(c.x);
            max_x = max_x.max(c.x);
            min_z = min_z.min(c.z);
            max_z = max_z.max(c.z);
        }
        let cx = (min_x + max_x) as f32 * 8.0 + 8.0;
        let cz = (min_z + max_z) as f32 * 8.0 + 8.0;

        let span_x = ((max_x - min_x + 1) as f32 * 16.0).max(32.0);
        let span_z = ((max_z - min_z + 1) as f32 * 16.0).max(32.0);
        let scale_x = canvas_w / span_x;
        let scale_z = canvas_h / span_z;
        let scale = scale_x.min(scale_z).clamp(0.002, 4.0);

        println!("维度: {}", dim.display);
        println!("  区块范围 X {min_x}..{max_x}  Z {min_z}..{max_z}");
        println!("  相机中心 ({cx:.0}, {cz:.0}) 方块 = 区块 ({:.0}, {:.0})", cx / 16.0, cz / 16.0);
        println!("  span ({span_x:.0}, {span_z:.0}) 方块");
        println!("  scale_x={scale_x:.4} scale_z={scale_z:.4} → 取 {scale:.4}");

        // 校验：内容在画布上占多大
        let draw_w = span_x * scale;
        let draw_h = span_z * scale;
        println!("  内容占画布 {draw_w:.0}×{draw_h:.0} 像素 (画布 {canvas_w:.0}×{canvas_h:.0})");
        if draw_w > canvas_w + 1.0 || draw_h > canvas_h + 1.0 {
            println!("  ❌ 内容超出画布！");
        } else {
            println!("  ✅ 装得下");
        }

        // 可见范围
        let half_w = canvas_w / 2.0 / scale;
        let half_h = canvas_h / 2.0 / scale;
        println!(
            "  可见范围 X {:.0}..{:.0}  Z {:.0}..{:.0}",
            cx - half_w, cx + half_w, cz - half_h, cz + half_h
        );
        let world_min_x = min_x as f32 * 16.0;
        let world_max_x = (max_x as f32 + 1.0) * 16.0;
        let world_min_z = min_z as f32 * 16.0;
        let world_max_z = (max_z as f32 + 1.0) * 16.0;
        println!("  存档实际 X {world_min_x:.0}..{world_max_x:.0}  Z {world_min_z:.0}..{world_max_z:.0}");
        if cx - half_w > world_min_x + 1.0 || cx + half_w < world_max_x - 1.0 {
            println!("  ❌ 存档有部分落在可见范围之外");
        } else {
            println!("  ✅ 全部可见");
        }
        println!();
    }
}
