// 检查区块的分布：是不是存在少量"离群"区块把范围撑大了。
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

    for dim in &world.dimensions {
        if !dim.exists(RegionKind::Terrain) {
            continue;
        }
        let all = world.scan_chunks(dim, &mut |_, _| {}).unwrap_or_default();
        if all.is_empty() {
            continue;
        }

        println!("维度: {}  ({} 区块)", dim.display, all.len());

        // ---- X 直方图（每 64 区块一桶） ----
        let mut hist: std::collections::BTreeMap<i32, usize> = Default::default();
        for c in &all {
            *hist.entry(c.x.div_euclid(64) * 64).or_default() += 1;
        }
        let mut buckets: Vec<_> = hist.into_iter().collect();
        buckets.sort_by_key(|(k, _)| *k);

        let total: usize = buckets.iter().map(|(_, n)| n).sum();
        println!("  X 分布（每 64 区块一桶，共 {} 桶）:", buckets.len());
        let mut cum = 0usize;
        for (start, n) in &buckets {
            cum += n;
            let pct = *n as f64 / total as f64 * 100.0;
            let bar_len = (pct * 1.5) as usize;
            let bar = "#".repeat(bar_len.min(50));
            println!(
                "    X {start:>6}..{:>6}  {n:>6} ({pct:>5.1}%) 累计 {:>5.1}%  {bar}",
                start + 63,
                cum as f64 / total as f64 * 100.0
            );
        }

        // ---- 分位数：看主区域范围 ----
        let mut xs: Vec<i32> = all.iter().map(|c| c.x).collect();
        xs.sort_unstable();
        let pick = |p: f64| xs[((xs.len() as f64 - 1.0) * p) as usize];
        println!(
            "  X 分位: 0%={} 1%={} 5%={} 50%={} 95%={} 99%={} 100%={}",
            xs[0],
            pick(0.01),
            pick(0.05),
            pick(0.5),
            pick(0.95),
            pick(0.99),
            xs[xs.len() - 1]
        );

        let mut zs: Vec<i32> = all.iter().map(|c| c.z).collect();
        zs.sort_unstable();
        let pickz = |p: f64| zs[((zs.len() as f64 - 1.0) * p) as usize];
        println!(
            "  Z 分位: 0%={} 1%={} 5%={} 50%={} 95%={} 99%={} 100%={}",
            zs[0],
            pickz(0.01),
            pickz(0.05),
            pickz(0.5),
            pickz(0.95),
            pickz(0.99),
            zs[zs.len() - 1]
        );
        println!();
    }
}
