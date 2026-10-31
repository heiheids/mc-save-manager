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
    /// 扫描完成：区块元数据 + 实体 + 方块实体
    Scanned {
        dimension: String,
        chunks: Vec<ChunkMeta>,
        entities: Vec<EntityMeta>,
        block_entity_count: usize,
        block_entity_types: Vec<(String, usize)>,
    },
    /// 地图采样完成一批
    Sampled {
        dimension: String,
        surfaces: Vec<mcworld_core::ChunkSurface>,
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
    Failed(String),
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
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    JobResult::Failed(_) => {
                        job.finished = true;
                        out.push(msg);
                    }
                    other => out.push(other),
                }
            }
        }
        // 清理已完成且队列已空的
        self.jobs.retain(|j| !j.finished || !j.rx.try_recv().is_err());
        out
    }

    /// 启动扫描任务。
    pub fn spawn_scan(&mut self, world_root: PathBuf, dim: Dimension) {
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
                Err(e) => { let _ = tx.send(JobResult::Failed(e)); }
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
    pub fn spawn_sample(
        &mut self,
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

                for (i, (cx, cz)) in chunks.into_iter().enumerate() {
                    match world.chunk_surface(&dim, cx, cz, &mut cache, mode) {
                        Some(s) => batch.push(s),
                        None => {}
                    }
                    if batch.len() >= 128 || i + 1 == total {
                        let complete = i + 1 == total;
                        let _ = tx.send(JobResult::Sampled {
                            dimension: dim.id.clone(),
                            surfaces: std::mem::take(&mut batch),
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
                let _ = tx.send(JobResult::Failed(e));
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
                let _ = tx.send(JobResult::Failed(e));
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

                let _ = tx.send(JobResult::ChunksDeleted {
                    count: removed,
                    message: format!(
                        "已删除 {removed} 个区块（共请求 {}），备份在 {}",
                        chunks.len(),
                        backup_dir.display()
                    ),
                    backup_dir: backup_dir.clone(),
                });
                Ok(())
            })();

            match result {
                Ok(()) => {}
                Err(e) => {
                    let _ = tx.send(JobResult::Failed(e));
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
                Err(e) => { let _ = tx.send(JobResult::Failed(e)); }
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
                let _ = tx.send(JobResult::Failed(e));
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
                let _ = tx.send(JobResult::Failed(e));
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
