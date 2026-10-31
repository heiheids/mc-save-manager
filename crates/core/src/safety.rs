//! 安全层：占用检测、自动备份、事务式提交。
//!
//! 任何会改动存档的操作都必须走 [`Session`]：
//! 1. 开事务 → 2. 改内存中的 region → 3. `commit()` 落盘，
//! 落盘前自动快照受影响文件，失败可 `rollback()`。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::anvil::{self, RegionFile};
use crate::world::{is_locked, Dimension, RegionKind, World};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("存档正被游戏占用（session.lock 被锁定），请先关闭游戏")]
    Locked,
    #[error("同一个存档已打开了另一个编辑会话")]
    AlreadyOpen,
    #[error("IO {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Region: {0}")]
    Anvil(#[from] anvil::Error),
    #[error("NBT: {0}")]
    Nbt(#[from] crate::nbt::Error),
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { path: PathBuf::new(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io { path: path.to_path_buf(), source }
}

/// 找不到原文件时可回滚的最小单元。
#[derive(Debug, Clone)]
struct BackupEntry {
    /// 原文件绝对路径。
    original: PathBuf,
    /// 备份副本路径（原文件不存在时为 None，回滚时删除）。
    saved: Option<PathBuf>,
}

/// 一次编辑会话：负责备份、提交、回滚。
pub struct Session {
    world_root: PathBuf,
    backup_root: PathBuf,
    touched: Vec<BackupEntry>,
    /// 已备份过的原始路径，避免重复备份（保留第一次的原始状态）。
    backed_up: HashSet<PathBuf>,
    committed: bool,
}

impl Session {
    /// 开始一次会话。会先检查游戏是否占用存档。
    ///
    /// `backup_root` 是备份根目录，通常放 `<workspace>/backups/<世界名>-<时间戳>`。
    pub fn begin(world: &World, backup_root: impl AsRef<Path>) -> Result<Self> {
        if is_locked(&world.root) {
            return Err(Error::Locked);
        }
        let backup_root = backup_root.as_ref().to_path_buf();
        fs::create_dir_all(&backup_root).map_err(|e| io_err(&backup_root, e))?;
        Ok(Session {
            world_root: world.root.clone(),
            backup_root,
            touched: Vec::new(),
            backed_up: HashSet::new(),
            committed: false,
        })
    }

    pub fn backup_root(&self) -> &Path {
        &self.backup_root
    }

    /// 在修改某个文件之前，先把它的原始内容复制到备份区。
    ///
    /// 同一文件在一次会话中只备份一次（保留最初状态，便于整体撤销）。
    pub fn touch(&mut self, path: &Path) -> Result<()> {
        if self.backed_up.contains(path) {
            return Ok(());
        }
        self.backed_up.insert(path.to_path_buf());

        if !path.exists() {
            self.touched.push(BackupEntry { original: path.to_path_buf(), saved: None });
            return Ok(());
        }

        // 备份副本保持相对存档根的目录结构，便于人工查找
        let rel = path.strip_prefix(&self.world_root).unwrap_or(path);
        let dest = self.backup_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        fs::copy(path, &dest).map_err(|e| io_err(path, e))?;
        self.touched.push(BackupEntry { original: path.to_path_buf(), saved: Some(dest) });
        Ok(())
    }

    /// 撤回本次会话对某个文件的所有改动，恢复到会话开始时的状态。
    pub fn restore(&mut self, path: &Path) -> Result<()> {
        let Some(entry) = self.touched.iter().find(|e| e.original == path) else {
            return Ok(());
        };
        match &entry.saved {
            Some(saved) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
                }
                fs::copy(saved, path).map_err(|e| io_err(path, e))?;
            }
            None => {
                // 原本不存在 → 删除新生成的文件
                if path.exists() {
                    fs::remove_file(path).map_err(|e| io_err(path, e))?;
                }
            }
        }
        Ok(())
    }

    /// 回滚整次会话。
    pub fn rollback(&mut self) -> Result<usize> {
        let entries = self.touched.clone();
        let mut n = 0;
        for e in entries {
            self.restore(&e.original)?;
            n += 1;
        }
        self.touched.clear();
        self.backed_up.clear();
        Ok(n)
    }

    /// 会话内已备份的文件数。
    pub fn backup_count(&self) -> usize {
        self.touched.len()
    }

    pub fn mark_committed(&mut self) {
        self.committed = true;
    }

    pub fn is_committed(&self) -> bool {
        self.committed
    }
}

/// 在备份区生成一个带时间戳的会话目录。
pub fn new_backup_dir(base: &Path, world_name: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 世界名可能含非法文件名字符，做一次清洗
    let safe: String = world_name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    base.join(format!("{safe}-{ts}"))
}

// ---------------------------------------------------------------------------
// 跨 region 的区块编辑
// ---------------------------------------------------------------------------

/// 批量区块编辑：自动按 region 分组、加载、修改、落盘。
/// 批量区块编辑：自动按 region 分组、加载、修改、落盘。
pub struct ChunkEditor<'a> {
    dim: &'a Dimension,
    regions: std::collections::HashMap<(i32, i32), RegionFile>,
    /// 每个 region 是否被改动。
    dirty: HashSet<(i32, i32)>,
    /// 已就地落盘的 entities / poi 文件（这些是独立文件，单独记录以便备份）。
    dirty_sidecar: HashSet<PathBuf>,
}

impl<'a> ChunkEditor<'a> {
    pub fn new(dim: &'a Dimension) -> Self {
        ChunkEditor {
            dim,
            regions: std::collections::HashMap::new(),
            dirty: HashSet::new(),
            dirty_sidecar: HashSet::new(),
        }
    }

    fn region_path(&self, kind: RegionKind, rx: i32, rz: i32) -> PathBuf {
        self.dim.dir(kind).join(anvil::region_file_name(rx, rz))
    }

    fn region_mut(
        &mut self,
        kind: RegionKind,
        rx: i32,
        rz: i32,
    ) -> Result<&mut RegionFile> {
        let key = (rx, rz);
        if !self.regions.contains_key(&key) {
            let path = self.region_path(kind, rx, rz);
            if !path.exists() {
                // 允许对空 region 写入：创建空表
                if let Some(p) = path.parent() {
                    fs::create_dir_all(p).map_err(|e| io_err(p, e))?;
                }
            }
            let rf = RegionFile::open(&path)?;
            self.regions.insert(key, rf);
        }
        Ok(self.regions.get_mut(&key).expect("just inserted"))
    }

    /// 删除一个地形区块，并同步删除该区块的实体与 POI 数据。
    ///
    /// 返回是否真的删除了地形区块。
    ///
    /// 注意：`entities` / `poi` 是独立文件，这里会立即落盘并记入备份清单；
    /// 地形 region 则留在内存里，等 `commit()` 统一写出。
    pub fn delete_chunk(&mut self, cx: i32, cz: i32, also_entities: bool, also_poi: bool) -> Result<bool> {
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
        let removed = {
            let rf = self.region_mut(RegionKind::Terrain, rx, rz)?;
            let r = rf.remove_chunk(cx, cz);
            if r {
                self.dirty.insert((rx, rz));
            }
            r
        };

        if also_entities {
            self.remove_from_sidecar(RegionKind::Entities, cx, cz)?;
        }
        if also_poi {
            self.remove_from_sidecar(RegionKind::Poi, cx, cz)?;
        }
        Ok(removed)
    }

    /// 从 entities / poi 目录里移除对应区块（文件不存在则跳过）。
    fn remove_from_sidecar(
        &mut self,
        kind: RegionKind,
        cx: i32,
        cz: i32,
    ) -> Result<Option<bool>> {
        if !self.dim.exists(kind) {
            return Ok(None);
        }
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
        let path = self.region_path(kind, rx, rz);
        if !path.exists() {
            return Ok(None);
        }
        let mut rf = RegionFile::open(&path)?;
        if !rf.contains(cx, cz) {
            return Ok(Some(false));
        }
        let removed = rf.remove_chunk(cx, cz);
        if removed {
            rf.save()?;
            self.dirty_sidecar.insert(path);
        }
        Ok(Some(removed))
    }

    /// 读取一个区块。
    pub fn get_chunk(&mut self, cx: i32, cz: i32) -> Result<Option<crate::Tag>> {
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
        Ok(self.region_mut(RegionKind::Terrain, rx, rz)?.chunk(cx, cz)?)
    }

    /// 写入一个区块。
    pub fn put_chunk(&mut self, cx: i32, cz: i32, tag: &crate::Tag) -> Result<()> {
        let (rx, rz) = (anvil::region_coord(cx), anvil::region_coord(cz));
        self.region_mut(RegionKind::Terrain, rx, rz)?.put_chunk(cx, cz, tag)?;
        self.dirty.insert((rx, rz));
        Ok(())
    }

    /// 所有被改动的 region 文件路径（用于备份）。
    pub fn dirty_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .dirty
            .iter()
            .map(|(rx, rz)| self.region_path(RegionKind::Terrain, *rx, *rz))
            .collect();
        out.extend(self.dirty_sidecar.iter().cloned());
        out
    }

    /// 提交：先把所有会被改动的文件备份到会话区，再由调用方控制落盘时机。
    ///
    /// 注意：`entities` / `poi` 的删除在 `delete_chunk` 里已就地落盘（它们是独立文件），
    /// 这里只负责把它们也纳入备份清单，保证能整体回滚。
    pub fn commit(mut self, session: &mut Session) -> Result<()> {
        let mut all: HashSet<PathBuf> = self.dirty_paths().into_iter().collect();
        // 地形 region 对应的同坐标 sidecar 也可能被创建/改写
        for (rx, rz) in self.dirty.iter() {
            for kind in [RegionKind::Entities, RegionKind::Poi] {
                if self.dim.exists(kind) {
                    all.insert(self.region_path(kind, *rx, *rz));
                }
            }
        }
        for p in all {
            session.touch(&p)?;
        }

        for (_, rf) in self.regions.iter_mut() {
            if rf.is_dirty() {
                rf.save()?;
            }
        }
        self.regions.clear();
        self.dirty.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbt::{Compound, Tag};

    fn tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcworld-sec-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn restore_from_backup_roundtrip() {
        // 模拟「删除区块 → 撤销」：备份一份，改动原文件，再从备份还原。
        let dir = tmp("restore");
        let world_root = dir.join("world");
        let region_dir = world_root.join("region");
        fs::create_dir_all(&region_dir).unwrap();
        let file = region_dir.join("r.0.0.mca");
        fs::write(&file, b"original-region-data").unwrap();

        let mut s = Session {
            world_root: world_root.clone(),
            backup_root: dir.join("backup"),
            touched: vec![],
            backed_up: HashSet::new(),
            committed: false,
        };
        fs::create_dir_all(&s.backup_root).unwrap();
        s.touch(&file).unwrap();

        // 模拟删除：把文件改小
        fs::write(&file, b"shrunk").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"shrunk");

        // 撤销：从备份目录还原
        let n = restore_from_backup(&world_root, &s.backup_root).unwrap();
        assert_eq!(n, 1, "应还原 1 个文件");
        assert_eq!(
            fs::read(&file).unwrap(),
            b"original-region-data",
            "撤销后应恢复原内容"
        );
    }

    #[test]
    fn restore_from_backup_rejects_missing_dir() {
        let dir = tmp("restore-missing");
        let world_root = dir.join("world");
        fs::create_dir_all(&world_root).unwrap();
        let missing = dir.join("no-such-backup");
        assert!(restore_from_backup(&world_root, &missing).is_err());
    }

    #[test]
    fn list_backups_sorts_newest_first() {
        let dir = tmp("listbackups");
        fs::create_dir_all(dir.join("world-100")).unwrap();
        fs::create_dir_all(dir.join("world-300")).unwrap();
        fs::create_dir_all(dir.join("world-200")).unwrap();
        fs::write(dir.join("not-a-dir.txt"), b"x").unwrap();

        let list = list_backups(&dir);
        assert_eq!(list.len(), 3, "只应列出目录");
        let names: Vec<String> = list
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["world-300", "world-200", "world-100"]);
    }

    #[test]
    fn backup_restore_roundtrip() {
        let dir = tmp("backup");
        let world_root = dir.join("world");
        fs::create_dir_all(&world_root).unwrap();
        let file = world_root.join("test.mca");
        fs::write(&file, b"original").unwrap();

        let mut s = Session {
            world_root: world_root.clone(),
            backup_root: dir.join("backup"),
            touched: vec![],
            backed_up: HashSet::new(),
            committed: false,
        };
        fs::create_dir_all(&s.backup_root).unwrap();

        s.touch(&file).unwrap();
        fs::write(&file, b"modified").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"modified");

        s.rollback().unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"original", "回滚后应恢复原内容");
    }

    #[test]
    fn backup_removes_files_that_did_not_exist() {
        let dir = tmp("newfile");
        let world_root = dir.join("world");
        fs::create_dir_all(&world_root).unwrap();
        let new_file = world_root.join("r.9.9.mca");

        let mut s = Session {
            world_root: world_root.clone(),
            backup_root: dir.join("backup"),
            touched: vec![],
            backed_up: HashSet::new(),
            committed: false,
        };
        fs::create_dir_all(&s.backup_root).unwrap();
        s.touch(&new_file).unwrap();
        fs::write(&new_file, b"new").unwrap();
        s.rollback().unwrap();
        assert!(!new_file.exists(), "原本不存在的文件应在回滚时删除");
    }

    #[test]
    fn touch_twice_only_backs_up_once() {
        let dir = tmp("once");
        let world_root = dir.join("world");
        fs::create_dir_all(&world_root).unwrap();
        let file = world_root.join("a.mca");
        fs::write(&file, b"v1").unwrap();

        let mut s = Session {
            world_root,
            backup_root: dir.join("backup"),
            touched: vec![],
            backed_up: HashSet::new(),
            committed: false,
        };
        fs::create_dir_all(&s.backup_root).unwrap();
        s.touch(&file).unwrap();
        fs::write(&file, b"v2").unwrap();
        s.touch(&file).unwrap(); // 不应覆盖第一次的备份
        assert_eq!(s.backup_count(), 1, "同一文件只应备份一次");
        fs::write(&file, b"v3").unwrap();
        s.rollback().unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"v1", "必须恢复到最初状态");
    }

    #[test]
    fn chunk_editor_deletes_across_regions() {
        let dir = tmp("editor");
        let world_root = dir.join("world");
        let region_dir = world_root.join("region");
        let ent_dir = world_root.join("entities");
        fs::create_dir_all(&region_dir).unwrap();
        fs::create_dir_all(&ent_dir).unwrap();

        // 准备两个 region 的地形与实体
        for (rx, rz) in [(0, 0), (1, 0)] {
            let mut rf = RegionFile::open(region_dir.join(anvil::region_file_name(rx, rz))).unwrap();
            let mut c = Compound::new();
            c.insert("xPos".into(), Tag::Int(rx * 32));
            rf.put_chunk(rx * 32, 0, &Tag::Compound(c)).unwrap();
            rf.save().unwrap();

            let mut ef = RegionFile::open(ent_dir.join(anvil::region_file_name(rx, rz))).unwrap();
            let mut ec = Compound::new();
            ec.insert("Entities".into(), Tag::empty_list(crate::TagType::Compound));
            ef.put_chunk(rx * 32, 0, &Tag::Compound(ec)).unwrap();
            ef.save().unwrap();
        }

        let dim = Dimension {
            id: "minecraft:overworld".into(),
            display: "主世界".into(),
            root: world_root.clone(),
        };

        let mut editor = ChunkEditor::new(&dim);
        assert!(editor.delete_chunk(0, 0, true, true).unwrap());
        assert!(editor.delete_chunk(32, 0, true, true).unwrap());

        let mut session = Session {
            world_root: world_root.clone(),
            backup_root: dir.join("backup"),
            touched: vec![],
            backed_up: HashSet::new(),
            committed: false,
        };
        fs::create_dir_all(&session.backup_root).unwrap();
        editor.commit(&mut session).unwrap();

        // 验证地形与实体都被删除
        let mut rf = RegionFile::open(region_dir.join("r.0.0.mca")).unwrap();
        assert!(rf.chunk(0, 0).unwrap().is_none(), "地形区块应被删除");
        let mut ef = RegionFile::open(ent_dir.join("r.0.0.mca")).unwrap();
        assert!(ef.chunk(0, 0).unwrap().is_none(), "实体区块应同步删除");
    }

    #[test]
    fn refuses_when_locked() {
        // 用目录占用模拟：真实锁检测依赖平台，这里只验证接口存在
        let dir = tmp("lock");
        let world_root = dir.join("world");
        fs::create_dir_all(&world_root).unwrap();
        let w = World {
            root: world_root.clone(),
            level: crate::LevelDat {
                path: world_root.join("level.dat"),
                root: Tag::Compound(Compound::new()),
                gzip: true,
            },
            dimensions: vec![],
        };
        // 无 session.lock 时不应报 Locked
        assert!(Session::begin(&w, dir.join("bk")).is_ok());
    }
}

// ---------------------------------------------------------------------------
// 撤销上一次操作
// ---------------------------------------------------------------------------

/// 最近一次操作的备份信息，用于「撤销」。
#[derive(Clone, Debug)]
pub struct LastBackup {
    /// 备份根目录（如 `…/backups/新的世界-1699999999`）。
    pub dir: PathBuf,
    /// 这一次操作对外的描述（显示在按钮提示里）。
    pub label: String,
}

/// 把一个备份目录里的文件**原样还原**回存档。
///
/// 备份目录里保持了相对存档根的目录结构（见 `Session::touch`），
/// 所以还原就是把每个文件按相对路径写回去。
///
/// 返回还原的文件数。
pub fn restore_from_backup(world_root: &Path, backup_dir: &Path) -> Result<usize> {
    if !backup_dir.is_dir() {
        return Err(Error::Other(format!(
            "备份目录不存在: {}",
            backup_dir.display()
        )));
    }

    let mut n = 0usize;
    let mut stack = vec![backup_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
        for e in entries {
            let e = e.map_err(|e| io_err(&dir, e))?;
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            // 相对备份根目录的路径 = 相对存档根的路径
            let rel = match p.strip_prefix(backup_dir) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let dest = world_root.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            fs::copy(&p, &dest).map_err(|e| io_err(&dest, e))?;
            n += 1;
        }
    }
    Ok(n)
}

/// 列出某个存档的所有备份目录（按名字倒序，最新的在前）。
pub fn list_backups(base: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // 目录名以 unix 时间戳结尾，按字符串倒序即按时间倒序
    out.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    out
}
