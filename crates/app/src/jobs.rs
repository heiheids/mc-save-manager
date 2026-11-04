//! 后台任务：把耗时的扫描/写入放到工作线程，避免界面卡死。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use mcworld_core::edit::{self, ChunkClipboard, CleanupPreset, DeleteStats, EntityFilter};
use mcworld_core::safety::{self, Session};
use mcworld_core::world::{ChunkMeta, Dimension, EntityMeta, RegionKind, World};

/// 任务进度。
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub message: String,
}

/// 后台任务的返回值。
pub enum JobResult {
    /// HTML 全图导出完成
    HtmlExported {
        message: String,
    },
    /// HTML 全图导出失败
    HtmlExportFailed {
        error: String,
    },
    /// 缩略图缓存保存完成
    CacheSaved {
        message: String,
    },
    /// 缩略图缓存保存失败
    CacheSaveFailed {
        error: String,
    },
    /// 缓存已读回：可直接复用这些缩略图
    CacheLoaded {
        /// 发起这次任务的标签页，用来把结果送回**原来的存档**。
        tab_id: usize,
        dimension: String,
        /// 可以复用的区块 → 16×16 RGBA
        tiles: Vec<((i32, i32), Vec<u8>)>,
        /// 因 region 变化而需要重新采样的区块
        stale: Vec<(i32, i32)>,
        /// 是否真的命中了一份可用缓存。
        ///
        /// 没命中时也必须回一条结果（hit = false），否则任务永远停在
        /// "未完成"状态，「处理中」浮窗会一直挂在屏幕上。
        hit: bool,
    },
    /// 扫描完成：区块元数据 + 实体 + 方块实体
    Scanned {
        tab_id: usize,
        dimension: String,
        chunks: Vec<ChunkMeta>,
        entities: Vec<EntityMeta>,
        block_entity_count: usize,
        block_entity_types: Vec<(String, usize)>,
    },
    /// 局部重扫完成（删除 / 粘贴后的范围刷新）
    ScannedRange {
        tab_id: usize,
        dimension: String,
        chunks: Vec<ChunkMeta>,
    },
    /// 地图采样完成一批
    Sampled {
        tab_id: usize,
        dimension: String,
        surfaces: Vec<mcworld_core::ChunkSurface>,
        /// 本批里"查不到数据"的区块坐标。
        ///
        /// 必须上报，否则它们会永远留在 pending 里，
        /// 导致"加载中"提示一直不消失。
        missing: Vec<(i32, i32)>,
        complete: bool,
    },
    /// 删除实体完成
    EntitiesDeleted { stats: DeleteStats, dimension: String },
    /// 删除区块完成
    ChunksDeleted {
        count: usize,
        message: String,
        /// 本次操作的备份目录（用于撤销）。
        backup_dir: std::path::PathBuf,
        /// 本次删除覆盖的区块范围（多个选区的并集，已归一化）。
        /// 用于只刷新这一块 + 周边，而不是重扫整个维度。
        bounds: (i32, i32, i32, i32),
    },
    /// 复制完成
    Copied(ChunkClipboard),
    /// 粘贴完成
    Pasted { message: String },
    /// 导出 .smclip 完成
    SmclipExported { path: std::path::PathBuf, bytes: u64, chunks: usize },
    /// 导入 .smclip 完成
    SmclipImported { clipboard: ChunkClipboard, message: String },
    /// 出错
    Failed {
        /// 发起任务的是哪个标签页（导出/复制这类与单个存档视图无关的为 None）。
        tab_id: Option<usize>,
        error: String,
    },
    /// 简单提示
    Info(String),
}

/// 一个后台任务的句柄。
pub struct Job {
    pub id: usize,
    pub rx: Receiver<JobResult>,
    pub progress: Progress,
    pub label: String,
    pub finished: bool,
}

/// 任务管理器（单线程串行执行，避免同时写同一个存档）。
pub struct Jobs {
    jobs: Vec<Job>,
    tx_progress: Sender<(usize, Progress)>,
    rx_progress: Receiver<(usize, Progress)>,
    next_id: usize,
}

impl Default for Jobs {
    fn default() -> Self {
        let (tx_progress, rx_progress) = channel();
        Jobs { jobs: Vec::new(), tx_progress, rx_progress, next_id: 0 }
    }
}

impl Jobs {
    pub fn is_busy(&self) -> bool {
        self.jobs.iter().any(|j| !j.finished)
    }

    pub fn labels(&self) -> Vec<(String, Progress)> {
        self.jobs
            .iter()
            .filter(|j| !j.finished)
            .map(|j| (j.label.clone(), j.progress.clone()))
            .collect()
    }

    /// 收取已完成任务的结果。
    pub fn poll(&mut self) -> Vec<JobResult> {
        while let Ok((id, p)) = self.rx_progress.try_recv() {
            for job in self.jobs.iter_mut() {
                if job.id == id {
                    job.progress = p.clone();
                    break;
                }
            }
        }

        let mut out = Vec::new();
        for job in self.jobs.iter_mut() {
            // ★ 判断任务结束的条件是**发送端断开**，不是"收到过消息"。
            //
            //   采样任务（spawn_sample）一条任务会分批发很多条 `Sampled`
            //   消息（每 128 个有数据的区块一条、每 256 个空区块一条）。
            //   如果收到第一条就把任务标记完成并移出列表，后面那些批次
            //   就再也送不到了 —— 它们对应的区块会永远留在 `pending` 里，
            //   界面一直显示"加载中… 剩余 N 个区块"却再也不动。
            //
            //   反过来，如果像更早的版本那样只在 `Failed` 时置 finished，
            //   成功完成的任务又永远清不掉，`is_busy()` 恒真、「处理中」
            //   浮窗永久挂着。两种都得避开，所以只能靠通道状态判断。
            loop {
                match job.rx.try_recv() {
                    Ok(msg) => out.push(msg),
                    // 暂时没有消息，但发送端还活着 —— 任务没结束，先留着
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    // 发送端已释放：该发的都发完了，任务真正结束
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        job.finished = true;
                        break;
                    }
                }
            }
        }
        // 结果已经全部收走，结束的任务移出列表
        self.jobs.retain(|j| !j.finished);
        out
    }

    /// 启动扫描任务。
    pub fn spawn_scan(&mut self, tab_id: usize, world_root: PathBuf, dim: Dimension) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();
        let label = format!("扫描 {}", dim.display);

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);

                let mut last = 0usize;
                let chunks = world
                    .scan_chunks(&dim, &mut |done, total| {
                        if done == total || done - last >= 16 {
                            last = done;
                            let _ = ptx.send((
                                id,
                                Progress {
                                    done,
                                    total,
                                    message: format!("扫描区块 {done}/{total}"),
                                },
                            ));
                        }
                    })
                    .map_err(|e| e.to_string())?;

                let _ = ptx.send((
                    id,
                    Progress {
                        done: 0,
                        total: 1,
                        message: "扫描实体…".into(),
                    },
                ));

                let entities = if dim.exists(RegionKind::Entities) {
                    world.scan_entities(&dim).map_err(|e| e.to_string())?
                } else {
                    Vec::new()
                };

                let _ = ptx.send((
                    id,
                    Progress {
                        done: 0,
                        total: 1,
                        message: "扫描方块实体…".into(),
                    },
                ));

                let bes = world.scan_block_entities(&dim).map_err(|e| e.to_string())?;
                let mut map: std::collections::HashMap<String, usize> = Default::default();
                for b in &bes {
                    *map.entry(b.id.clone()).or_default() += 1;
                }
                let mut types: Vec<_> = map.into_iter().collect();
                types.sort_by(|a, b| b.1.cmp(&a.1));

                let _ = tx.send(JobResult::Scanned {
                        tab_id,
                    dimension: dim.id.clone(),
                    chunks,
                    entities,
                    block_entity_count: bes.len(),
                    block_entity_types: types,
                });
                Ok(())
            })();

            match result {
                Ok(()) => {}
                Err(e) => { let _ = tx.send(JobResult::Failed { tab_id: Some(tab_id), error: e }); }
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 启动地图采样任务（分批返回，界面可以渐进更新）。
    /// 只扫描指定区块范围（闭区间），用于删除后的局部刷新。
    ///
    /// 与 `spawn_scan` 的区别：不遍历整个 region 文件，
    /// 只按坐标逐个读需要的区块。范围通常很小（几十个区块），
    /// 所以比全量扫描快得多。
    pub fn spawn_scan_range(
        &mut self,
        tab_id: usize,
        world_root: PathBuf,
        dim: Dimension,
        x0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let label = format!("刷新 {} 个区块", (x1 - x0 + 1) * (z1 - z0 + 1));

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);

                let base = dim.dir(RegionKind::Terrain);
                let mut fresh: Vec<ChunkMeta> = Vec::new();
                // 缓存已打开的 region，避免同一文件反复开
                let mut open: std::collections::HashMap<(i32, i32), mcworld_core::anvil::RegionFile> =
                    Default::default();

                for cz in z0..=z1 {
                    for cx in x0..=x1 {
                        let (rx, rz) = (
                            mcworld_core::anvil::region_coord(cx),
                            mcworld_core::anvil::region_coord(cz),
                        );
                        let path = base.join(mcworld_core::anvil::region_file_name(rx, rz));
                        if !path.exists() {
                            continue;
                        }
                        if !open.contains_key(&(rx, rz)) {
                            match mcworld_core::anvil::RegionFile::open(&path) {
                                Ok(f) => {
                                    open.insert((rx, rz), f);
                                }
                                Err(_) => continue,
                            }
                        }
                        let Some(rf) = open.get_mut(&(rx, rz)) else { continue };
                        if let Ok(Some(tag)) = rf.chunk(cx, cz) {
                            fresh.push(ChunkMeta::from_tag(cx, cz, &tag));
                        }
                    }
                }

                let _ = tx.send(JobResult::ScannedRange {
                        tab_id,
                    dimension: dim.id.clone(),
                    chunks: fresh,
                });
                Ok(())
            })();

            if let Err(e) = result {
                let _ = tx.send(JobResult::Failed { tab_id: Some(tab_id), error: e });
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    pub fn spawn_sample(
        &mut self,
        tab_id: usize,
        world_root: PathBuf,
        dim: Dimension,
        chunks: Vec<(i32, i32)>,
        mode: mcworld_core::chunk::SliceMode,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();
        let label = format!("采样地图 {}", dim.display);

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);
                let mut cache = mcworld_core::world::RegionCache::new(64);
                let total = chunks.len();
                let mut batch = Vec::with_capacity(256);
                let mut miss_batch: Vec<(i32, i32)> = Vec::new();

                for (i, (cx, cz)) in chunks.into_iter().enumerate() {
                    match world.chunk_surface(&dim, cx, cz, &mut cache, mode) {
                        Some(s) => batch.push(s),
                        // 记录无数据的区块，供前端清理 pending
                        None => miss_batch.push((cx, cz)),
                    }
                    if batch.len() >= 128 || miss_batch.len() >= 256 || i + 1 == total {
                        let complete = i + 1 == total;
                        let _ = tx.send(JobResult::Sampled {
                            tab_id,
                            dimension: dim.id.clone(),
                            surfaces: std::mem::take(&mut batch),
                            missing: std::mem::take(&mut miss_batch),
                            complete,
                        });
                        let _ = ptx.send((
                            id,
                            Progress {
                                done: i + 1,
                                total,
                                message: format!("采样 {}/{}", i + 1, total),
                            },
                        ));
                        if complete {
                            return Ok(());
                        }
                    }
                }
                Ok(())
            })();

            if let Err(e) = result {
                let _ = tx.send(JobResult::Failed { tab_id: Some(tab_id), error: e });
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 导出全图为 HTML（后台任务）。
    ///
    /// 大存档渲染一张图可能要好几分钟，必须放到后台线程，
    /// 否则界面直接假死。进度通过 `ptx` 回报。
    pub fn spawn_export_html(
        &mut self,
        world_root: PathBuf,
        dims: Vec<String>,
        out_path: PathBuf,
        mode: mcworld_core::chunk::SliceMode,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();
        let label = "导出全图 HTML".to_string();

        std::thread::spawn(move || {
            let mut cb = |dim: &str, done: usize, total: usize| {
                let _ = ptx.send((
                    id,
                    Progress {
                        done,
                        total,
                        message: format!("{dim} {done}/{total}"),
                    },
                ));
            };
            match crate::html_export::export_html(&world_root, &dims, &out_path, mode, &mut cb) {
                Ok(msg) => {
                    let _ = tx.send(JobResult::HtmlExported { message: msg });
                }
                Err(e) => {
                    let _ = tx.send(JobResult::HtmlExportFailed { error: e });
                }
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 保存某个维度的缩略图缓存（后台）。
    ///
    /// 采集该维度所有 region 的指纹，连同当前已渲染的缩略图一起落盘。
    pub fn spawn_save_cache(
        &mut self,
        save_root: PathBuf,
        dim_id: String,
        dim_display: String,
        region_dir: PathBuf,
        slice: String,
        data_version: i32,
        tiles: Vec<((i32, i32), Vec<u8>, Option<String>)>,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let label = format!("保存缓存 {dim_display}");

        std::thread::spawn(move || {
            let dir = crate::cache::save_cache_dir(&save_root).join(sanitize(&dim_id));
            let stamps = crate::cache::collect_stamps(&region_dir);
            let mut meta = crate::cache::CacheMeta {
                format_version: crate::cache::FORMAT_VERSION,
                dim_id,
                slice,
                data_version,
                regions: stamps,
                chunks: Vec::new(),
            };
            let mut data = Vec::with_capacity(tiles.len());
            let mut biomes = Vec::with_capacity(tiles.len());
            for (key, px, bi) in tiles {
                meta.chunks.push(key);
                data.push(px);
                biomes.push(bi);
            }
            match crate::cache::save(&dir, &meta, &data, &biomes) {
                Ok(()) => {
                    let _ = tx.send(JobResult::CacheSaved {
                        message: format!(
                            "缓存已保存：{} 个区块 → {}",
                            meta.chunks.len(),
                            dir.display()
                        ),
                    });
                }
                Err(e) => {
                    let _ = tx.send(JobResult::CacheSaveFailed { error: e });
                }
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 尝试读取某个维度的缩略图缓存（后台）。
    ///
    /// 会把缓存里的 region 指纹与磁盘当前状态对比，把"没变化的区块"
    /// 和"需要重算的区块"分开返回。
    pub fn spawn_load_cache(
        &mut self,
        tab_id: usize,
        save_root: PathBuf,
        dim_id: String,
        region_dir: PathBuf,
        slice: String,
        data_version: i32,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let label = format!("读取缓存 {dim_id}");

        std::thread::spawn(move || {
            // 没有缓存时**也必须回一条结果**。
            //
            // 之前这里直接 `return`，tx 被丢弃，接收端永远收不到东西，
            // 任务就一直是"未完成"：is_busy() 恒为真、界面恒定重绘，
            // 而「处理中」浮窗会永久显示"读取缓存 xxx / 准备中…"。
            // 用户把程序复制到新目录（那里没有 地图缓存/）后一打开存档
            // 就能看到这个卡住的框。
            let dir = crate::cache::save_cache_dir(&save_root).join(sanitize(&dim_id));
            let loaded = crate::cache::load(&dir);

            let Some((meta, tiles, _biomes)) = loaded else {
                let _ = tx.send(JobResult::CacheLoaded {
                    tab_id,
                    dimension: dim_id,
                    tiles: Vec::new(),
                    stale: Vec::new(),
                    hit: false,
                });
                return;
            };

            // 切片方式或数据版本变了 → 整份作废
            if meta.slice != slice || meta.data_version != data_version {
                let _ = tx.send(JobResult::CacheLoaded {
                    tab_id,
                    dimension: dim_id,
                    tiles: Vec::new(),
                    stale: Vec::new(),
                    hit: false,
                });
                return;
            }

            let now = crate::cache::collect_stamps(&region_dir);
            let changed = meta.changed_regions(&now);

            // 标记受影响的区块
            let mut stale: Vec<(i32, i32)> = Vec::new();
            let mut good: Vec<((i32, i32), Vec<u8>)> = Vec::new();
            for (i, key) in meta.chunks.iter().enumerate() {
                let (cx, cz) = *key;
                let rx = cx.div_euclid(32);
                let rz = cz.div_euclid(32);
                if changed.iter().any(|(a, b)| *a == rx && *b == rz) {
                    stale.push((cx, cz));
                } else if let Some(px) = tiles.get(i) {
                    good.push((*key, px.clone()));
                }
            }

            let _ = tx.send(JobResult::CacheLoaded {
                tab_id,
                dimension: dim_id,
                tiles: good,
                stale,
                hit: true,
            });
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 启动实体删除任务。
    pub fn spawn_delete_entities(
        &mut self,
        world_root: PathBuf,
        dim: Dimension,
        filter: EntityFilter,
        label: String,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);

                let backup_dir = safety::new_backup_dir(
                    &world_root.join("..").join("backups"),
                    &world.level.level_name(),
                );
                let mut session = Session::begin(&world, &backup_dir).map_err(|e| e.to_string())?;

                // 备份所有实体 region 文件
                for (_, _, p) in
                    mcworld_core::anvil::list_region_files(&dim.dir(RegionKind::Entities))
                        .map_err(|e| e.to_string())?
                {
                    session.touch(&p).map_err(|e| e.to_string())?;
                }

                let _ = ptx.send((
                    id,
                    Progress { done: 0, total: 1, message: "删除中…".into() },
                ));

                let stats = edit::delete_entities(&dim, &filter).map_err(|e| e.to_string())?;
                let _ = tx.send(JobResult::EntitiesDeleted {
                    stats,
                    dimension: dim.id.clone(),
                });
                Ok::<(), String>(())
            })();

            if let Err(e) = result {
                let _ = tx.send(JobResult::Failed { tab_id: None, error: e });
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 启动区块删除任务。
    pub fn spawn_delete_chunks(
        &mut self,
        world_root: PathBuf,
        dim: Dimension,
        chunks: Vec<(i32, i32)>,
        also_entities: bool,
        also_poi: bool,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();
        let label = format!("删除 {} 个区块", chunks.len());

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);
                let backup_dir = safety::new_backup_dir(
                    &world_root.join("..").join("backups"),
                    &world.level.level_name(),
                );
                let mut session = Session::begin(&world, &backup_dir).map_err(|e| e.to_string())?;

                // 备份可能被改动的 region
                let mut files = std::collections::HashSet::new();
                for (cx, cz) in &chunks {
                    let (rx_, rz_) = (
                        mcworld_core::anvil::region_coord(*cx),
                        mcworld_core::anvil::region_coord(*cz),
                    );
                    for kind in [RegionKind::Terrain, RegionKind::Entities, RegionKind::Poi] {
                        if dim.exists(kind) {
                            files.insert(
                                dim.dir(kind)
                                    .join(mcworld_core::anvil::region_file_name(rx_, rz_)),
                            );
                        }
                    }
                }
                for p in &files {
                    session.touch(p).map_err(|e| e.to_string())?;
                }

                let mut editor = safety::ChunkEditor::new(&dim);
                let total = chunks.len();
                let mut removed = 0usize;
                for (i, (cx, cz)) in chunks.iter().enumerate() {
                    if editor
                        .delete_chunk(*cx, *cz, also_entities, also_poi)
                        .map_err(|e| e.to_string())?
                    {
                        removed += 1;
                    }
                    if i % 32 == 0 || i + 1 == total {
                        let _ = ptx.send((
                            id,
                            Progress {
                                done: i + 1,
                                total,
                                message: format!("删除 {}/{}", i + 1, total),
                            },
                        ));
                    }
                }
                editor.commit(&mut session).map_err(|e| e.to_string())?;
                session.mark_committed();

                // 算出本次覆盖的范围（用于局部刷新）
                let mut bmin_x = i32::MAX;
                let mut bmax_x = i32::MIN;
                let mut bmin_z = i32::MAX;
                let mut bmax_z = i32::MIN;
                for (cx, cz) in &chunks {
                    bmin_x = bmin_x.min(*cx);
                    bmax_x = bmax_x.max(*cx);
                    bmin_z = bmin_z.min(*cz);
                    bmax_z = bmax_z.max(*cz);
                }

                let _ = tx.send(JobResult::ChunksDeleted {
                    count: removed,
                    message: format!(
                        "已删除 {removed} 个区块（共请求 {}），备份在 {}",
                        chunks.len(),
                        backup_dir.display()
                    ),
                    backup_dir: backup_dir.clone(),
                    bounds: (bmin_x, bmin_z, bmax_x, bmax_z),
                });
                Ok(())
            })();

            match result {
                Ok(()) => {}
                Err(e) => {
                    let _ = tx.send(JobResult::Failed { tab_id: None, error: e });
                }
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 启动复制任务。
    pub fn spawn_copy(
        &mut self,
        world_root: PathBuf,
        dim: Dimension,
        x0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
        include_entities: bool,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let label = "复制区块".to_string();

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);
                edit::copy_chunks(&dim, x0, z0, x1, z1, include_entities)
                    .map_err(|e| e.to_string())
                    .map(|clipboard| {
                        let _ = tx.send(JobResult::Copied(clipboard));
                    })
            })();

            match result {
                Ok(()) => {}
                Err(e) => { let _ = tx.send(JobResult::Failed { tab_id: None, error: e }); }
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 把选区导出成 SwiftMap 的 `.smclip` 格式。
    ///
    /// 按用户要求：**本程序导出一律是全部高度**（Y -64..319），
    /// 不跟随哪个高度层。
    pub fn spawn_export_smclip(
        &mut self,
        world_root: PathBuf,
        dim: Dimension,
        x0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
        out_path: PathBuf,
        note: String,
    ) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let ptx = self.tx_progress.clone();
        let label = "导出 .smclip".to_string();

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let world = World::open(&world_root).map_err(|e| e.to_string())?;
                let dim = world
                    .dimensions
                    .iter()
                    .find(|d| d.id == dim.id)
                    .cloned()
                    .unwrap_or(dim);

                let mut block_palette: Vec<String> = Vec::new();
                let mut block_index = std::collections::HashMap::new();
                let mut biome_palette: Vec<String> = Vec::new();
                let mut biome_index = std::collections::HashMap::new();
                let mut chunks = Vec::new();

                let sx = x1 - x0 + 1;
                let sz = z1 - z0 + 1;
                let total = (sx * sz) as usize;
                let mut done = 0usize;

                for cz in z0..=z1 {
                    for cx in x0..=x1 {
                        let rp = dim.dir(mcworld_core::world::RegionKind::Terrain).join(
                            mcworld_core::anvil::region_file_name(
                                cx.div_euclid(32),
                                cz.div_euclid(32),
                            ),
                        );
                        let mut rf = mcworld_core::anvil::RegionFile::open(&rp)
                            .map_err(|e| format!("打开 region 失败: {e}"))?;
                        let tag = match rf.chunk(cx, cz) {
                            Ok(Some(t)) => t,
                            _ => {
                                done += 1;
                                continue;
                            }
                        };
                        let cc = mcworld_core::smclip::chunk_from_nbt(
                            &tag,
                            &mut block_palette,
                            &mut block_index,
                            &mut biome_palette,
                            &mut biome_index,
                            Some((-64, 319)),
                        )
                        .map_err(|e| format!("转换区块 ({cx},{cz}) 失败: {e}"))?;
                        chunks.push(cc);
                        done += 1;
                        if done % 8 == 0 || done == total {
                            let _ = ptx.send((
                                id,
                                Progress {
                                    done,
                                    total,
                                    message: format!("导出 {done}/{total}"),
                                },
                            ));
                        }
                    }
                }

                if chunks.is_empty() {
                    return Err("选区内没有可导出的区块".into());
                }

                let header = mcworld_core::smclip::ClipHeader {
                    format_version: mcworld_core::smclip::FORMAT_VERSION,
                    data_version: world.level.data_version(),
                    mod_version: "存档管理器".into(),
                    created_millis: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                    note,
                    source_world: world.level.level_name(),
                    dimension: dim.id.clone(),
                    origin_chunk_x: x0,
                    origin_chunk_z: z0,
                    size_x: sx,
                    size_z: sz,
                    min_y: -64,
                    max_y: 319,
                    flags: mcworld_core::smclip::FLAG_BIOMES
                        | mcworld_core::smclip::FLAG_ENTITIES
                        | mcworld_core::smclip::FLAG_BLOCK_ENTITY_DATA,
                    block_palette,
                    biome_palette,
                };

                let file = mcworld_core::smclip::ClipFile {
                    header,
                    chunks,
                    raw_lengths: vec![],
                };
                file.save(&out_path).map_err(|e| format!("写出失败: {e}"))?;
                let bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

                let _ = tx.send(JobResult::SmclipExported {
                    path: out_path,
                    bytes,
                    chunks: file.chunks.len(),
                });
                Ok(())
            })();

            if let Err(e) = result {
                let _ = tx.send(JobResult::Failed { tab_id: None, error: e });
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }

    /// 导入 `.smclip` 到内存剪贴板（之后照常走粘贴流程）。
    ///
    /// SwiftMap 的文件可能只保存某个高度层，这里**把它放到对应的高度上**，
    /// 本程序自己贴的时候按全部高度处理（用户明确要求：不考虑修改高度放置）。
    pub fn spawn_import_smclip(&mut self, path: PathBuf) {
        let (tx, rx) = channel();
        let id = self.next_id;
        self.next_id += 1;
        let label = "导入 .smclip".to_string();

        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let file = mcworld_core::smclip::ClipFile::open(&path)
                    .map_err(|e| format!("读取失败: {e}"))?;

                let h = &file.header;
                let n = file.chunks.len();
                if n == 0 {
                    return Err("文件里没有区块".into());
                }
                if n != h.chunk_count() {
                    return Err(format!(
                        "区块数与头部不符: 头部 {} 个，实际 {n} 个",
                        h.chunk_count()
                    ));
                }
                let sx = h.size_x.max(1);
                // 行优先：Z 外 X 内
                let mut chunks = Vec::with_capacity(n);
                for (i, cc) in file.chunks.iter().enumerate() {
                    let dx = (i as i32) % sx;
                    let dz = (i as i32) / sx;
                    // 把 .smclip 的 section 数据还原成该区块的完整 NBT
                    let tag = mcworld_core::smclip::chunk_to_nbt(cc, h, h.origin_chunk_x + dx, h.origin_chunk_z + dz)
                        .map_err(|e| format!("还原区块 {i} 失败: {e}"))?;
                    chunks.push(mcworld_core::edit::CopiedChunk { dx, dz, tag });
                }

                let block_entities = 0usize;
                let entities = 0usize;
                let clipboard = mcworld_core::edit::ChunkClipboard {
                    source_dimension: h.dimension.clone(),
                    chunks,
                    origin: (h.origin_chunk_x, h.origin_chunk_z),
                    block_entities,
                    entities,
                };
                let msg = format!(
                    "已导入 {} 个区块（{}×{}，Y {}..{}，格式 v{}）",
                    n, h.size_x, h.size_z, h.min_y, h.max_y, h.format_version
                );
                let _ = tx.send(JobResult::SmclipImported { clipboard, message: msg });
                Ok(())
            })();

            if let Err(e) = result {
                let _ = tx.send(JobResult::Failed { tab_id: None, error: e });
            }
        });

        self.jobs.push(Job {
            id,
            rx,
            progress: Progress::default(),
            label,
            finished: false,
        });
    }
}

impl Job {
    pub fn id_of(&self) -> usize {
        0
    }
}


/// 把维度 id 变成安全的目录名：minecraft:overworld → minecraft_overworld
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}


#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    /// 等任务结束（最多 3 秒），期间反复 poll。
    fn drain(jobs: &mut Jobs) -> Vec<JobResult> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut got = Vec::new();
        while std::time::Instant::now() < deadline {
            got.extend(jobs.poll());
            if !jobs.is_busy() {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        got
    }

    /// 没有缓存时，任务必须**照样回一条结果**并结束。
    ///
    /// 这正是"把程序复制到新目录后，浮窗卡在读取缓存"那个 bug：
    /// 缓存不存在时 spawn_load_cache 直接 return，一条结果都不发，
    /// 任务永远不结束 —— is_busy() 恒真、浮窗永远挂着。
    #[test]
    fn load_cache_without_cache_still_finishes() {
        let mut jobs = Jobs::default();
        let tmp = std::env::temp_dir().join("mcworld_test_no_cache_dir");
        let _ = std::fs::create_dir_all(&tmp);

        jobs.spawn_load_cache(
            1, // tab_id
            tmp.clone(),
            "minecraft:overworld".to_string(),
            tmp.join("region"),
            "surface".to_string(),
            1234,
        );

        assert!(jobs.is_busy(), "刚提交应当处于忙碌状态");

        let results = drain(&mut jobs);

        assert!(!jobs.is_busy(), "任务结束后不应再忙碌");
        assert!(
            jobs.labels().is_empty(),
            "任务结束后不应还留在 labels() 里，否则浮窗不会消失"
        );
        assert_eq!(results.len(), 1, "应当恰好回一条结果");
        match &results[0] {
            JobResult::CacheLoaded { hit, tiles, .. } => {
                assert!(!hit, "没有缓存时 hit 应为 false");
                assert!(tiles.is_empty());
            }
            _ => panic!("结果类型不对，应当是 CacheLoaded"),
        }
    }

    /// 一条任务发多条消息时，必须把所有消息都收完才清理。
    ///
    /// 这正是"加载卡在 剩余 N 个区块"那个 bug：采样任务会分批发很多条
    /// `Sampled`，早期实现收到第一条就把任务移出列表，后面的批次全部丢失，
    /// 对应的区块永远留在 pending 里。
    #[test]
    fn multi_message_job_is_not_finished_early() {
        let mut jobs = Jobs::default();
        let (tx, rx) = channel();
        jobs.jobs.push(Job {
            id: 999,
            rx,
            progress: Progress::default(),
            label: "fake".into(),
            finished: false,
        });

        // 第一批发完，发送端仍然活着
        for i in 0..3 {
            tx.send(JobResult::Info(format!("d{i}")))
            .unwrap();
        }
        let got = jobs.poll();
        assert_eq!(got.len(), 3, "应当收到 3 条");
        assert!(jobs.is_busy(), "发送端还活着，任务不应算结束");

        // 再来一批
        for i in 0..2 {
            tx.send(JobResult::Info(format!("e{i}")))
            .unwrap();
        }
        let got2 = jobs.poll();
        assert_eq!(got2.len(), 2, "后到的消息也要收到");
        assert!(jobs.is_busy(), "仍然不应结束");

        // 发送端释放后才算结束
        drop(tx);
        let _ = jobs.poll();
        assert!(!jobs.is_busy(), "发送端断开后任务应结束");
        assert!(jobs.jobs.is_empty(), "结束的任务应被移出列表");
    }

    /// 任务结果必须带回发起它的 `tab_id`。
    ///
    /// 没有这个字段时，后台任务完成后会被写进"当前激活的标签页"——
    /// 用户开着 A 存档、后台还在采样，这时打开 B，A 的缩略图就会画到 B 上
    /// （区块坐标重合，看起来就是"新存档残留了旧存档的图像"）。
    #[test]
    fn job_result_carries_tab_id() {
        let mut jobs = Jobs::default();
        let tmp = std::env::temp_dir().join("mcworld_test_tab_id");
        let _ = std::fs::create_dir_all(&tmp);

        jobs.spawn_load_cache(
            42,
            tmp,
            "minecraft:overworld".into(),
            std::path::PathBuf::from("/nonexistent/region"),
            "surface".into(),
            0,
        );

        let got = drain(&mut jobs);
        assert_eq!(got.len(), 1);
        match &got[0] {
            JobResult::CacheLoaded { tab_id, .. } => {
                assert_eq!(*tab_id, 42, "结果必须带回发起任务的 tab_id");
            }
            _ => panic!("应当是 CacheLoaded"),
        }
    }

    /// 成功完成的任务必须被清理，不能永远留在列表里。
    #[test]
    fn finished_jobs_are_removed() {
        let mut jobs = Jobs::default();
        let tmp = std::env::temp_dir().join("mcworld_test_no_cache_dir2");
        let _ = std::fs::create_dir_all(&tmp);

        jobs.spawn_load_cache(
            2, // tab_id
            tmp,
            "minecraft:the_end".to_string(),
            std::path::PathBuf::from("/nonexistent/region"),
            "surface".to_string(),
            0,
        );

        let _ = drain(&mut jobs);
        assert!(!jobs.is_busy());
        assert!(jobs.jobs.is_empty(), "完成的任务应当从列表里移除");
    }
}
