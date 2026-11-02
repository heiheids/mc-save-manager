<a id="chinese"></a>

# Minecraft 存档管理器
<img width="1282" height="792" alt="2026-10-31_150612" src="https://github.com/user-attachments/assets/89de1e7f-b991-46e6-80b3-c4b45369b57a" />

一个用 Rust 写的 Minecraft 存档编辑器。直接在存档文件层面编辑世界，
不依赖游戏运行，支持 **1.21.1** 到 **26.4 快照**。

> A Minecraft save editor written in Rust. Edits the world at the save-file
> level without needing the game running. Supports **1.21.1** through
> **26.4 snapshots**. [Jump to English](#english)

![版本](https://img.shields.io/badge/version-1.0.1-brightgreen)
![协议](https://img.shields.io/badge/license-GPL--3.0-blue)

**中文** | [English](#english)

---

## 功能

### 地图浏览

- 直接从 `.mca` 区块文件渲染世界地图，不需要启动游戏
- 支持主世界 / 下界 / 末地，可随时切换（**切换不会丢失已加载的数据**）
- 按"上方紧邻空气的非空气方块"判定地表，能正确处理洞穴、树叶、
  下界顶盖等特殊情况
- 区块网格、跳转到坐标、回到玩家 / 出生点、全图视野
- 状态栏实时显示鼠标处的 **坐标 / 区块 / 群系**（中文名）

### 区块编辑

- 框选任意区域，删除区块
- **多位置选择**：勾选后可累积多个选区，统一删除或批量导出
- 复制 / 粘贴区块（含方块实体与实体）
- 查看每个区块内的实体类型分布，可只清空某个区块的实体

### 存档属性

- 世界名、种子、游戏模式、难度、作弊、硬核
- 出生点、玩家坐标
- 游戏规则（58 条，中文名）

### 导入导出

| 格式 | 说明 |
|---|---|
| `.litematic` | Litematica 结构文件（只存非空气方块） |
| `.smclip` | [SwiftMap](https://github.com/heiheids) 剪贴板格式（完整快照，含空气/群系/实体，最多 7×7 区块） |

多选区导出时自动追加 `_1`、`_2`、`_3` 序号。

### 安全

- **每次写操作前自动备份**，备份按时间戳命名，保留在 `backups/` 目录
- 工具栏「撤销」一键还原上一次操作
- 删除 / 粘贴 / 改存档属性都是立即写盘 + 自动备份
- 检测 `session.lock` 时区分"游戏真的在运行"和"文件只读权限"，
  不会误报占用

---

## 截图

![主界面](https://github.com/user-attachments/assets/89de1e7f-b991-46e6-80b3-c4b45369b57a)

---

## 下载

直接到 [Releases](https://github.com/heiheids/mc-save-manager/releases) 页面下载对应平台的版本，
不需要自己编译：

| 平台 | 文件 |
|---|---|
| Windows x64 | `MinecraftSaveManager-windows-x64.exe` |
| Linux x64 | `MinecraftSaveManager-linux-x64` |
| macOS Intel | `MinecraftSaveManager-macos-x64` |
| macOS Apple Silicon | `MinecraftSaveManager-macos-arm64` |

Linux / macOS 下载后需要先给执行权限：

```bash
chmod +x MinecraftSaveManager-linux-x64
./MinecraftSaveManager-linux-x64
```

---

## 编译

### 前置条件

- [Rust](https://rustup.rs) 1.80 或更新
- 一个 C 链接器（任选其一）：
  - **MSVC**：装 Visual Studio Build Tools 的「使用 C++ 的桌面开发」
  - **MinGW**：装 MSYS2 的 mingw-w64 工具链

### 常规构建

```bash
git clone https://github.com/heiheids/mc-save-manager.git
cd mc-save-manager
cargo build --release
```

产物在 `target/release/mcworld-app.exe`。

### 零 C 工具链构建（作者使用）

不想装 MSVC / MinGW 的话，可以用 rustup 自带的 `rust-lld` + gnullvm 目标。
参考 `.cargo/config.local.toml.example`，复制成 `.cargo/config.toml` 并改好路径，
然后：

```bash
cargo build --target x86_64-pc-windows-gnullvm
```

### 图标修补（仅零工具链方案需要）

`.cargo/config.toml.example` 里链接了 `crates/app/assets/icon_obj.obj`（含 `.rsrc` 节）。
COFF 对象里资源数据项的 `OffsetToData` 只能写节内偏移，链接后需要补成全局 RVA，
否则 Windows 读不到图标：

```bash
node .build/patch_rsrc_rva.js target/x86_64-pc-windows-gnullvm/debug/mcworld-app.exe --write
```

`build.cmd` 已自动包含这一步。

---

## 项目结构

```
crates/
  core/                 # 与 UI 无关的存档读写库
    src/nbt.rs          # NBT 编解码（Java Modified UTF-8，字节级往返）
    src/anvil.rs        # .mca 区域文件（4KiB 扇区、zlib/gzip/LZ4/none、外部 .mcc）
    src/chunk.rs        # 区块解析、方块调色板、高度图、地表采样
    src/world.rs        # level.dat（1.21.1 vs 26.x 双格式）、维度发现
    src/edit.rs         # 区块复制 / 粘贴 / 删除、实体过滤
    src/safety.rs       # 写入前备份、会话回滚
    src/regions.rs      # 已加载片区的连通性分簇
    src/colors.rs       # 方块地图配色（含 26.x 新方块）
    src/swiftmap.rs     # SwiftMap 小地图数据（.smr 只读）
    src/smclip.rs       # SwiftMap 剪贴板格式（.smclip 读写）
  app/                  # eframe/egui 桌面界面
    src/main.rs         # 应用状态、标签页、维度视图
    src/panels.rs       # 顶栏 / 工具栏 / 侧栏 / 状态栏 / 各类窗口
    src/map_view.rs     # 地图绘制、相机、选区交互
    src/jobs.rs         # 后台任务（扫描 / 采样 / 删除 / 导出）
    src/lang.rs         # 中文名查表（运行时读用户的 .minecraft）
    src/theme.rs        # 米色 / 橄榄绿配色
    assets/             # 图标
docs/
  开发说明.md           # 详细的开发记录与踩坑笔记
```

---

## 已知限制

- **地图采样是全区扫描**，大存档（上万区块）首次加载需要十几秒
- 地图走的是存档扫描，暂未使用 SwiftMap 的瓦片数据加速
- 结构文件 `.litematic` / `.schem` 目前只能导出，导入未实现
- 加密存档（`session.lock` 被游戏独占）只能只读浏览
- 只在 Windows 上测试过

---

## 中文名称

方块 / 群系 / 实体 / 物品的中文名来自 Minecraft **官方语言文件**，
版权归 Mojang / Microsoft 所有。本项目**不附带该文件**，
而是在运行时从你自己安装的 `.minecraft/assets/` 目录读取。

找不到时（例如装了但从未启动过游戏）会退回显示英文注册名，程序不会崩溃。

---

## 协议

[GPL-3.0](LICENSE) © 黑山大叔

本项目与 Mojang Studios / Microsoft 无关，Minecraft 是其注册商标。


---
---

<a id="english"></a>

# Minecraft Save Manager

[中文](#chinese) | **English**

A Minecraft save editor written in Rust. It edits the world directly at the
save-file level — no need for the game to be running. Supports **1.21.1**
through **26.4 snapshots**.

![version](https://img.shields.io/badge/version-1.0.1-brightgreen)
![license](https://img.shields.io/badge/license-GPL--3.0-blue)

---

## Features

### Map browsing

- Renders the world map straight from `.mca` region files — the game does not
  need to be running
- Overworld / Nether / End, switchable at any time (**switching never discards
  already-loaded data**)
- Surface detection uses *"the topmost non-air block that has air directly
  above it"*, so caves, tree canopies and the Nether ceiling are handled
  correctly
- Chunk grid overlay, jump-to-coordinates, go-to-player / go-to-spawn, fit-all
- Status bar shows the cursor's **coordinates / chunk / biome** in real time

### Chunk editing

- Drag-select any rectangle and delete those chunks
- **Multi-location selection**: tick the box, accumulate several regions, then
  delete them all or export them in one go
- Copy / paste chunks, including block entities and entities
- Per-chunk entity type breakdown; clear entities from a single chunk

### Save properties

- Level name, seed, game mode, difficulty, cheats, hardcore
- Spawn point and player position
- Game rules (58 of them, with readable names)

### Import / export

| Format | Notes |
|---|---|
| `.litematic` | Litematica schematic (stores non-air blocks only) |
| `.smclip` | [SwiftMap](https://github.com/heiheids) clipboard format (full snapshot incl. air / biomes / entities, max 7×7 chunks) |

With multiple selections, `_1`, `_2`, `_3` suffixes are appended automatically.

### Safety

- **Automatic backup before every write**, timestamped, kept in a `backups/`
  folder next to the save
- Toolbar **Undo** restores the previous operation in one click
- Delete / paste / property changes are written immediately, with automatic
  backup
- `session.lock` detection distinguishes *"the game is really running"* from
  *"the file is just read-only"*, so it does not produce false lock reports

---

## Screenshots

![Main window](https://github.com/user-attachments/assets/89de1e7f-b991-46e6-80b3-c4b45369b57a)

---

## Download

Grab a prebuilt binary from the [Releases](https://github.com/heiheids/mc-save-manager/releases)
page — no need to build anything:

| Platform | File |
|---|---|
| Windows x64 | `MinecraftSaveManager-windows-x64.exe` |
| Linux x64 | `MinecraftSaveManager-linux-x64` |
| macOS Intel | `MinecraftSaveManager-macos-x64` |
| macOS Apple Silicon | `MinecraftSaveManager-macos-arm64` |

On Linux / macOS you need to mark the file executable first:

```bash
chmod +x MinecraftSaveManager-linux-x64
./MinecraftSaveManager-linux-x64
```

---

## Building

### Prerequisites

- [Rust](https://rustup.rs) 1.80 or newer
- A C linker (pick one):
  - **MSVC**: install Visual Studio Build Tools with *"Desktop development
    with C++"*
  - **MinGW**: install the mingw-w64 toolchain from MSYS2

### Standard build

```bash
git clone https://github.com/heiheids/mc-save-manager.git
cd mc-save-manager
cargo build --release
```

The binary lands at `target/release/mcworld-app.exe`.

### C-toolchain-free build (what the author uses)

If you would rather not install MSVC or MinGW, you can use rustup's bundled
`rust-lld` with the gnullvm target. See `.cargo/config.local.toml.example`,
copy it to `.cargo/config.toml` and fix up the paths, then:

```bash
cargo build --target x86_64-pc-windows-gnullvm
```

### Icon patching (only needed for the toolchain-free build)

The example config links `crates/app/assets/icon_obj.obj` (a COFF object with
a `.rsrc` section). Resource data entries inside a COFF object can only hold
section-relative offsets; after linking they must be turned into global RVAs,
otherwise Windows cannot read the icon:

```bash
node .build/patch_rsrc_rva.js target/x86_64-pc-windows-gnullvm/debug/mcworld-app.exe --write
```

`build.cmd` already runs this step.

---

## Project layout

```
crates/
  core/                 # UI-independent save-file library
    src/nbt.rs          # NBT codec (Java Modified UTF-8, byte-exact round-trip)
    src/anvil.rs        # .mca region files (4KiB sectors, zlib/gzip/LZ4/none, external .mcc)
    src/chunk.rs        # Chunk parsing, block palette, heightmaps, surface sampling
    src/world.rs        # level.dat (dual format: 1.21.1 vs 26.x), dimension discovery
    src/edit.rs         # Chunk copy / paste / delete, entity filtering
    src/safety.rs       # Pre-write backup, session rollback
    src/regions.rs      # Connectivity clustering of loaded regions
    src/colors.rs       # Block map colours (including new 26.x blocks)
    src/swiftmap.rs     # SwiftMap minimap data (.smr, read-only)
    src/smclip.rs       # SwiftMap clipboard format (.smclip, read/write)
  app/                  # eframe/egui desktop UI
    src/main.rs         # App state, tabs, per-dimension views
    src/panels.rs       # Top bar / toolbar / sidebar / status bar / windows
    src/map_view.rs     # Map rendering, camera, selection interaction
    src/jobs.rs         # Background tasks (scan / sample / delete / export)
    src/lang.rs         # Localised names (reads the user's .minecraft at runtime)
    src/theme.rs        # Cream / olive colour scheme
    assets/             # Icons
docs/
  开发说明.md           # Detailed development log and lessons learned (Chinese)
```

---

## Known limitations

- **Map sampling is a full scan** — large saves (tens of thousands of chunks)
  take 10–20 seconds on first load
- The map is built from a save scan; SwiftMap's tile data is not yet used to
  speed it up
- `.litematic` / `.schem` files can be exported but **not imported** yet
- Encrypted / locked saves can only be browsed read-only
- Only tested on Windows

---

## Localised names

Block / biome / entity / item names come from Minecraft's **official language
file**, copyright Mojang / Microsoft. This project **does not bundle that
file**; it reads it at runtime from your own `.minecraft/assets/` directory.

When the file cannot be found (e.g. the game was installed but never launched),
the program falls back to raw registry names and does not crash.

---

## License

[GPL-3.0](LICENSE) © 黑山大叔 (heishandashu)

This project is not affiliated with Mojang Studios or Microsoft.
Minecraft is a trademark of Mojang Synergies AB.
