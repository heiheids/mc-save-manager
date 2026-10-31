//! 「已加载片区」识别。
//!
//! 存档里已加载的区块往往散落在极远的位置（长途跑图、跨区试验），
//! 直接看区块列表很难知道哪一片在哪儿。这里把**互相连通**的区块
//! 归成一片，每片给出中心坐标，方便跳转。
//!
//! 连通性按 4 邻域判断（上下左右），这也是游戏里区块"连成一片"的直觉含义。

use std::collections::{HashMap, HashSet, VecDeque};

/// 一个连通的已加载片区。
#[derive(Clone, Debug)]
pub struct Region {
    /// 片区内的区块数。
    pub chunk_count: usize,
    /// 中心区块坐标（取包围盒中心，保证落在片区内或附近）。
    pub center_chunk: (i32, i32),
    /// 区块包围盒 (min_x, min_z, max_x, max_z)。
    pub bounds: (i32, i32, i32, i32),
    /// 仅含 full 状态区块的数量，用于判断这片地形是否已经生成完毕。
    pub full_count: usize,
    /// 片区内实体总数（若调用方提供）。
    pub entity_count: usize,
}

impl Region {
    /// 中心对应的方块坐标（区块中心格）。
    pub fn center_block(&self) -> (i32, i32) {
        (
            self.center_chunk.0 * 16 + 8,
            self.center_chunk.1 * 16 + 8,
        )
    }

    /// 跨度的区块数。
    pub fn span(&self) -> (i32, i32) {
        (
            self.bounds.2 - self.bounds.0 + 1,
            self.bounds.3 - self.bounds.1 + 1,
        )
    }
}

/// 把一组区块坐标按 4 邻域连通性分簇。
///
/// - `chunks`：所有已加载区块坐标
/// - `is_full`：判断某区块是否已完全生成（用于统计）
/// - `entity_of`：某区块的实体数（可选，用于展示）
///
/// 返回的片区按区块数从多到少排序。
pub fn cluster_chunks(
    chunks: &[(i32, i32)],
    is_full: impl Fn(i32, i32) -> bool,
    entity_of: impl Fn(i32, i32) -> usize,
) -> Vec<Region> {
    let present: HashSet<(i32, i32)> = chunks.iter().copied().collect();
    let mut visited: HashSet<(i32, i32)> = HashSet::with_capacity(present.len());
    let mut regions: Vec<Region> = Vec::new();

    for &start in &present {
        if visited.contains(&start) {
            continue;
        }
        // 洪水填充
        let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);

        let mut count = 0usize;
        let mut full = 0usize;
        let mut ents = 0usize;
        let mut min_x = i32::MAX;
        let mut min_z = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_z = i32::MIN;

        while let Some((x, z)) = queue.pop_front() {
            count += 1;
            if is_full(x, z) {
                full += 1;
            }
            ents += entity_of(x, z);
            min_x = min_x.min(x);
            min_z = min_z.min(z);
            max_x = max_x.max(x);
            max_z = max_z.max(z);

            for (nx, nz) in [(x + 1, z), (x - 1, z), (x, z + 1), (x, z - 1)] {
                if present.contains(&(nx, nz)) && visited.insert((nx, nz)) {
                    queue.push_back((nx, nz));
                }
            }
        }

        regions.push(Region {
            chunk_count: count,
            center_chunk: ((min_x + max_x) / 2, (min_z + max_z) / 2),
            bounds: (min_x, min_z, max_x, max_z),
            full_count: full,
            entity_count: ents,
        });
    }

    regions.sort_by(|a, b| b.chunk_count.cmp(&a.chunk_count));
    regions
}

/// 便捷版本：直接用区块元数据分簇。
pub fn cluster_from_meta(
    meta: &[crate::world::ChunkMeta],
    entity_counts: &HashMap<(i32, i32), usize>,
) -> Vec<Region> {
    let coords: Vec<(i32, i32)> = meta.iter().map(|c| (c.x, c.z)).collect();
    let full_set: HashSet<(i32, i32)> = meta
        .iter()
        .filter(|c| c.is_full())
        .map(|c| (c.x, c.z))
        .collect();

    cluster_chunks(
        &coords,
        |x, z| full_set.contains(&(x, z)),
        |x, z| entity_counts.get(&(x, z)).copied().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_full(_x: i32, _z: i32) -> bool {
        false
    }
    fn no_ent(_x: i32, _z: i32) -> usize {
        0
    }

    #[test]
    fn single_block_is_one_region() {
        let r = cluster_chunks(&[(0, 0)], no_full, no_ent);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].chunk_count, 1);
        assert_eq!(r[0].center_chunk, (0, 0));
    }

    #[test]
    fn adjacent_chunks_merge() {
        let r = cluster_chunks(&[(0, 0), (1, 0), (0, 1)], no_full, no_ent);
        assert_eq!(r.len(), 1, "相邻区块应合成一片");
        assert_eq!(r[0].chunk_count, 3);
        assert_eq!(r[0].bounds, (0, 0, 1, 1));
    }

    #[test]
    fn diagonal_is_not_connected() {
        // 仅对角相邻在 4 邻域下是分开的
        let r = cluster_chunks(&[(0, 0), (1, 1)], no_full, no_ent);
        assert_eq!(r.len(), 2, "对角不算连通");
    }

    #[test]
    fn far_apart_clusters_are_separate() {
        // 模拟你的存档：主城区 + 50km 外的试验场
        let mut chunks = Vec::new();
        for x in 0..3 {
            for z in 0..3 {
                chunks.push((x, z));
            }
        }
        for x in 39..41 {
            for z in 3120..3122 {
                chunks.push((x, z));
            }
        }
        let r = cluster_chunks(&chunks, no_full, no_ent);
        assert_eq!(r.len(), 2, "两块不相连的区域应分成两片");
        // 按大小排序：3x3=9 在前
        assert_eq!(r[0].chunk_count, 9);
        assert_eq!(r[1].chunk_count, 4);
        // 第二片中心应该在远处
        assert!(r[1].center_chunk.1 > 3000);
    }

    #[test]
    fn negative_coords_work() {
        let r = cluster_chunks(&[(-1, -1), (-2, -1), (-1, -2)], no_full, no_ent);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].bounds, (-2, -2, -1, -1));
        // 包围盒中心：(-2 + -1) / 2 向零截断 = -1
        assert_eq!(r[0].center_chunk, (-1, -1));
        // 中心必须落在包围盒内
        let (cx, cz) = r[0].center_chunk;
        assert!((cx, cz) == (-1, -1) || (cx, cz) == (-2, -2));
    }

    #[test]
    fn counts_full_and_entities() {
        let chunks = [(0, 0), (1, 0)];
        let r = cluster_chunks(
            &chunks,
            |x, _z| x == 0,      // 只有 (0,0) 是 full
            |_x, z| if z == 0 { 5 } else { 0 },
        );
        assert_eq!(r[0].full_count, 1);
        assert_eq!(r[0].entity_count, 10);
    }

    #[test]
    fn center_block_is_inside_bounds() {
        let r = cluster_chunks(&[(0, 0), (1, 0), (2, 0)], no_full, no_ent);
        let (bx, bz) = r[0].center_block();
        let (cx, cz) = r[0].center_chunk;
        assert_eq!(bx, cx * 16 + 8);
        assert_eq!(bz, cz * 16 + 8);
        // 中心区块必须落在包围盒内
        assert!(cx >= 0 && cx <= 2);
    }

    #[test]
    fn empty_input_yields_no_regions() {
        let r = cluster_chunks(&[], no_full, no_ent);
        assert!(r.is_empty());
    }

    #[test]
    fn large_line_is_one_region() {
        // 模拟长条形的加载带
        let chunks: Vec<(i32, i32)> = (0..100).map(|x| (x, 0)).collect();
        let r = cluster_chunks(&chunks, no_full, no_ent);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].chunk_count, 100);
        assert_eq!(r[0].span(), (100, 1));
    }
}
