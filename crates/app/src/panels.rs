//! 界面各区块的绘制。

use eframe::egui::{self, Align, Color32, Layout, RichText, Vec2};
use std::path::PathBuf;
use mcworld_core::world::{LevelDat, RegionKind};

use crate::map_view;
use crate::theme;
use crate::{App, ConfirmAction, StatusKind, WorldTab};

/// 顶栏：存档标签 + 打开新存档。
pub fn top_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top")
        .frame(
            egui::Frame::new()
                .fill(theme::TOPBAR)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .stroke(egui::Stroke::new(1.0, theme::BORDER)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let mut close: Option<usize> = None;
                for i in 0..app.tabs.len() {
                    let name = app.tabs[i].name.clone();
                    let active = i == app.active;
                    let text = RichText::new(format!("{name}"))
                        .size(13.0)
                        .color(if active { theme::TEXT } else { theme::TEXT_DIM });
                    let btn = egui::Button::new(text)
                        .fill(if active { theme::ACCENT_SOFT } else { theme::PANEL })
                        .stroke(egui::Stroke::new(1.0, if active { theme::ACCENT } else { theme::BORDER }))
                        .min_size(Vec2::new(120.0, 30.0));
                    if ui.add(btn).clicked() {
                        app.active = i;
                        app.request_scan();
                    }
                    // 右键关闭
                    if ui.rect_contains_pointer(ui.min_rect()) {
                        let _ = &close;
                    }
                    ui.add_space(2.0);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(theme::accent_button("+ 打开新存档")).clicked() {
                        open_world_dialog(app);
                    }
                    if !app.tabs.is_empty() {
                        if ui.add(theme::tool_button("关闭当前")).clicked() {
                            close = Some(app.active);
                        }
                    }
                });

                if let Some(i) = close {
                    app.tabs.remove(i);
                    if app.active >= app.tabs.len() && !app.tabs.is_empty() {
                        app.active = app.tabs.len() - 1;
                    }
                    app.set_status("已关闭存档", StatusKind::Info);
                }
            });
        });
}

fn open_world_dialog(app: &mut App) {
    let start = app
        .tabs
        .get(app.active)
        .map(|t| t.path.parent().map(|p| p.to_path_buf()))
        .flatten()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    if let Some(dir) = rfd::FileDialog::new()
        .set_title("选择存档目录（包含 level.dat 的文件夹）")
        .set_directory(start)
        .pick_folder()
    {
        app.open_world(dir);
    }
}

/// 工具栏：保存 / 另存为 / 复制区域 / 粘贴区域 / 另存区域 / 导入区域 / 实体管理。
pub fn toolbar(app: &mut App, ctx: &egui::Context) {
    let mut pending_toolbar_confirm: Option<ConfirmAction> = None;
    let mut pending_undo = false;
    let mut pending_reload = false;
    let mut pending_about = false;
    let mut pending_export_html = false;
    let mut pending_save_cache = false;

    egui::TopBottomPanel::top("toolbar")
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(12, 6))
                .stroke(egui::Stroke::new(1.0, theme::BORDER)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let has_tab = !app.tabs.is_empty();

                ui.add_enabled_ui(has_tab, |ui| {
                    // 全部操作都是「立即执行」，所以这里不是保存按钮，
                    // 而是一个状态灯：有未落盘改动时显示黄点，平时显示已保存。
                    let dirty = app.tab().map(|t| t.dirty_level).unwrap_or(false);
                    let (label, color) = if dirty {
                        ("● 正在自动保存…", theme::WARN)
                    } else {
                        ("● 已自动保存", Color32::from_rgb(0x6A, 0x9A, 0x4C))
                    };
                    ui.label(RichText::new(label).size(12.0).color(color));

                    ui.separator();

                    let sel = app.tab().and_then(|t| t.view().selection);
                    ui.add_enabled_ui(sel.is_some(), |ui| {
                        if ui.add(theme::tool_button("复制区域")).clicked() {
                            do_copy(app);
                        }
                    });

                    let can_paste = app.clipboard.is_some();
                    ui.add_enabled_ui(can_paste, |ui| {
                        let label = if app.paste_mode {
                            "粘贴区域（选择中…）"
                        } else {
                            "粘贴区域"
                        };
                        let btn = egui::Button::new(label).fill(if app.paste_mode {
                            theme::DANGER
                        } else {
                            theme::FIELD
                        });
                        if ui
                            .add(btn)
                            .on_hover_text("进入粘贴定位模式：鼠标处显示红框，左键确认位置")
                            .clicked()
                        {
                            app.paste_mode = !app.paste_mode;
                            app.paste_anchor = None;
                            if app.paste_mode {
                                app.set_status(
                                    "粘贴定位：移动鼠标选择位置，左键确认（右键取消）",
                                    StatusKind::Info,
                                );
                            }
                        }
                    });

                    ui.add_enabled_ui(sel.is_some(), |ui| {
                        ui.menu_button("导出区域", |ui| {
                            if ui
                                .button("Litematica (.litematic)")
                                .on_hover_text("只存非空气方块，体积小，适合做结构")
                                .clicked()
                            {
                                export_selection(app);
                                ui.close();
                            }
                            if ui
                                .button("SwiftMap 剪贴板 (.smclip)")
                                .on_hover_text(
                                    "SwiftMap 模组的区块快照格式，含空气/群系/实体；\n\
                                     最多 7×7 区块，本程序导出一律是全部高度",
                                )
                                .clicked()
                            {
                                export_smclip(app);
                                ui.close();
                            }
                        });
                    });

                    if ui
                        .add(theme::tool_button("导入区域"))
                        .on_hover_text("可导入 .litematic / .schem / .smclip")
                        .clicked()
                    {
                        import_any(app);
                    }

                    if ui
                        .add(theme::tool_button("导出全图"))
                        .on_hover_text(
                            "把整个世界渲染成 HTML 网页（可离线打开，鼠标查看坐标与群系）",
                        )
                        .clicked()
                    {
                        pending_export_html = true;
                    }

                    if ui
                        .add(theme::tool_button("保存缓存"))
                        .on_hover_text(
                            "把已渲染的缩略图存到磁盘，下次打开同一存档可秒开（大存档推荐）",
                        )
                        .clicked()
                    {
                        pending_save_cache = true;
                    }

                    ui.separator();

                    // 面板开关：与上面的「选区相关」按钮脱钩，
                    // 只要打开了存档就可用（不受选区状态影响）。
                    let region_label = {
                        let n = app.tab().map(|t| t.view().regions.len()).unwrap_or(0);
                        if n > 0 {
                            format!("已加载片区 ({n})")
                        } else {
                            "已加载片区".to_string()
                        }
                    };
                    let selected = ui.add(
                        egui::Button::new(region_label).fill(if app.show_regions {
                            theme::ACCENT_SOFT
                        } else {
                            theme::FIELD
                        }),
                    );
                    if selected
                        .on_hover_text("列出所有互不相连的已加载区域，点击可跳转")
                        .clicked()
                    {
                        app.show_regions = !app.show_regions;
                    }

                    let selected = ui.add(
                        egui::Button::new("实体管理")
                            .fill(if app.show_entities { theme::ACCENT_SOFT } else { theme::FIELD }),
                    );
                    if selected.clicked() {
                        app.show_entities = !app.show_entities;
                    }
                    let selected = ui.add(
                        egui::Button::new("区块列表")
                            .fill(if app.show_chunks { theme::ACCENT_SOFT } else { theme::FIELD }),
                    );
                    if selected.clicked() {
                        app.show_chunks = !app.show_chunks;
                    }
                    let selected = ui.add(
                        egui::Button::new("游戏规则")
                            .fill(if app.show_gamerules { theme::ACCENT_SOFT } else { theme::FIELD }),
                    );
                    if selected.clicked() {
                        app.show_gamerules = !app.show_gamerules;
                    }

                    ui.separator();

                    // ---- 多位置选择勾选槽 ----
                    {
                        let multi = app.tab().map(|t| t.view().multi_select).unwrap_or(false);
                        let mut on = multi;
                        if ui
                            .checkbox(&mut on, "多位置")
                            .on_hover_text(
                                "勾选后可框选多个区域，每个区域独立保留；\n                                 左键或右键单击空白处，取消上一个选区",
                            )
                            .changed()
                        {
                            if let Some(tab) = app.tabs.get_mut(app.active) {
                                let v = tab.view_mut();
                                v.multi_select = on;
                                if on {
                                    // 进入多选：把当前选区收进列表
                                    if let Some(s) = v.selection.take() {
                                        v.selections.push(s);
                                    }
                                } else {
                                    // 退出多选：清空列表
                                    v.selections.clear();
                                }
                            }
                        }
                        if multi {
                            let n = app
                                .tab()
                                .map(|t| t.view().selections.len())
                                .unwrap_or(0);
                            if n > 0 {
                                ui.label(
                                    RichText::new(format!("已选 {n} 处"))
                                        .size(11.0)
                                        .color(theme::ACCENT),
                                );
                            }
                        }
                    }

                    ui.separator();

                    // ---- 删除区块（红色，需要选区）----
                    // 多选模式下删除**所有**已确认选区 + 当前选区
                    let (targets, total_chunks) = collect_delete_targets(app);
                    ui.add_enabled_ui(!targets.is_empty(), |ui| {
                        let btn = egui::Button::new(
                            RichText::new("删除区块").color(Color32::WHITE),
                        )
                        .fill(theme::DANGER);
                        let hint = if targets.len() > 1 {
                            format!(
                                "删除 {} 个选区，共 {total_chunks} 个区块（会先自动备份）",
                                targets.len()
                            )
                        } else {
                            "删除地图上框选的区块（会先自动备份）".to_string()
                        };
                        if ui.add(btn).on_hover_text(hint).clicked() {
                            pending_toolbar_confirm = Some(ConfirmAction::DeleteChunks {
                                count: total_chunks,
                                dim: app
                                    .tab()
                                    .and_then(|t| t.dim().map(|d| d.display.clone()))
                                    .unwrap_or_default(),
                                targets,
                            });
                        }
                    });

                    // ---- 撤销上一次写操作 ----
                    let can_undo = app.last_backup.is_some();
                    ui.add_enabled_ui(can_undo, |ui| {
                        let hint = match &app.last_backup {
                            Some(b) => format!("撤销：{}  ({})", b.label, b.dir.display()),
                            None => "还没有可撤销的操作".to_string(),
                        };
                        if ui
                            .add(egui::Button::new("撤销"))
                            .on_hover_text(hint)
                            .clicked()
                        {
                            pending_undo = true;
                        }
                    });

                    // ---- 重载存档 ----
                    if ui
                        .add(egui::Button::new("重载存档"))
                        .on_hover_text("丢弃内存中的扫描结果，重新从磁盘读取")
                        .clicked()
                    {
                        pending_reload = true;
                    }

                    // ---- 关于（靠右）----
                    // 用 right_to_left 布局把它推到这一行的最右端
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("关于"))
                            .on_hover_text(concat!(
                                "Minecraft 存档管理器 v",
                                env!("CARGO_PKG_VERSION"),
                                " 正式版\n作者：黑山大叔"
                            ))
                            .clicked()
                        {
                            pending_about = true;
                        }
                    });
                });
            });
        });

    // ---- 绘制结束后执行 ----
    if let Some(a) = pending_toolbar_confirm {
        app.pending_confirm = Some(a);
    }
    if pending_undo {
        undo_last(app);
    }
    if pending_reload {
        app.reload_requested = true;
    }
    if pending_about {
        app.show_about = true;
    }
    if pending_export_html {
        app.show_export_html = true;
    }
    if pending_save_cache {
        app.request_save_cache();
    }
}

/// 撤销上一次写操作：把备份目录里的文件原样还原。
fn undo_last(app: &mut App) {
    let Some(backup) = app.last_backup.clone() else {
        app.set_status("没有可撤销的操作", StatusKind::Info);
        return;
    };
    let Some(tab) = app.tab() else { return };
    let root = tab.path.clone();

    match mcworld_core::safety::restore_from_backup(&root, &backup.dir) {
        Ok(n) => {
            app.set_status(
                format!("已撤销：{}（还原 {n} 个文件）", backup.label),
                StatusKind::Success,
            );
            app.last_backup = None;
            app.refresh_current();
        }
        Err(e) => app.set_status(format!("撤销失败: {e}"), StatusKind::Error),
    }
}

fn do_copy(app: &mut App) {
    let Some(tab) = app.tab() else { return };
    let Some(sel) = tab.view().selection else { return };
    let (x0, z0, x1, z1) = sel.normalized();
    let Some(dim) = tab.dim().cloned() else { return };
    let root = tab.path.clone();
    app.jobs.spawn_copy(root, dim, x0, z0, x1, z1, true);
    app.set_status("正在复制选区…", StatusKind::Info);
}

/// 收集本次操作要处理的选区（已归一化的区块范围）。
///
/// - 单选模式：只有当前选区
/// - 多选模式：已确认的所有选区 + 当前选区（如果存在）
///
/// 返回 (范围列表, 总区块数)。自动去重。
fn collect_delete_targets(app: &crate::App) -> (Vec<(i32, i32, i32, i32)>, usize) {
    let Some(tab) = app.tab() else {
        return (Vec::new(), 0);
    };
    let v = tab.view();
    let mut out: Vec<(i32, i32, i32, i32)> = Vec::new();

    if v.multi_select {
        for sel in &v.selections {
            out.push(sel.normalized());
        }
    }
    if let Some(sel) = v.selection {
        out.push(sel.normalized());
    }

    // 去重（同一范围被重复加入时只算一次）
    out.sort_unstable();
    out.dedup();

    let total = out
        .iter()
        .map(|(a, b, c, d)| ((c - a + 1) as usize) * ((d - b + 1) as usize))
        .sum();
    (out, total)
}

/// 导出选区为 SwiftMap 的 `.smclip` 格式。
///
/// 按需求：本程序导出一律是**全部高度**（Y -64..319）。
fn export_smclip(app: &mut App) {
    let Some(tab) = app.tab() else { return };

    // 收集要导出的选区：多选模式下是已确认列表 + 当前选区，单选模式只有当前选区
    let (targets, _) = collect_delete_targets(app);
    if targets.is_empty() {
        app.set_status("没有选区可导出", StatusKind::Info);
        return;
    }

    // SwiftMap 单边上限 7 区块，先全部校验
    for (a, b, c, d) in &targets {
        let sx = c - a + 1;
        let sz = d - b + 1;
        if sx > 7 || sz > 7 {
            app.set_status(
                format!(
                    ".smclip 最多 7×7 区块，选区 ({a},{b})→({c},{d}) 是 {sx}×{sz}，请缩小"
                ),
                StatusKind::Error,
            );
            return;
        }
    }

    let dim_name = tab
        .dim()
        .map(|d| d.display.clone())
        .unwrap_or_else(|| "world".into());

    // 默认文件名：单选区带坐标，多选区只给基础名（后缀由程序加序号）
    let default_name = if targets.len() == 1 {
        let (a, b, c, d) = targets[0];
        format!("{dim_name}_{a}_{b}_{}x{}.smclip", c - a + 1, d - b + 1)
    } else {
        format!("{dim_name}_选区.smclip")
    };

    let Some(path) = rfd::FileDialog::new()
        .set_title(if targets.len() > 1 {
            "导出为多个 .smclip（程序会自动加序号）"
        } else {
            "导出为 SwiftMap 剪贴板 (.smclip)"
        })
        .set_file_name(&default_name)
        .add_filter("SwiftMap 剪贴板", &["smclip"])
        .save_file()
    else {
        return;
    };

    let Some(dim) = tab.dim().cloned() else { return };
    let root = tab.path.clone();

    // 多选区：按用户选的文件名，尾部追加 _1 _2 _3…
    // 单选区：直接用这个名字
    let paths: Vec<PathBuf> = if targets.len() == 1 {
        vec![path.clone()]
    } else {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "region".into());
        let ext = path
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "smclip".into());
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        targets
            .iter()
            .enumerate()
            .map(|(i, _)| dir.join(format!("{stem}_{}.{ext}", i + 1)))
            .collect()
    };

    for (i, (a, b, c, d)) in targets.iter().enumerate() {
        let out = paths[i].clone();
        let note = format!("{dim_name} ({a},{b}) {}×{}", c - a + 1, d - b + 1);
        app.jobs
            .spawn_export_smclip(root.clone(), dim.clone(), *a, *b, *c, *d, out, note);
    }

    if targets.len() > 1 {
        app.set_status(
            format!(
                "正在导出 {} 个选区（文件名自动加 _1.._{}）…",
                targets.len(),
                targets.len()
            ),
            StatusKind::Info,
        );
    } else {
        app.set_status("正在导出 .smclip…", StatusKind::Info);
    }
}

/// 导入 SwiftMap 的 `.smclip` 到剪贴板，之后照常走「粘贴区域」流程。
fn import_smclip(app: &mut App) {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入 SwiftMap 剪贴板 (.smclip)")
        .add_filter("SwiftMap 剪贴板", &["smclip"])
        .pick_file()
    else {
        return;
    };
    app.jobs.spawn_import_smclip(path);
    app.set_status("正在导入 .smclip…", StatusKind::Info);
}

fn export_selection(app: &mut App) {
    let Some(tab) = app.tab() else { return };

    let (targets, _) = collect_delete_targets(app);
    if targets.is_empty() {
        app.set_status("没有选区可导出", StatusKind::Info);
        return;
    }

    let dim_name = tab
        .dim()
        .map(|d| d.display.clone())
        .unwrap_or_else(|| "world".into());

    let default_name = if targets.len() == 1 {
        let (a, b, c, d) = targets[0];
        format!("{dim_name}_{a}_{b}_{c}_{d}.litematic")
    } else {
        format!("{dim_name}_选区.litematic")
    };

    let Some(path) = rfd::FileDialog::new()
        .set_title(if targets.len() > 1 {
            "导出为多个 .litematic（程序会自动加序号）"
        } else {
            "导出选区为 .litematic"
        })
        .set_file_name(&default_name)
        .add_filter("Litematica 结构", &["litematic"])
        .save_file()
    else {
        return;
    };

    let Some(dim) = tab.dim().cloned() else { return };
    let root = tab.path.clone();

    // 多选区：尾部追加 _1 _2 _3…
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "region".into());
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "litematic".into());

    let mut ok = 0usize;
    let mut errs: Vec<String> = Vec::new();
    for (i, (x0, z0, x1, z1)) in targets.iter().enumerate() {
        let out = if targets.len() == 1 {
            path.clone()
        } else {
            dir.join(format!("{stem}_{}.{ext}", i + 1))
        };
        match crate::export::export_litematic(&root, &dim, *x0, *z0, *x1, *z1, &out) {
            Ok(_) => ok += 1,
            Err(e) => errs.push(format!("第 {} 处: {e}", i + 1)),
        }
    }

    if errs.is_empty() {
        app.set_status(
            if targets.len() > 1 {
                format!("已导出 {} 个 .litematic（自动加序号）", ok)
            } else {
                "已导出 .litematic".to_string()
            },
            StatusKind::Success,
        );
    } else {
        app.set_status(
            format!("成功 {ok} 个，失败 {}: {}", errs.len(), errs.join("; ")),
            StatusKind::Error,
        );
    }
}

/// 导入区域：按扩展名分派到不同的解析器。
///
/// - `.smclip` → SwiftMap 剪贴板（已实现，导入后可直接粘贴）
/// - `.litematic` / `.schem` → 结构文件（暂未实现）
fn import_any(app: &mut App) {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入区域文件")
        .add_filter("支持的格式", &["smclip", "litematic", "schem", "schematic"])
        .add_filter("SwiftMap 剪贴板", &["smclip"])
        .add_filter("结构文件", &["litematic", "schem", "schematic"])
        .pick_file()
    else {
        return;
    };

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "smclip" => {
            app.jobs.spawn_import_smclip(path);
            app.set_status("正在导入 .smclip…", StatusKind::Info);
        }
        _ => {
            app.set_status(
                format!(
                    "已选择 {} —— {} 结构导入尚未实现，当前只支持 .smclip",
                    path.display(),
                    ext
                ),
                StatusKind::Info,
            );
        }
    }
}

fn import_schematic(app: &mut App) {
    let Some(path) = rfd::FileDialog::new()
        .set_title("导入结构文件")
        .add_filter("结构文件", &["litematic", "schem", "schematic"])
        .pick_file()
    else {
        return;
    };
    app.set_status(
        format!(
            "已选择 {} —— 结构粘贴功能需先选中目标位置（下一步实现）",
            path.display()
        ),
        StatusKind::Info,
    );
}

/// 左侧属性栏。
pub fn sidebar(app: &mut App, ctx: &egui::Context) {
    egui::SidePanel::left("sidebar")
        .resizable(true)
        .default_width(260.0)
        .width_range(220.0..=380.0)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(14))
                .stroke(egui::Stroke::new(1.0, theme::BORDER)),
        )
        .show(ctx, |ui| {
            if app.tabs.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(theme::section_title("尚未打开存档"));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("点击右上角「打开新存档」\n选择包含 level.dat 的文件夹")
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                return;
            }

            let Some(tab) = app.tabs.get(app.active) else { return };
            let locked = tab.world.is_locked();

            // 本帧只读取侧边栏数据，所有会改动 app 的操作先收集到 pending_*，
            // 等侧边栏绘制结束后再统一执行（否则会与 tab 的不可变借用冲突）。
            let mut pending_switch: Option<usize> = None;
            let mut pending_scan = false;
            let mut pending_sample = false;
            let mut pending_confirm: Option<ConfirmAction> = None;
            // 跳转请求（方块坐标）
            let mut pending_goto: Option<(f32, f32)> = None;
            let mut spawn_edit: Option<(i32, i32, i32)> = None;
            let mut player_edit: Option<(f64, f64, f64)> = None;
            // (游戏模式, 难度, 作弊, 硬核)
            let mut combo_edit: Option<(i32, i32, bool, bool)> = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                if locked {
                    ui.add_space(4.0);
                    let frame = egui::Frame::new()
                        .fill(Color32::from_rgb(0xF6, 0xE3, 0xDC))
                        .inner_margin(egui::Margin::same(8))
                        .corner_radius(6);
                    frame.show(ui, |ui| {
                        ui.label(
                            RichText::new("! 存档正被游戏占用，写入将被拒绝")
                                .size(12.0)
                                .color(theme::DANGER),
                        );
                    });
                    ui.add_space(8.0);
                }

                field(ui, "存档名称", &tab.edit_name, |_| {});
                info_row(ui, "游戏版本", &tab.world.level.version_name());
                info_row(ui, "数据版本", &tab.world.level.data_version().to_string());
                info_row(
                    ui,
                    "种子",
                    &tab
                        .world
                        .seed()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".into()),
                );

                // ---- 可点选：游戏模式 ----
                let mut gametype = tab.edit_gametype;
                combo_row(ui, "游戏模式", &mut gametype, &[
                    (0, "生存"), (1, "创造"), (2, "冒险"), (3, "旁观"),
                ]);

                // ---- 可点选：游戏难度 ----
                let mut difficulty = tab.edit_difficulty;
                combo_row(ui, "游戏难度", &mut difficulty, &[
                    (0, "和平"), (1, "简单"), (2, "普通"), (3, "困难"),
                ]);

                // ---- 可点选：作弊 ----
                let mut cheats = tab.edit_cheats;
                combo_bool_row(ui, "作弊", &mut cheats);

                // ---- 可点选：硬核 ----
                let mut hardcore = tab.edit_hardcore;
                combo_bool_row(ui, "硬核", &mut hardcore);

                // 写回编辑缓冲留到滚动区外统一处理，避免与 tab 借用冲突
                combo_edit = Some((gametype, difficulty, cheats, hardcore));

                // ---- 可输入：出生点 ----
                let mut spawn = tab.spawn_buf;
                ui.label(theme::section_title("出生点"));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut spawn.0).speed(1.0).prefix("X "));
                    ui.add(egui::DragValue::new(&mut spawn.1).speed(1.0).prefix("Y ").range(-64..=320));
                    ui.add(egui::DragValue::new(&mut spawn.2).speed(1.0).prefix("Z "));
                });
                ui.add_space(8.0);

                // ---- 可输入：玩家坐标 ----
                let mut player = tab.player_buf;
                ui.label(theme::section_title("玩家坐标"));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut player.0).speed(1.0).prefix("X "));
                    ui.add(egui::DragValue::new(&mut player.1).speed(1.0).prefix("Y "));
                    ui.add(egui::DragValue::new(&mut player.2).speed(1.0).prefix("Z "));
                });
                // 玩家坐标改完立刻把地图跳到该处
                if player != tab.player_prev {
                    pending_goto = Some((player.0 as f32, player.2 as f32));
                }
                ui.add_space(8.0);

                // 记录本帧的输入变化
                spawn_edit = Some(spawn);
                player_edit = Some(player);

                ui.add_space(10.0);
                ui.label(theme::section_title("维度"));
                ui.add_space(4.0);

                let dims: Vec<(String, String)> = tab
                    .world
                    .dimensions
                    .iter()
                    .map(|d| (d.id.clone(), d.display.clone()))
                    .collect();
                let mut switch: Option<usize> = None;
                for (i, (_id, display)) in dims.iter().enumerate() {
                    let active = i == tab.current_dim;
                    let label = format!("{display}");
                    let btn = egui::Button::new(
                        RichText::new(label).color(if active { theme::TEXT } else { theme::TEXT_DIM }),
                    )
                    .fill(if active { theme::ACCENT_SOFT } else { theme::FIELD })
                    .min_size(Vec2::new(ui.available_width(), 26.0));
                    if ui.add(btn).clicked() {
                        switch = Some(i);
                    }
                }
                if let Some(i) = switch {
                    pending_switch = Some(i);
                }

                ui.add_space(10.0);
                ui.label(theme::section_title("统计"));
                ui.add_space(4.0);

                let chunk_count = tab.chunks_of_current().len();
                let full = tab.chunks_of_current().iter().filter(|c| c.is_full()).count();
                let ent_count = tab.entities_of_current().len();
                let be = tab
                    .block_entities
                    .get(&tab.dim_id())
                    .map(|(n, _)| *n)
                    .unwrap_or(0);

                if chunk_count == 0 {
                    ui.label(RichText::new("正在扫描…").size(12.0).color(theme::TEXT_DIM));
                } else {
                    info_row(ui, "区块", &format!("{chunk_count}（完整 {full}）"));
                    info_row(ui, "未生成", &format!("{}", chunk_count - full));
                    info_row(ui, "实体", &ent_count.to_string());
                    info_row(ui, "方块实体", &be.to_string());
                }

                // SwiftMap 小地图模组数据：有就用它渲染地图，比扫描存档快得多
                if let Some(sm) = &tab.swiftmap {
                    ui.add_space(10.0);
                    ui.label(theme::section_title("SwiftMap 小地图"));
                    let dim_id = tab.dim_id();
                    if let Some(d) = sm.dimensions.iter().find(|d| d.dim_id == dim_id) {
                        info_row(ui, "本维度容器", &d.containers.len().to_string());
                        if let Some(v) = d.format_version {
                            info_row(ui, "格式版本", &v.to_string());
                        }
                        if let Some(p) = &d.producer {
                            ui.label(
                                RichText::new(p).size(11.0).color(theme::TEXT_DIM),
                            );
                        }
                    } else {
                        info_row(ui, "本维度", "无数据");
                    }
                }

                if let Some(cb) = &app.clipboard {
                    ui.add_space(10.0);
                    ui.label(theme::section_title("剪贴板"));
                    info_row(ui, "区块数", &cb.chunk_count().to_string());
                    info_row(ui, "方块实体", &cb.block_entities.to_string());
                    info_row(ui, "实体", &cb.entities.to_string());
                    info_row(ui, "来源", &cb.source_dimension);
                }
            });

            // 侧边栏绘制完成，借用结束，此时再执行操作
            if let Some(i) = pending_switch {
                if let Some(tab) = app.tabs.get_mut(app.active) {
                    // 只切换"当前维度"，不动任何已加载的数据。
                    // 各维度的地图纹理、相机、选区、片区都各自保存，切回来原样恢复。
                    tab.current_dim = i;
                    tab.ensure_view(&tab.dim_id());
                }
                pending_scan = true;
                pending_sample = true;
            }
            if pending_scan {
                app.request_scan();
            }
            // 该维度若已扫描过（只是地图被清空），直接重新采样，不必再扫一遍
            if pending_sample {
                app.request_sample();
            }

            // 枚举字段的输入回写
            if let Some((gt, df, ch, hc)) = combo_edit {
                if let Some(tab) = app.tabs.get_mut(app.active) {
                    if tab.edit_gametype != gt {
                        tab.edit_gametype = gt;
                        tab.dirty_level = true;
                    }
                    if tab.edit_difficulty != df {
                        tab.edit_difficulty = df;
                        tab.dirty_level = true;
                    }
                    if tab.edit_cheats != ch {
                        tab.edit_cheats = ch;
                        tab.dirty_level = true;
                    }
                    if tab.edit_hardcore != hc {
                        tab.edit_hardcore = hc;
                        tab.dirty_level = true;
                    }
                }
            }

            // 出生点 / 玩家坐标的输入回写
            if let Some(sp) = spawn_edit {
                if let Some(tab) = app.tabs.get_mut(app.active) {
                    if tab.edit_spawn != sp {
                        tab.edit_spawn = sp;
                        tab.spawn_buf = sp;
                        tab.dirty_level = true;
                    }
                }
            }
            if let Some(pl) = player_edit {
                if let Some(tab) = app.tabs.get_mut(app.active) {
                    if tab.player_buf != pl {
                        tab.player_buf = pl;
                        tab.player_prev = pl;
                    }
                }
            }

            // 跳到指定坐标
            if let Some((gx, gz)) = pending_goto {
                if let Some(tab) = app.tabs.get_mut(app.active) {
                    let v = tab.view_mut();
                    v.camera.center = Vec2::new(gx, gz);
                    if v.camera.scale < 0.5 {
                        v.camera.scale = 1.0;
                    }
                }
            }

            if let Some(a) = pending_confirm {
                app.pending_confirm = Some(a);
            }
        });
}

/// 一行「标签 + 下拉选择」，用于布尔字段。
fn combo_bool_row(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    ui.label(theme::section_title(label));
    ui.add_space(2.0);
    let current = if *value { "开启" } else { "关闭" };

    egui::ComboBox::from_id_salt(format!("combo-b-{label}"))
        .selected_text(RichText::new(current).color(theme::TEXT))
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, false, "关闭");
            ui.selectable_value(value, true, "开启");
        });
    ui.add_space(8.0);
}

/// 一行「标签 + 下拉选择」，用于枚举类字段（游戏模式 / 难度 / 作弊 / 硬核）。
fn combo_row(ui: &mut egui::Ui, label: &str, value: &mut i32, options: &[(i32, &str)]) {
    ui.label(theme::section_title(label));
    ui.add_space(2.0);
    let current = options
        .iter()
        .find(|(v, _)| *v == *value)
        .map(|(_, t)| *t)
        .unwrap_or("—");

    egui::ComboBox::from_id_salt(format!("combo-{label}"))
        .selected_text(RichText::new(current).color(theme::TEXT))
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            for (v, text) in options {
                ui.selectable_value(value, *v, *text);
            }
        });
    ui.add_space(8.0);
}

fn field(ui: &mut egui::Ui, label: &str, value: &str, _edit: impl FnOnce(&str)) {
    ui.label(theme::section_title(label));
    ui.add_space(2.0);
    let mut s = value.to_string();
    ui.add(
        egui::TextEdit::singleline(&mut s)
            .desired_width(ui.available_width())
            .font(egui::TextStyle::Body),
    );
    ui.add_space(8.0);
}

fn info_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(theme::section_title(label));
    ui.add_space(2.0);
    let frame = egui::Frame::new()
        .fill(theme::FIELD)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .corner_radius(6)
        .stroke(egui::Stroke::new(1.0, theme::BORDER));
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::value_text(value));
    });
    ui.add_space(8.0);
}

pub fn game_type_label(v: i32) -> &'static str {
    match v {
        0 => "生存",
        1 => "创造",
        2 => "冒险",
        3 => "旁观",
        _ => "未知",
    }
}

pub fn difficulty_label(v: i32) -> &'static str {
    match v {
        0 => "和平",
        1 => "简单",
        2 => "普通",
        3 => "困难",
        _ => "未知",
    }
}

/// 中央地图区域。
pub fn map_area(app: &mut App, ctx: &egui::Context) {
    // 本帧的悬停结果，绘制结束后再写回 app（避免与 tab 的借用冲突）
    let mut hover_out: Option<crate::HoverInfo> = None;
    // 加载提示的剩余时间，绘制完地图后叠在画布上
    let hint_left = app.hint_remaining;
    let mut show_about = false;
    // 本帧是否需要为视口补加载区块
    let mut viewport_needs_load = false;
    // 本帧的地图画布矩形（用于视口计算）
    let mut map_viewport: Option<egui::Rect> = None;
    let mut paste_done = false;
    let mut paste_cancelled = false;

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(12)))
        .show(ctx, |ui| {
            if app.tabs.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("打开一个存档开始编辑")
                            .size(16.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                return;
            }

            let dim_display = app
                .tab()
                .and_then(|t| t.dim().map(|d| d.display.clone()))
                .unwrap_or_default();

            // ---- 单行工具栏：维度标题 + 网格 + 跳转 + 渲染高度 + 缩放控制 ----
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{dim_display}"))
                        .size(14.0)
                        .color(theme::TEXT),
                );
                ui.separator();

                // 网格开关
                let grid_on = app.show_grid;
                let label = if grid_on { "▣ 网格：开" } else { "▢ 网格：关" };
                let btn = egui::Button::new(
                    RichText::new(label)
                        .size(12.0)
                        .color(if grid_on { Color32::WHITE } else { theme::TEXT }),
                )
                .fill(if grid_on { theme::ACCENT } else { theme::FIELD })
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .min_size(Vec2::new(84.0, 22.0));
                if ui.add(btn).on_hover_text("显示 / 隐藏区块边界网格").clicked() {
                    app.show_grid = !app.show_grid;
                }

                // 跳转
                ui.label(RichText::new("跳转").size(11.0).color(theme::TEXT_DIM));
                let mut gx = app.goto_x.clone();
                let mut gz = app.goto_z.clone();
                ui.add(
                    egui::TextEdit::singleline(&mut gx)
                        .hint_text("X")
                        .desired_width(58.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut gz)
                        .hint_text("Z")
                        .desired_width(58.0),
                );
                app.goto_x = gx;
                app.goto_z = gz;
                let parse = |s: &str| s.trim().parse::<f32>().ok();
                if ui.small_button("跳转").clicked() {
                    if let (Some(x), Some(z)) = (parse(&app.goto_x), parse(&app.goto_z)) {
                        if let Some(tab) = app.tabs.get_mut(app.active) {
                            let v = tab.view_mut();
                            v.camera.center = Vec2::new(x, z);
                            if v.camera.scale < 0.5 {
                                v.camera.scale = 1.5;
                            }
                        }
                    } else {
                        app.set_status("请输入有效的 X / Z 坐标", crate::StatusKind::Error);
                    }
                }
                if ui.small_button("填入出生点").clicked() {
                    if let Some(tab) = app.tab() {
                        let (sx, _sy, sz) = tab.world.level.spawn();
                        app.goto_x = sx.to_string();
                        app.goto_z = sz.to_string();
                    }
                }

                // 渲染高度
                ui.label(RichText::new("高度").size(11.0).color(theme::TEXT_DIM));
                let dim_id = app.tab().map(|t| t.dim_id()).unwrap_or_default();
                let is_nether = dim_id == "minecraft:the_nether";
                let mut mode = if app.render_y.is_some() { 1 } else { 0 };
                ui.selectable_value(
                    &mut mode,
                    0,
                    if is_nether { "默认 Y65" } else { "默认 地表" },
                );
                ui.selectable_value(&mut mode, 1, "自定义");
                if mode == 1 {
                    let mut y = app.render_y.unwrap_or(if is_nether { 65 } else { 64 });
                    let changed = ui
                        .add(
                            egui::DragValue::new(&mut y)
                                .speed(1.0)
                                .range(-64..=319)
                                .prefix("Y "),
                        )
                        .changed();
                    if changed || app.render_y.is_none() {
                        app.render_y = Some(y);
                    }
                    if ui.small_button("应用").clicked() {
                        app.resample_current();
                    }
                } else if app.render_y.is_some() {
                    app.render_y = None;
                    app.resample_current();
                }

                // 右侧：缩放 / 定位
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let Some(tab) = app.tabs.get_mut(app.active) else { return };
                    if ui.small_button("回到玩家").clicked() {
                        if let Some(p) = tab.world.player_pos() {
                            let v = tab.view_mut();
                            v.camera.center = Vec2::new(p[0] as f32, p[2] as f32);
                            v.camera.scale = 2.0;
                        }
                    }
                    if ui.small_button("回到出生点").clicked() {
                        let (sx, _sy, sz) = tab.world.level.spawn();
                        let v = tab.view_mut();
                        v.camera.center = Vec2::new(sx as f32, sz as f32);
                        if v.camera.scale < 0.5 {
                            v.camera.scale = 1.5;
                        }
                    }
                    ui.label(
                        RichText::new(format!("{:.2}×", tab.view().camera.scale))
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
            ui.add_space(4.0);

            // 只有真正「连地形都还没有」的区块才值得提示。
            // 注意：`structure_starts` 之类虽然 !is_full()，但地形可能已经生成好
            // （它们的方块数据是完整的），压暗显示反而会误导。这里只统计
            // 方块数据为空 / 只有基岩/群系的阶段。
            {
                let (total, empty) = app
                    .tab()
                    .map(|t| {
                        let all = t.chunks_of_current();
                        let e = all
                            .iter()
                            .filter(|c| {
                                let s = c.status_short();
                                matches!(s, "biomes" | "empty" | "structure_starts")
                                    && c.nbt_bytes < 2048
                            })
                            .count();
                        (all.len(), e)
                    })
                    .unwrap_or((0, 0));
                if empty > 0 && total > 0 {
                    let pct = empty as f64 / total as f64 * 100.0;
                    let frame = egui::Frame::new()
                        .fill(Color32::from_rgb(0x2C, 0x33, 0x3A))
                        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(0x55, 0x60, 0x6E)))
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .corner_radius(4.0);
                    frame.show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{empty} / {total} 个区块没有任何地形数据（{pct:.0}%），\
                                 地图上显示为深灰底色 —— 这些区块在游戏里还没开始生成地形"
                            ))
                            .size(11.0)
                            .color(Color32::from_rgb(0xB8, 0xC2, 0xCE)),
                        );
                    });
                    ui.add_space(4.0);
                }
            }

            let player = app.tab().and_then(|t| t.world.player_pos());
            let spawn = app.tab().map(|t| t.world.level.spawn());
            // 粘贴定位模式：把剪贴板尺寸传给地图，用于画红色预览框
            let paste_size = if app.paste_mode {
                app.clipboard.as_ref().map(|cb| {
                    let (a, b, c, d) = cb.bounds();
                    (c - a + 1, d - b + 1)
                })
            } else {
                None
            };
            let confirmed = app
                .tab()
                .map(|t| t.view().selections.clone())
                .unwrap_or_default();
            let opts = map_view::MapOptions {
                show_grid: app.show_grid,
                paste_size,
                confirmed,
            };

            // 记录地图画布在本帧的像素矩形，供"按视口加载区块"使用。
            map_viewport = Some(ui.available_rect_before_wrap());

            let Some(tab) = app.tabs.get_mut(app.active) else { return };
            // 同一个 DimView 里同时读 map、写 camera/selection：
            // 先取出一次可变引用，再拆成字段借用，避免重复借用冲突。
            let v = tab.view_mut();
            let resp = map_view::draw(
                ui,
                &v.map,
                &mut v.camera,
                &mut v.selection,
                player,
                spawn,
                opts,
            );

            // 记录画布尺寸：fit_all 要按真实宽高比例算缩放
            v.canvas_size = Some((resp.canvas_w, resp.canvas_h));

            // ---- "加载中"提示（画在画布右上角）----
            {
                let loading = v.loading;
                let pending_n = v.pending.len();
                if loading && pending_n > 0 {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(150));
                    let painter = ui.painter();
                    let vp = ui.max_rect();
                    let text = format!("加载中…  剩余 {pending_n} 个区块");
                    let galley = painter.layout_no_wrap(
                        text,
                        egui::FontId::proportional(13.0),
                        Color32::from_rgba_unmultiplied(0xFF, 0xE8, 0xB0, 235),
                    );
                    let pad = Vec2::new(12.0, 7.0);
                    let sz = galley.size() + pad * 2.0;
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(vp.right() - sz.x - 14.0, vp.top() + 14.0),
                        sz,
                    );
                    painter.rect_filled(
                        rect,
                        6.0,
                        Color32::from_rgba_unmultiplied(0x2A, 0x24, 0x18, 225),
                    );
                    painter.rect_stroke(
                        rect,
                        6.0,
                        egui::Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(0xC8, 0xA8, 0x60, 220),
                        ),
                        egui::StrokeKind::Middle,
                    );
                    painter.galley(rect.min + pad, galley, Color32::WHITE);
                }
            }

            // 决定本帧要不要再为视口补载区块。
            //
            // 三种情况需要补：
            //   1. 相机中心移动超过 2 个区块（拖动 / 跳转）
            //   2. 缩放比例变了 —— 缩小之后视野内会露出大量没加载的区块，
            //      而中心没动。早期只看中心位移，缩小时完全不补载，
            //      表现就是"缩小后地图空着，转半天也不出来"。
            //   3. 上一批已经快处理完，但还没加载完 —— 否则一次最多只排
            //      1024 个，剩下的永远等不到下一次触发。
            {
                let cam_center = v.camera.center;
                let cur = (
                    (cam_center.x / 16.0).floor() as i32,
                    (cam_center.y / 16.0).floor() as i32,
                );
                let moved = match v.last_view_center {
                    Some(prev) => (prev.0 - cur.0).abs() >= 2 || (prev.1 - cur.1).abs() >= 2,
                    None => true,
                };
                // 缩放变化超过 1% 就认为视野范围变了
                let zoom_changed = match v.last_view_scale {
                    Some(prev) => ((prev - v.camera.scale) / prev.max(1e-6)).abs() > 0.01,
                    None => true,
                };
                // 队列快空了但还在加载 → 补下一批（阈值留出余量，避免频繁触发）
                let top_up = v.loading && v.pending.len() < 256;

                if moved || zoom_changed || top_up {
                    v.last_view_center = Some(cur);
                    v.last_view_scale = Some(v.camera.scale);
                    viewport_needs_load = true;
                }
            }

            // 粘贴定位：左键确认 → 弹出确认框；右键取消
            if let Some((ax, az)) = resp.paste_confirm {
                if let Some(cb) = app.clipboard.as_ref() {
                    app.pending_confirm = Some(ConfirmAction::PasteChunks {
                        count: cb.chunk_count(),
                        dim: format!("({ax}, {az})"),
                        target: (ax, az),
                    });
                }
                app.paste_mode = false;
                app.paste_anchor = None;
                paste_done = true;
            }
            if resp.paste_cancelled && app.paste_mode {
                app.paste_mode = false;
                app.paste_anchor = None;
                paste_cancelled = true;
            }
            if app.paste_mode {
                app.paste_anchor = resp.paste_anchor;
            }

            // 点击地图 → 设为实体删除中心
            if let Some((wx, wz)) = resp.clicked_world {
                tab.filter_center = Some((wx, 64.0, wz));
                // 单击地图（左键或右键）：
                //   多选模式 → 取消**上一个**已确认的选区
                //   单选模式 → 清空当前选区
                // 正在拖拽框选时不处理，避免刚开始拖就清掉。
                if resp.clicked_button.is_some() && !resp.selecting {
                    let v = tab.view_mut();
                    if v.multi_select && !v.selections.is_empty() {
                        v.selections.pop();
                    } else {
                        v.selection = None;
                    }
                }
            }

            // ---- 多选模式：拖拽结束后把当前选区「确认」进列表 ----
            if resp.selecting_ended {
                let v = tab.view_mut();
                if v.multi_select {
                    if let Some(sel) = v.selection.take() {
                        v.selections.push(sel);
                    }
                }
            }

            // ---- 加载提示：叠加在地图画布内部（不是按钮栏）----
            if hint_left > 0.0 {
                let a = (hint_left.min(1.0) * 235.0) as u8;
                let painter = ui.painter();
                let area = ui.max_rect();
                let galley = painter.layout_no_wrap(
                    "编辑存档有风险，请谨慎操作".to_string(),
                    egui::FontId::proportional(13.0),
                    Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, a),
                );
                let pad = Vec2::new(14.0, 8.0);
                let sz = galley.size() + pad * 2.0;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(area.center().x - sz.x * 0.5, area.top() + 14.0),
                    sz,
                );
                painter.rect_filled(
                    rect,
                    6.0,
                    Color32::from_rgba_unmultiplied(0x24, 0x2C, 0x1E, a.saturating_sub(20)),
                );
                painter.rect_stroke(
                    rect,
                    6.0,
                    egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x8A, 0xA8, 0x62, a)),
                    egui::StrokeKind::Middle,
                );
                painter.galley(rect.min + pad, galley, Color32::WHITE);
            }
            // 悬停 → 记录坐标 / 区块（群系在状态栏按需补）
            hover_out = resp.hover_world.map(|(wx, wz)| {
                let bx = wx.floor() as i32;
                let bz = wz.floor() as i32;
                crate::HoverInfo {
                    x: bx,
                    z: bz,
                    chunk_x: bx.div_euclid(16),
                    chunk_z: bz.div_euclid(16),
                    block: None,
                    block_y: 0,
                    biome: None,
                }
            });
        });

        app.hover_info = hover_out;
        if paste_done {
            app.set_status("已选择粘贴位置，请确认", StatusKind::Info);
        }
        if paste_cancelled {
            app.set_status("已取消粘贴定位", StatusKind::Info);
        }
        if show_about {
            app.show_about = true;
        }

        // 视口内还有没采样的区块 → 排进后台队列。
        // 这里放在绘制之后，拿到的 viewport 才是本帧真实尺寸。
        if viewport_needs_load {
            app.request_viewport_sample(map_viewport);
        }
}

fn fit_all(tab: &mut WorldTab) {
    // 先算范围（借用不可变），再改相机（借用可变）——
    // 不能边持有 chunks 切片边 view_mut()。
    // ★ 视野取**最大的那片连通区域**，而不是全部区块的外接矩形。
    //
    //   实测主世界的分布是这样的：
    //     94% 的区块在 X -192..191（主要活动区）
    //      6% 在 X 1728..1919（远处零散探索点），两者相距约 1800 区块。
    //
    //   按全部区块的外接矩形算，视野会被那 6% 撑大十倍，
    //   主体只能占画布左边一小块 —— 用户点「全图」想看的是主体。
    //
    //   改用"最大连通片区"的包围盒：主体铺满画布，远处的零散点
    //   落在视野外（想看的话拖过去，视口加载会补上）。
    //   没有片区信息时退回全部区块的外接矩形。
    let (min_x, max_x, min_z, max_z) = {
        let chunks = tab.chunks_of_current();
        if chunks.is_empty() {
            return;
        }

        // 优先用最大片区
        if let Some(best) = tab
            .view()
            .regions
            .iter()
            .max_by_key(|r| r.chunk_count)
        {
            let (bx0, bz0, bx1, bz1) = best.bounds;
            // 片区可能只覆盖主体的一部分，稍微外扩一点更自然
            (bx0, bx1, bz0, bz1)
        } else {
            let mut xs: Vec<i32> = chunks.iter().map(|c| c.x).collect();
            let mut zs: Vec<i32> = chunks.iter().map(|c| c.z).collect();
            xs.sort_unstable();
            zs.sort_unstable();
            (xs[0], xs[xs.len() - 1], zs[0], zs[zs.len() - 1])
        }
    };
    let cx = (min_x + max_x) as f32 * 8.0 + 8.0;
    let cz = (min_z + max_z) as f32 * 8.0 + 8.0;

    // ★ 宽高分别算，取更小的缩放，才能保证两个方向都装得下。
    //
    //   早期只用一个 `span = max(横向, 纵向)` 配一个硬编码的画布宽度，
    //   问题有两个：
    //     1. 画布不是正方形，横向 1200、纵向 800，用同一个边长必然有
    //        一边溢出（实测全图后左侧内容被切掉）；
    //     2. 硬编码 1500 与真实宽度差 300，缩放偏小约 25%，
    //        叠加起来就会"以为缩到全图了，其实只看得到一角"。
    //
    //   这里按实际地图区尺寸算。取不到真实尺寸时退回 1200×800（窗口
    //   默认大小下的地图区），误差可接受。
    let (canvas_w, canvas_h) = tab
        .view()
        .canvas_size
        .unwrap_or((1200.0, 800.0));

    let span_x = ((max_x - min_x + 1) as f32 * 16.0).max(32.0);
    let span_z = ((max_z - min_z + 1) as f32 * 16.0).max(32.0);
    let scale_x = canvas_w / span_x;
    let scale_z = canvas_h / span_z;
    let scale = scale_x.min(scale_z).clamp(0.002, 4.0);

    let v = tab.view_mut();
    v.camera.center = Vec2::new(cx, cz);
    v.camera.scale = scale;
}

/// 底部状态栏。
pub fn status_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status")
        .frame(
            egui::Frame::new()
                .fill(theme::TOPBAR)
                .inner_margin(egui::Margin::symmetric(12, 5))
                .stroke(egui::Stroke::new(1.0, theme::BORDER)),
        )
        .show(ctx, |ui| {
            // 鼠标在地图上时，把坐标/区块/群系补全后显示在最前面。
            // 群系按需读取（只在区块变化时读一次，避免每帧解压）。
            if let Some(info) = app.hover_info.clone() {
                let need_lookup = info.biome.is_none();
                if need_lookup {
                    if let Some(filled) = lookup_hover(app, &info) {
                        app.hover_info = Some(filled);
                    }
                }
            }

            ui.horizontal(|ui| {
                if let Some(info) = app.hover_info.clone() {
                    ui.label(
                        RichText::new(info.to_line())
                            .size(12.0)
                            .color(theme::TEXT)
                            .monospace(),
                    );
                    ui.separator();
                }

                let color = match app.status_kind {
                    StatusKind::Info => theme::TEXT_DIM,
                    StatusKind::Success => Color32::from_rgb(0x4A, 0x7A, 0x3C),
                    StatusKind::Error => theme::DANGER,
                };
                ui.label(RichText::new(&app.status).size(12.0).color(color));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let busy = app.jobs.labels();
                    if busy.is_empty() {
                        ui.label(
                            RichText::new("空闲")
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                        );
                    } else {
                        for (label, p) in busy.iter().take(2) {
                            let frac = if p.total > 0 {
                                p.done as f32 / p.total as f32
                            } else {
                                0.0
                            };
                            // 进度条上要能看出"做到哪了"：
                            //   - 有阶段性 message（如 "主世界 1234/5678"）就优先显示它
                            //   - 再补上百分比
                            //   以前只显示任务名，"导出全图 HTML" 挂了十几分钟
                            //   却看不出到底在动还是卡死了。
                            let text = if p.message.is_empty() {
                                format!("{label}")
                            } else if p.total > 0 {
                                format!("{}  {:.0}%", p.message, frac * 100.0)
                            } else {
                                p.message.clone()
                            };
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(220.0)
                                    .text(text),
                            );
                        }
                    }
                });
            });
        });
}

/// 补全悬停信息里的方块与群系（按当前维度与渲染高度取）。
fn lookup_hover(app: &mut App, info: &crate::HoverInfo) -> Option<crate::HoverInfo> {
    let mode = app.slice_mode();
    let (lx, lz) = (
        info.x.rem_euclid(16) as usize,
        info.z.rem_euclid(16) as usize,
    );
    let (cx, cz) = (info.chunk_x, info.chunk_z);

    // 方块名与高度：按需从磁盘读这一个区块。
    // 缩略图里不再保留 256 列原始数据（那是"卸载区块数据"的关键），
    // 所以这里查一次、缓存一次，鼠标在同一区块里移动不会重复读盘。
    let tab = app.tab_mut()?;
    let hit = tab.column_on_demand(cx, cz, lx, lz);
    let dim = tab.dim()?.clone();

    let (block, block_y) = match hit {
        Some((b, by)) => (Some(b), by),
        None => (None, match mode {
            mcworld_core::chunk::SliceMode::Fixed(v) => v,
            mcworld_core::chunk::SliceMode::Surface => 64,
        }),
    };

    // 群系
    let mut cache = mcworld_core::world::RegionCache::new(2);
    let biome = tab.world.biome_at(&dim, info.x, block_y, info.z, &mut cache);

    Some(crate::HoverInfo {
        x: info.x,
        z: info.z,
        chunk_x: info.chunk_x,
        chunk_z: info.chunk_z,
        block,
        block_y,
        biome,
    })
}

/// 各种弹窗。
/// 「关于」窗口：版本、作者与主页链接。
fn about_window(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_about;

    egui::Window::new("关于")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // 标题行
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Minecraft 存档管理器")
                        .size(17.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                        .size(13.0)
                        .color(theme::ACCENT),
                );
            });
            ui.label(
                RichText::new("正式版")
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);

            // 作者
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("作者")
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("黑山大叔").size(13.0).color(theme::TEXT));
            });

            ui.add_space(6.0);

            // 主页（可点击）
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("主页")
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                ui.hyperlink_to(
                    RichText::new("space.bilibili.com/19428259")
                        .size(12.0)
                        .color(theme::ACCENT),
                    "https://space.bilibili.com/19428259",
                );
            });

            ui.add_space(6.0);

            // GitHub（可点击）
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("GitHub")
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                ui.hyperlink_to(
                    RichText::new("github.com/heiheids")
                        .size(12.0)
                        .color(theme::ACCENT),
                    "https://github.com/heiheids",
                );
            });

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("打开主页").clicked() {
                    // egui 的 open_url 在 eframe 下会交给系统浏览器
                    ctx.open_url(egui::OpenUrl::new_tab(
                        "https://space.bilibili.com/19428259",
                    ));
                }
                if ui.button("打开 GitHub").clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab("https://github.com/heiheids"));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("关闭").clicked() {
                        app.show_about = false;
                    }
                });
            });

            ui.add_space(4.0);
        });

    // 标题栏的 × 也要同步回状态
    if !open {
        app.show_about = false;
    }
}

pub fn windows(app: &mut App, ctx: &egui::Context) {
    if app.show_entities {
        entity_window(app, ctx);
    }
    if app.show_chunks {
        chunk_window(app, ctx);
    }
    if app.show_gamerules {
        gamerules_window(app, ctx);
    }
    if app.show_regions {
        regions_window(app, ctx);
    }
    if app.show_about {
        about_window(app, ctx);
    }

    if app.show_export_html {
        export_html_window(app, ctx);
    }

    // 导出进行中：浮窗显示阶段和进度（状态栏那个太小，容易看不见）
    export_progress_window(app, ctx);
    if app.show_cache_prompt {
        cache_prompt_window(app, ctx);
    }

    // 危险操作确认
    if let Some(action) = app.pending_confirm.take() {
        let mut confirmed = false;
        let mut cancelled = false;

        let (title, desc) = match &action {
            ConfirmAction::DeleteChunks {
                count,
                dim,
                targets,
            } => {
                let ranges: Vec<String> = targets
                    .iter()
                    .take(6)
                    .map(|(a, b, c, d)| format!("({a},{b}) → ({c},{d})"))
                    .collect();
                let mut lines = ranges.join("\n");
                if targets.len() > 6 {
                    lines.push_str(&format!("\n… 还有 {} 处", targets.len() - 6));
                }
                (
                    "确认删除区块".to_string(),
                    format!(
                        "将删除 {dim} 的 {count} 个区块，共 {} 处选区。\n范围：\n{lines}\n\n区块内的方块、方块实体、实体与 POI 都会一并清除。\n\n操作前会自动备份，之后可用工具栏的「撤销」还原。",
                        targets.len()
                    ),
                )
            }
            ConfirmAction::DeleteEntities { count, dim } => (
                "确认删除实体".to_string(),
                format!("将从 {dim} 删除 {count} 个匹配的实体。\n\n操作前会自动备份。"),
            ),
            ConfirmAction::DeleteChunkEntities {
                chunk_x,
                chunk_z,
                count,
                dim,
            } => (
                "确认删除区块内实体".to_string(),
                format!(
                    "将删除 {dim} 区块 ({chunk_x}, {chunk_z}) 内的 {count} 个实体。\n\
                     该区块的方块与地形不受影响。\n\n操作前会自动备份。"
                ),
            ),
            ConfirmAction::PasteChunks { count, dim, .. } => (
                "确认粘贴".to_string(),
                format!("将把剪贴板中的 {count} 个区块粘贴到 {dim}，覆盖原有区块。\n\n操作前会自动备份。"),
            ),
            ConfirmAction::SaveLevel => (
                "确认保存 level.dat".to_string(),
                "将把左侧表单的修改写入 level.dat。\n原文件会先备份为 level.dat_old。".to_string(),
            ),
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label(desc);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(
                            RichText::new("取消").color(theme::TEXT),
                        ).fill(theme::FIELD))
                        .clicked()
                    {
                        cancelled = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                RichText::new("确认执行").color(Color32::WHITE),
                            ).fill(theme::DANGER))
                            .clicked()
                        {
                            confirmed = true;
                        }
                    });
                });
            });

        if confirmed {
            execute_confirmed(app, action);
        } else if !cancelled {
            // 用户还没决定，把动作放回
            app.pending_confirm = Some(action);
        }
    }
}

fn execute_confirmed(app: &mut App, action: ConfirmAction) {
    match action {
        ConfirmAction::SaveLevel => {
            let Some(tab) = app.tabs.get_mut(app.active) else { return };
            let label = format!("修改 level.dat（{}）", tab.edit_name);
            match save_level(tab) {
                Ok((msg, backup_dir)) => {
                    app.last_backup = Some(mcworld_core::safety::LastBackup {
                        dir: backup_dir,
                        label,
                    });
                    app.set_status(msg, StatusKind::Success);
                }
                Err(e) => app.set_status(format!("保存失败: {e}"), StatusKind::Error),
            }
        }
        ConfirmAction::DeleteChunks { count, targets, .. } => {
            let Some(tab) = app.tab() else { return };
            // 用确认框弹出时的快照坐标（可能有多处），不再依赖当前选区
            let mut chunks = Vec::new();
            for (x0, z0, x1, z1) in &targets {
                for cx in *x0..=*x1 {
                    for cz in *z0..=*z1 {
                        chunks.push((cx, cz));
                    }
                }
            }
            // 多个选区可能有重叠，去重后才能得到正确的"实际删除数"
            chunks.sort_unstable();
            chunks.dedup();
            let Some(dim) = tab.dim().cloned() else { return };
            let root = tab.path.clone();
            let n_sel = targets.len();
            app.jobs.spawn_delete_chunks(root, dim, chunks, true, true);
            if n_sel > 1 {
                app.set_status(
                    format!("正在删除 {n_sel} 处选区的 {count} 个区块…"),
                    StatusKind::Info,
                );
            } else {
                app.set_status(format!("正在删除 {count} 个区块…"), StatusKind::Info);
            }
        }
        ConfirmAction::DeleteEntities { count, .. } => {
            let Some(tab) = app.tab() else { return };
            let filter = build_filter(tab);
            let Some(dim) = tab.dim().cloned() else { return };
            let root = tab.path.clone();
            app.jobs
                .spawn_delete_entities(root, dim, filter, format!("删除 {count} 个实体"));
            app.set_status(format!("正在删除 {count} 个实体…"), StatusKind::Info);
        }
        ConfirmAction::DeleteChunkEntities {
            chunk_x,
            chunk_z,
            count,
            ..
        } => {
            let Some(tab) = app.tab() else { return };
            // 只针对这一个区块，且不限类型
            let mut filter = mcworld_core::edit::EntityFilter::default();
            filter.chunks.insert((chunk_x, chunk_z));
            let Some(dim) = tab.dim().cloned() else { return };
            let root = tab.path.clone();
            app.jobs.spawn_delete_entities(
                root,
                dim,
                filter,
                format!("删除区块 ({chunk_x},{chunk_z}) 的实体"),
            );
            app.set_status(
                format!("正在删除区块 ({chunk_x}, {chunk_z}) 的 {count} 个实体…"),
                StatusKind::Info,
            );
        }
        ConfirmAction::PasteChunks { count, target, .. } => {
            let Some(tab) = app.tab() else { return };
            let Some(cb) = app.clipboard.clone() else { return };
            // 目标位置由粘贴定位模式的光标位置决定
            let (x0, z0) = target;
            let Some(dim) = tab.dim().cloned() else { return };
            let root = tab.path.clone();
            match crate::export::paste(&root, &dim, &cb, x0, z0) {
                Ok(msg) => {
                    app.set_status(msg, StatusKind::Success);
                    app.refresh_current();
                }
                Err(e) => app.set_status(format!("粘贴失败: {e}"), StatusKind::Error),
            }
            let _ = count;
        }
    }
}

/// 把左侧/筛选控件的内容转成实体筛选条件。
pub fn build_filter(tab: &WorldTab) -> mcworld_core::edit::EntityFilter {
    let mut f = mcworld_core::edit::EntityFilter::default();
    if let Some(c) = tab.filter_center {
        f.center = Some(c);
        f.radius = tab.filter_radius;
    }
    for t in tab.filter_types.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        f.include_types.insert(mcworld_core::edit::normalize_id(t));
    }
    for t in tab.filter_exclude.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        f.exclude_types.insert(mcworld_core::edit::normalize_id(t));
    }
    f
}

/// 统计当前筛选命中数（在已扫描的索引上直接算，不读盘）。
pub fn count_matches(tab: &WorldTab, f: &mcworld_core::edit::EntityFilter) -> usize {
    tab.entities_of_current()
        .iter()
        .filter(|e| {
            if !f.include_types.is_empty() && !f.include_types.contains(&e.id) {
                return false;
            }
            if f.exclude_types.contains(&e.id) {
                return false;
            }
            if let (Some((cx, cy, cz)), Some(p)) = (f.center, e.pos) {
                let d2 = (p[0] - cx).powi(2) + (p[1] - cy).powi(2) + (p[2] - cz).powi(2);
                if d2 > f.radius * f.radius {
                    return false;
                }
            }
            if !tab.filter_text.is_empty() {
                let t = tab.filter_text.to_lowercase();
                if !e.id.to_lowercase().contains(&t) {
                    return false;
                }
            }
            true
        })
        .count()
}

fn entity_window(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_entities;
    egui::Window::new("实体管理")
        .open(&mut open)
        .default_size(Vec2::new(620.0, 520.0))
        .show(ctx, |ui| {
            let Some(tab) = app.tabs.get_mut(app.active) else {
                ui.label("请先打开存档");
                return;
            };

            ui.horizontal(|ui| {
                ui.label("中心坐标");
                let mut cx = tab.filter_center.map(|c| c.0).unwrap_or(0.0);
                let mut cy = tab.filter_center.map(|c| c.1).unwrap_or(64.0);
                let mut cz = tab.filter_center.map(|c| c.2).unwrap_or(0.0);
                let mut has_center = tab.filter_center.is_some();
                ui.checkbox(&mut has_center, "");
                ui.add(egui::DragValue::new(&mut cx).speed(1.0).prefix("X "));
                ui.add(egui::DragValue::new(&mut cy).speed(1.0).prefix("Y "));
                ui.add(egui::DragValue::new(&mut cz).speed(1.0).prefix("Z "));
                tab.filter_center = if has_center { Some((cx, cy, cz)) } else { None };
            });

            ui.horizontal(|ui| {
                ui.label("半径");
                ui.add(egui::DragValue::new(&mut tab.filter_radius).speed(1.0).range(1.0..=1000.0));
                ui.label("（在地图上点一下即可设定中心）");
            });

            ui.horizontal(|ui| {
                ui.label("包含类型");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.filter_types)
                        .hint_text("item, zombie（逗号分隔，留空=全部）")
                        .desired_width(280.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("排除类型");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.filter_exclude)
                        .hint_text("villager, armor_stand")
                        .desired_width(280.0),
                );
            });

            let filter = build_filter(tab);
            let matched = count_matches(tab, &filter);

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("命中 {matched} 个实体"))
                        .size(14.0)
                        .color(if matched > 0 { theme::DANGER } else { theme::TEXT_DIM }),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = matched > 0;
                    ui.add_enabled_ui(enabled, |ui| {
                        if ui
                            .add(egui::Button::new(
                                RichText::new(format!("删除这 {matched} 个")).color(Color32::WHITE),
                            ).fill(theme::DANGER))
                            .clicked()
                        {
                            let dim = tab.dim().map(|d| d.display.clone()).unwrap_or_default();
                            app.pending_confirm = Some(ConfirmAction::DeleteEntities {
                                count: matched,
                                dim,
                            });
                        }
                    });
                });
            });

            ui.add_space(6.0);
            ui.label(theme::section_title("快速清理"));
            ui.horizontal_wrapped(|ui| {
                for preset in mcworld_core::edit::ALL_PRESETS {
                    let f = preset.to_filter();
                    let n = count_matches(tab, &f);
                    let label = format!("{}（{n}）", preset.label());
                    if ui
                        .add_enabled(n > 0, egui::Button::new(label).fill(theme::FIELD))
                        .on_hover_text(preset.description())
                        .clicked()
                    {
                        let dim = tab.dim().map(|d| d.display.clone()).unwrap_or_default();
                        app.pending_confirm = Some(ConfirmAction::DeleteEntities {
                            count: n,
                            dim,
                        });
                        // 用预设覆盖筛选条件
                        tab.filter_types = preset
                            .to_filter()
                            .include_types
                            .iter()
                            .map(|s| s.strip_prefix("minecraft:").unwrap_or(s).to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        tab.filter_exclude = preset
                            .to_filter()
                            .exclude_types
                            .iter()
                            .map(|s| s.strip_prefix("minecraft:").unwrap_or(s).to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        tab.filter_center = None;
                    }
                }
            });

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);

            // 类型统计表
            ui.label(theme::section_title("当前维度实体类型分布"));
            let mut counts: std::collections::HashMap<&str, usize> = Default::default();
            for e in tab.entities_of_current() {
                *counts.entry(e.short_type()).or_default() += 1;
            }
            let mut list: Vec<_> = counts.into_iter().collect();
            list.sort_by(|a, b| b.1.cmp(&a.1));

            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                egui::Grid::new("entity_types")
                    .num_columns(2)
                    .spacing([24.0, 4.0])
                    .show(ui, |ui| {
                        for (ty, n) in list.iter().take(40) {
                            ui.label(ty.to_string());
                            ui.label(RichText::new(n.to_string()).color(theme::TEXT_DIM));
                            ui.end_row();
                        }
                    });
            });
        });
    app.show_entities = open;
}

fn chunk_window(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_chunks;
    // 本帧收集的「删除某区块实体」请求，绘制结束后统一执行
    let mut delete_req: Option<((i32, i32), usize)> = None;
    let mut goto_chunk: Option<(i32, i32)> = None;
    let sort_mode_in = app.chunk_sort;
    let mut sort_mode_out: Option<crate::ChunkSort> = None;

    egui::Window::new("区块列表")
        .open(&mut open)
        .default_size(Vec2::new(880.0, 520.0))
        .show(ctx, |ui| {
            let Some(tab) = app.tabs.get(app.active) else {
                ui.label("请先打开存档");
                return;
            };
            let chunks = tab.chunks_of_current();
            if chunks.is_empty() {
                ui.label("尚未扫描完成…");
                return;
            }

            // ---- 顶部统计 ----
            let full = chunks.iter().filter(|c| c.is_full()).count();
            let proto = chunks.len() - full;
            let total_nbt: usize = chunks.iter().map(|c| c.nbt_bytes).sum();
            let total_ents = tab.entities_of_current().len();
            ui.horizontal(|ui| {
                ui.label(format!("共 {} 个区块", chunks.len()));
                ui.label(RichText::new(format!("完整 {full}")).color(Color32::from_rgb(0x4A, 0x7A, 0x3C)));
                ui.label(RichText::new(format!("未生成 {proto}")).color(theme::TEXT_DIM));
                ui.label(format!("实体 {total_ents}"));
                ui.label(format!("解压后 {:.1} MiB", total_nbt as f64 / 1048576.0));
            });
            ui.separator();

            // ---- 排序控制 ----
            let mut sort_mode = sort_mode_in;
            ui.horizontal(|ui| {
                ui.label(RichText::new("排序").size(12.0).color(theme::TEXT_DIM));
                for (mode, name) in [
                    (crate::ChunkSort::EntityCount, "实体数"),
                    (crate::ChunkSort::Size, "体积"),
                    (crate::ChunkSort::Coord, "坐标"),
                ] {
                    if ui
                        .selectable_label(sort_mode == mode, name)
                        .clicked()
                    {
                        sort_mode = mode;
                    }
                }
            });
            sort_mode_out = Some(sort_mode);

            let mut sorted: Vec<&mcworld_core::ChunkMeta> = chunks.iter().collect();
            match sort_mode {
                crate::ChunkSort::EntityCount => {
                    // 先算出每个区块的实体总数，避免排序闭包里再借 tab
                    let counts: std::collections::HashMap<(i32, i32), usize> = tab
                        .view()
                        .chunk_entity_types
                        .iter()
                        .map(|(k, v)| (*k, v.iter().map(|(_, n)| *n).sum()))
                        .collect();
                    sorted.sort_by(|a, b| {
                        let ea = counts.get(&(a.x, a.z)).copied().unwrap_or(0);
                        let eb = counts.get(&(b.x, b.z)).copied().unwrap_or(0);
                        eb.cmp(&ea).then_with(|| b.nbt_bytes.cmp(&a.nbt_bytes))
                    })
                }
                crate::ChunkSort::Size => sorted.sort_by(|a, b| b.nbt_bytes.cmp(&a.nbt_bytes)),
                crate::ChunkSort::Coord => sorted.sort_by_key(|c| (c.z, c.x)),
            }
            ui.add_space(2.0);

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("chunk_grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(theme::section_title("区块"));
                        ui.label(theme::section_title("状态"));
                        ui.label(theme::section_title("体积"));
                        ui.label(theme::section_title("实体分布"));
                        ui.label(theme::section_title("驻留"));
                        ui.label(theme::section_title("操作"));
                        ui.end_row();

                        for c in sorted.iter().take(500) {
                            // 坐标：点击可跳转
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("({}, {})", c.x, c.z))
                                            .color(theme::ACCENT),
                                    )
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .on_hover_text("点击在地图上定位到该区块")
                                .clicked()
                            {
                                goto_chunk = Some((c.x, c.z));
                            }

                            ui.label(mcworld_core::world::status_label(&c.status));
                            ui.label(format!("{:.1} KB", c.nbt_bytes as f64 / 1024.0));

                            // 实体类型分布
                            let types = tab.view().chunk_entity_types.get(&(c.x, c.z));
                            let ent_total: usize = types
                                .map(|v| v.iter().map(|(_, n)| n).sum())
                                .unwrap_or(0);

                            if ent_total == 0 {
                                ui.label(RichText::new("—").color(theme::TEXT_DIM));
                            } else {
                                // 最多显示 3 种，其余折叠成 "+N 种"
                                let shown: Vec<String> = types
                                    .unwrap()
                                    .iter()
                                    .take(3)
                                    .map(|(id, n)| {
                                        format!("{}×{n}", crate::lang::entity(id))
                                    })
                                    .collect();
                                let more = types.unwrap().len().saturating_sub(3);
                                let text = if more > 0 {
                                    format!("{} … +{more} 种", shown.join("  "))
                                } else {
                                    shown.join("  ")
                                };
                                ui.label(RichText::new(text).size(12.0))
                                    .on_hover_text(
                                        types
                                            .unwrap()
                                            .iter()
                                            .map(|(id, n)| {
                                                format!("{}: {n}", crate::lang::entity(id))
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    );
                            }

                            ui.label(format!("{:.1} 分", c.inhabited_time as f64 / 1200.0));

                            ui.add_enabled_ui(ent_total > 0, |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(format!("删除 {ent_total} 个实体"))
                                                .size(12.0)
                                                .color(Color32::WHITE),
                                        )
                                        .fill(crate::theme::DANGER),
                                    )
                                    .clicked()
                                {
                                    delete_req = Some(((c.x, c.z), ent_total));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        });

    // ---- 绘制结束后执行 ----
    if let Some(m) = sort_mode_out {
        app.chunk_sort = m;
    }
    if let Some((cx, cz)) = goto_chunk {
        if let Some(tab) = app.tabs.get_mut(app.active) {
            let v = tab.view_mut();
            v.camera.center = Vec2::new(cx as f32 * 16.0 + 8.0, cz as f32 * 16.0 + 8.0);
            if v.camera.scale < 2.0 {
                v.camera.scale = 4.0;
            }
        }
        app.set_status(format!("已定位到区块 ({cx}, {cz})"), StatusKind::Info);
    }
    if let Some(((cx, cz), n)) = delete_req {
        let dim = app
            .tab()
            .map(|t| t.dim().map(|d| d.display.clone()).unwrap_or_default())
            .unwrap_or_default();
        app.pending_confirm = Some(ConfirmAction::DeleteChunkEntities {
            chunk_x: cx,
            chunk_z: cz,
            count: n,
            dim,
        });
    }

    app.show_chunks = open;
}

/// 「已加载片区」列表：把不相连的已加载区域分开列出，每项显示中心坐标，点击跳转。
fn regions_window(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_regions;
    let mut goto: Option<(f32, f32)> = None;
    let mut recompute = false;

    egui::Window::new("已加载片区")
        .open(&mut open)
        .default_size(Vec2::new(560.0, 460.0))
        .show(ctx, |ui| {
            let Some(tab) = app.tabs.get(app.active) else {
                ui.label("请先打开存档");
                return;
            };

            if tab.chunks_of_current().is_empty() {
                ui.label("尚未扫描完成…");
                return;
            }

            ui.horizontal(|ui| {
                let total_chunks: usize = tab.view().regions.iter().map(|r| r.chunk_count).sum();
                ui.label(
                    RichText::new(format!(
                        "共 {} 个片区，{} 个已加载区块",
                        tab.view().regions.len(),
                        total_chunks
                    ))
                    .size(12.0)
                    .color(theme::TEXT_DIM),
                );
                if ui.small_button("重新计算").clicked() {
                    recompute = true;
                }
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("regions_grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(theme::section_title("片区中心"));
                        ui.label(theme::section_title("区块数"));
                        ui.label(theme::section_title("范围"));
                        ui.label(theme::section_title("实体"));
                        ui.end_row();

                        for (i, r) in tab.view().regions.iter().enumerate() {
                            let (bx, bz) = r.center_block();
                            let label = format!("#{i}  ({bx}, {bz})");

                            // 点击坐标即跳转
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(label).color(theme::ACCENT),
                                    )
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .on_hover_text("点击跳转到该片区中心")
                                .clicked()
                            {
                                goto = Some((bx as f32, bz as f32));
                            }

                            ui.label(format!("{}", r.chunk_count));

                            let (sx, sz) = r.span();
                            ui.label(
                                RichText::new(format!("{sx}×{sz} 区块"))
                                    .color(theme::TEXT_DIM),
                            );

                            ui.label(
                                RichText::new(format!("{}", r.entity_count))
                                    .color(theme::TEXT_DIM),
                            );
                            ui.end_row();
                        }
                    });
            });
        });

    if recompute {
        if let Some(tab) = app.tabs.get_mut(app.active) {
            tab.recompute_regions();
        }
    }
    if let Some((x, z)) = goto {
        if let Some(tab) = app.tabs.get_mut(app.active) {
            let v = tab.view_mut();
            v.camera.center = Vec2::new(x, z);
            if v.camera.scale < 1.0 {
                v.camera.scale = 2.0;
            }
        }
        app.set_status(
            format!("已跳转到区块中心 ({}, {})", x as i32, z as i32),
            StatusKind::Info,
        );
    }

    app.show_regions = open;
}

fn gamerules_window(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_gamerules;
    egui::Window::new("游戏规则")
        .open(&mut open)
        .default_size(Vec2::new(420.0, 520.0))
        .show(ctx, |ui| {
            let Some(tab) = app.tabs.get(app.active) else {
                ui.label("请先打开存档");
                return;
            };
            let rules = tab.world.game_rules();
            ui.label(
                RichText::new(format!("共 {} 条（只读预览）", rules.len()))
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("rules")
                    .num_columns(2)
                    .striped(true)
                    .spacing([16.0, 3.0])
                    .show(ui, |ui| {
                        for (k, v) in rules {
                            ui.label(k);
                            ui.label(RichText::new(v).color(theme::TEXT_DIM));
                            ui.end_row();
                        }
                    });
            });
        });
    app.show_gamerules = open;
}

/// 把左侧表单写回 level.dat。
/// 自动保存：表单有改动时由主循环调用。
///
/// 与手动保存的唯一区别是**失败时不弹确认框、只提示状态**，
/// 并且成功后会把备份记进 `last_backup` 以支持撤销。
pub fn autosave_level(app: &mut App) {
    let Some(tab) = app.tabs.get_mut(app.active) else { return };
    if !tab.dirty_level {
        return;
    }
    let label = format!("修改 level.dat（{}）", tab.edit_name);
    match save_level(tab) {
        Ok((_msg, backup_dir)) => {
            app.last_backup = Some(mcworld_core::safety::LastBackup {
                dir: backup_dir,
                label,
            });
            app.set_status("已自动保存到 level.dat", StatusKind::Success);
        }
        Err(e) => app.set_status(format!("自动保存失败: {e}"), StatusKind::Error),
    }
}

/// 把左侧表单的当前值写入 level.dat。
///
/// 会先备份原文件，返回备份目录供「撤销」使用。
fn save_level(tab: &mut WorldTab) -> Result<(String, std::path::PathBuf), String> {
    if tab.world.is_locked() {
        return Err("存档正被游戏占用（session.lock 被锁定），请先关闭游戏".into());
    }

    // 备份原文件
    let backup_dir = mcworld_core::safety::new_backup_dir(
        &tab.path.join("..").join("backups"),
        &tab.world.level.level_name(),
    );
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let level_path = tab.path.join("level.dat");
    if level_path.exists() {
        std::fs::copy(&level_path, backup_dir.join("level.dat"))
            .map_err(|e| format!("备份 level.dat 失败: {e}"))?;
    }

    let mut level = LevelDat::open(&tab.path).map_err(|e| e.to_string())?;

    level.set("LevelName", mcworld_core::Tag::String(tab.edit_name.clone()));
    if let Ok(seed) = tab.edit_seed.trim().parse::<i64>() {
        if let Some(d) = level.data_mut() {
            if let Some(wg) = d.get_mut("WorldGenSettings").and_then(|t| t.as_compound_mut()) {
                wg.insert("seed".into(), mcworld_core::Tag::Long(seed));
            }
        }
    }
    level.set("GameType", mcworld_core::Tag::Int(tab.edit_gametype));
    // 以下三项自动适配 1.21.1 / 26.x 两种存储格式
    level.set_difficulty(tab.edit_difficulty);
    level.set_hardcore(tab.edit_hardcore);
    level.set_allow_commands(tab.edit_cheats);
    level.set_spawn(tab.edit_spawn);

    level.save().map_err(|e| e.to_string())?;
    tab.dirty_level = false;
    Ok((
        "level.dat 已保存（可用「撤销」还原）".to_string(),
        backup_dir,
    ))
}


/// 「导出全图 HTML」对话框。
///
/// 程序内不再提供全图视图（8 万区块缩到 0.04× 每区块不到 1 像素，
/// 又拖又卡），改成导出成网页：浏览器缩放流畅，还能标坐标与群系。
fn export_html_window(app: &mut App, ctx: &egui::Context) {
    // 维度列表：从当前存档取
    let dims: Vec<(String, String)> = app
        .tab()
        .map(|t| {
            t.world
                .dimensions
                .iter()
                .map(|d| (d.id.clone(), d.display.clone()))
                .collect()
        })
        .unwrap_or_default();

    // 首次打开：默认全选
    if app.export_dims.is_empty() && !dims.is_empty() {
        app.export_dims = dims.iter().map(|(id, _)| id.clone()).collect();
    }

    let mut open = app.show_export_html;
    let mut start = false;

    egui::Window::new("导出全图 HTML")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(420.0);

            ui.label("把世界渲染成一个可离线打开的网页：");
            ui.label(
                RichText::new("网页里滚轮缩放、拖动画布，鼠标移上去显示方块坐标与群系。")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(8.0);

            ui.label(RichText::new("导出哪些维度").strong());
            for (id, display) in &dims {
                let mut checked = app.export_dims.iter().any(|d| d == id);
                if ui.checkbox(&mut checked, display).changed() {
                    if checked {
                        app.export_dims.push(id.clone());
                    } else {
                        app.export_dims.retain(|d| d != id);
                    }
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.small_button("全选").clicked() {
                    app.export_dims = dims.iter().map(|(id, _)| id.clone()).collect();
                }
                if ui.small_button("全不选").clicked() {
                    app.export_dims.clear();
                }
            });

            ui.add_space(10.0);
            let total: usize = app.tab().map(|t| t.chunk_count()).unwrap_or(0);
            ui.label(
                RichText::new(format!(
                    "当前维度共 {total} 个区块。区块多时会自动降低贴图精度，\
                     8 万区块大约需要几十秒。"
                ))
                .size(12.0)
                .color(theme::TEXT_DIM),
            );

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let can = !app.export_dims.is_empty() && app.tab().is_some();
                if ui
                    .add_enabled(
                        can,
                        egui::Button::new("选择保存位置并导出")
                            .fill(theme::ACCENT_SOFT),
                    )
                    .clicked()
                {
                    start = true;
                }
                if ui.button("取消").clicked() {
                    app.show_export_html = false;
                }
            });
        });

    app.show_export_html = open;

    if start {
        start_html_export(app);
    }
}

/// 弹出保存对话框并启动后台导出。
fn start_html_export(app: &mut App) {
    let Some(tab) = app.tab() else {
        app.set_status("没有打开的存档", StatusKind::Error);
        return;
    };
    let name = tab.name.clone();
    let root = tab.path.clone();
    let dims = app.export_dims.clone();
    let mode = app.slice_mode();

    let default_name = format!("{name}_地图.html");
    let Some(out) = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter("HTML 网页", &["html"])
        .save_file()
    else {
        return;
    };

    app.jobs.spawn_export_html(root, dims, out, mode);
    app.show_export_html = false;
    app.set_status("正在导出全图，请稍候…", StatusKind::Info);
}


/// 「大存档可以保存缓存」提示。
///
/// 触发条件：当前维度区块数 > 2000。只在每次打开存档后问一次。
fn cache_prompt_window(app: &mut App, ctx: &egui::Context) {
    let n = app.cache_prompt_chunks;
    let mut open = app.show_cache_prompt;
    let mut do_save = false;

    egui::Window::new("存档较大，建议保存缩略图缓存")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(430.0);

            ui.label(format!(
                "当前维度有 {n} 个区块。每次打开都要重新读盘采样，比较慢。"
            ));
            ui.add_space(6.0);
            ui.label(
                RichText::new("保存缓存后：")
                    .strong(),
            );
            ui.label("· 缩略图与每个 region 的指纹一起存到磁盘");
            ui.label("· 下次打开先读缓存，只重算「region 文件有变化」的部分");
            ui.label("· 缓存位置：程序所在目录下的「地图缓存」文件夹");
            ui.add_space(6.0);
            ui.label(
                RichText::new("存档没改动的话，下次几乎是秒开。")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("现在保存缓存").fill(theme::ACCENT_SOFT))
                    .clicked()
                {
                    do_save = true;
                }
                if ui.button("以后再说").clicked() {
                    app.show_cache_prompt = false;
                }
            });
        });

    if !open {
        app.show_cache_prompt = false;
    }

    if do_save {
        app.show_cache_prompt = false;
        app.request_save_cache();
    }
}


/// 导出/缓存任务进行中的浮窗。
///
/// 只在存在"导出全图 HTML"或"保存缓存"任务时显示。
/// 大存档导出要几十秒到几分钟，没有明确的进度指示用户会以为程序卡死。
fn export_progress_window(app: &mut App, ctx: &egui::Context) {
    let busy = app.jobs.labels();
    let job = busy
        .iter()
        .find(|(label, _)| label.contains("导出") || label.contains("缓存"));

    let Some((label, prog)) = job else {
        return;
    };

    let frac = if prog.total > 0 {
        prog.done as f32 / prog.total as f32
    } else {
        0.0
    };

    egui::Window::new("处理中")
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .anchor(egui::Align2::CENTER_TOP, egui::Vec2::new(0.0, 90.0))
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.label(RichText::new(job_title(label)).strong());
            ui.add_space(2.0);
            ui.label(
                RichText::new(label.clone())
                    .size(11.5)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(6.0);

            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(340.0)
                    .text(if prog.message.is_empty() {
                        // 任务还没发出第一条进度。给个更具体的说明，
                        // 而不是干巴巴的"准备中…"。
                        format!("{label} · 准备中")
                    } else if prog.total > 0 {
                        format!("{}  {:.0}%", prog.message, frac * 100.0)
                    } else {
                        prog.message.clone()
                    }),
            );

            ui.add_space(4.0);
            ui.label(
                RichText::new("大存档需要几分钟，请勿关闭程序。")
                    .size(11.5)
                    .color(theme::TEXT_DIM),
            );
        });

    // 保持刷新，让进度条动起来
    ctx.request_repaint_after(std::time::Duration::from_millis(120));
}


/// 把内部任务名翻译成给人看的一句话。
fn job_title(label: &str) -> String {
    if label.starts_with("读取缓存") {
        "读取缩略图缓存".into()
    } else if label.starts_with("保存缓存") {
        "保存缩略图缓存".into()
    } else if label.contains("导出") {
        "导出全图 HTML".into()
    } else {
        label.to_string()
    }
}
