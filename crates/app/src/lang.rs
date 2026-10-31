//! Minecraft 官方中文名称查表。
//!
//! 名称数据来自客户端资源包 `assets/minecraft/lang/zh_cn.json`，
//! **版权归 Mojang / Microsoft 所有**。因此本程序**不随代码分发该文件**
//! （早期版本曾把 137KB 的字典内嵌进 exe，那是不合规的），
//! 改为在运行时从用户自己的 `.minecraft` 目录读取：
//!
//! ```text
//! <gameDir>/assets/indexes/<最新索引>.json       → 查出 zh_cn.json 的 sha1
//! <gameDir>/assets/objects/<sha1 前两位>/<sha1>  → 真正的语言文件
//! ```
//!
//! 找不到时（只装了服务端、版本目录异常、或用户从未启动过游戏），
//! 所有查表函数退回显示原始注册名（英文 ID），**不会崩溃**。
//!
//! 键格式：`种类:短id`，例如
//! - `block:stone`           → 石头
//! - `biome:crimson_forest`  → 绯红森林
//! - `entity:item`           → 物品

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
static GAME_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 告诉查表模块"用户的 .minecraft 在哪"，以便去读官方语言文件。
///
/// 由主程序在打开存档时调用（从存档路径向上找 `assets/` 目录）。
/// 只记录第一次设置的值；之后再调用无效。
pub fn set_game_dir(dir: PathBuf) {
    let _ = GAME_DIR.set(dir);
}

/// 从存档路径推测 `.minecraft` 目录并设置。
///
/// 常见布局：`<gameDir>/saves/<世界名>`，所以向上找直到出现 `assets` 目录。
pub fn set_game_dir_from_save(save: &Path) {
    let mut cur = save.parent();
    while let Some(dir) = cur {
        if dir.join("assets").is_dir() {
            set_game_dir(dir.to_path_buf());
            return;
        }
        cur = dir.parent();
    }
}

fn table() -> &'static HashMap<String, String> {
    TABLE.get_or_init(|| {
        if let Some(dir) = GAME_DIR.get() {
            if let Some(map) = load_from_game_dir(dir) {
                return map;
            }
        }
        HashMap::new()
    })
}

/// 从 `.minecraft` 目录定位并解析 `zh_cn.json`。
fn load_from_game_dir(game: &Path) -> Option<HashMap<String, String>> {
    // 1) 找一个可用的资源索引。取数字最大的那个（通常是当前版本）。
    let indexes = game.join("assets").join("indexes");
    let mut best: Option<(i64, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(&indexes) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") {
                continue;
            }
            let key = name
                .trim_end_matches(".json")
                .parse::<i64>()
                .unwrap_or_default();
            if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                best = Some((key, e.path()));
            }
        }
    }
    let (_, index_path) = best?;
    let index = std::fs::read_to_string(&index_path).ok()?;

    // 2) 从索引里取 minecraft/lang/zh_cn.json 的 hash
    let hash = extract_hash(&index, "minecraft/lang/zh_cn.json")?;
    if hash.len() < 2 {
        return None;
    }

    // 3) 资源对象路径：objects/<前两位>/<完整 hash>
    let obj = game
        .join("assets")
        .join("objects")
        .join(&hash[..2])
        .join(&hash);
    let raw = std::fs::read_to_string(&obj).ok()?;

    // 4) 官方语言文件是完整的扁平表，键形如 "block.minecraft.stone"
    let map = convert_official(&raw);
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// 从资源索引 JSON 里抠出某个资源名的 hash 字段。
///
/// 索引结构：`{"objects":{"minecraft/lang/zh_cn.json":{"hash":"...","size":123}}}`
/// 这里做最小解析，不为这一个用途引入 serde。
fn extract_hash(index: &str, target: &str) -> Option<String> {
    let key = format!("\"{target}\"");
    let pos = index.find(&key)?;
    let rest = &index[pos + key.len()..];
    let hpos = rest.find("\"hash\"")?;
    let after = &rest[hpos + 6..];
    let q1 = after.find('"')?;
    let q2 = after[q1 + 1..].find('"')?;
    Some(after[q1 + 1..q1 + 1 + q2].to_string())
}

/// 把官方 `zh_cn.json` 转成本程序内部用的扁平表。
///
/// 官方键：`block.minecraft.stone`、`biome.minecraft.forest`
/// 本程序：`block:stone`、`biome:forest`
fn convert_official(raw: &str) -> HashMap<String, String> {
    let full = serde_json_lite(raw);
    let mut out = HashMap::with_capacity(full.len());

    for (k, v) in full {
        let Some((kind, rest)) = k.split_once('.') else {
            continue;
        };
        if !matches!(kind, "block" | "biome" | "entity" | "item" | "effect") {
            continue;
        }
        let Some(id) = rest.strip_prefix("minecraft.") else {
            continue;
        };
        out.insert(format!("{kind}:{id}"), v);
    }
    out
}

/// 极简 JSON 解析：只处理 `{"k":"v",...}` 这种扁平字符串表。
///
/// 为了不给依赖树再加一个 serde，这里手写。输入是我们自己生成的固定格式，
/// 只包含普通字符串键值与少量转义。
fn serde_json_lite(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let bytes = raw.as_bytes();
    let mut i = 0usize;

    // 跳过开头的 {
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    i += 1;

    loop {
        // 跳过空白与逗号
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }

        let Some((key, ni)) = read_string(raw, i) else { break };
        i = ni;
        while i < bytes.len() && bytes[i] != b':' {
            i += 1;
        }
        i += 1;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            continue;
        }
        let Some((val, ni)) = read_string(raw, i) else { break };
        i = ni;
        out.insert(key, val);
    }
    out
}

/// 从 `start` 处的引号开始读一个 JSON 字符串，返回 (内容, 新位置)。
fn read_string(s: &str, start: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut i = start + 1;
    let mut out = String::new();
    let mut seg_start = i;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                out.push_str(&s[seg_start..i]);
                return Some((out, i + 1));
            }
            b'\\' => {
                out.push_str(&s[seg_start..i]);
                i += 1;
                match bytes.get(i) {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'/') => out.push('/'),
                    Some(b'n') => out.push('\n'),
                    Some(b't') => out.push('\t'),
                    Some(b'r') => out.push('\r'),
                    Some(b'b') => out.push('\u{8}'),
                    Some(b'f') => out.push('\u{c}'),
                    Some(b'u') => {
                        // \uXXXX
                        let hex = s.get(i + 1..i + 5).unwrap_or("");
                        if let Ok(cp) = u32::from_str_radix(hex, 16) {
                            if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            }
                        }
                        i += 4;
                    }
                    _ => {}
                }
                i += 1;
                seg_start = i;
            }
            _ => i += 1,
        }
    }
    None
}

/// 把 `minecraft:stone` 这类 id 转成中文名。
///
/// - 自动去掉 `minecraft:` 前缀再查表
/// - 找不到时：原版 id 去掉前缀返回，模组 id 保留命名空间
///   （否则不同模组的同名方块会混淆）
pub fn name_of(kind: &str, id: &str) -> String {
    let short = id.strip_prefix("minecraft:").unwrap_or(id);
    let key = format!("{kind}:{short}");
    if let Some(v) = table().get(&key) {
        return v.clone();
    }
    // 有些方块/物品的翻译键与 id 不完全一致，尝试常见变体
    if kind == "block" {
        if let Some(v) = table().get(&format!("item:{short}")) {
            return v.clone();
        }
    }
    short.to_string()
}

/// 方块中文名。
pub fn block(id: &str) -> String {
    name_of("block", id)
}

/// 群系中文名。
pub fn biome(id: &str) -> String {
    name_of("biome", id)
}

/// 实体中文名。
pub fn entity(id: &str) -> String {
    name_of("entity", id)
}

/// 表里共有多少条（自检用）。
pub fn len() -> usize {
    table().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_may_be_empty_without_game_dir() {
        // 没有设置 gameDir 时，表应为空，且所有查表函数安全退回英文 id。
        // 这保证程序在没装游戏的机器上也不会崩。
        assert_eq!(block("minecraft:stone"), "stone");
        assert_eq!(biome("minecraft:plains"), "plains");
    }

    #[test]
    #[ignore = "需要本机 .minecraft 的 zh_cn.json"]
    fn common_blocks() {
        assert_eq!(block("minecraft:stone"), "石头");
        assert_eq!(block("minecraft:netherrack"), "下界岩");
        assert_eq!(block("minecraft:grass_block"), "草方块");
    }

    #[test]
    #[ignore = "需要本机 .minecraft 的 zh_cn.json"]
    fn common_biomes() {
        assert_eq!(biome("minecraft:plains"), "平原");
        assert_eq!(biome("minecraft:crimson_forest"), "绯红森林");
    }

    #[test]
    fn loads_real_game_assets_if_present() {
        // 从环境变量拿一个 .minecraft 路径来验证真实数据。
        // 没设就跳过——这样 CI / 无游戏环境也能通过，不把本机路径写进仓库。
        //
        //   $env:MCWORLD_TEST_GAMEDIR = "C:/Users/you/AppData/Roaming/.minecraft"
        //   cargo test -p mcworld-app
        let Ok(dir) = std::env::var("MCWORLD_TEST_GAMEDIR") else {
            println!("未设置 MCWORLD_TEST_GAMEDIR，跳过真实数据测试");
            return;
        };
        let p = std::path::Path::new(&dir);
        if !p.join("assets").is_dir() {
            println!("{dir} 下没有 assets 目录，跳过");
            return;
        }
        set_game_dir_from_save(&p.join("saves").join("x"));
        if len() == 0 {
            println!("未读到语言文件，跳过");
            return;
        }
        println!("从 {dir} 加载 {} 条中文名", len());
        assert_eq!(block("minecraft:stone"), "石头");
        assert_eq!(block("minecraft:grass_block"), "草方块");
        assert_eq!(biome("minecraft:plains"), "平原");
    }

    #[test]
    fn unknown_falls_back_to_id() {
        // 模组 id 保留命名空间（否则不同模组的同名方块会混淆）
        assert_eq!(block("somemod:weird_block"), "somemod:weird_block");
        // 原版 id 去掉 minecraft: 前缀
        assert_eq!(block("minecraft:not_a_real_block"), "not_a_real_block");
    }

    #[test]
    fn prefix_optional() {
        assert_eq!(block("stone"), block("minecraft:stone"));
    }
}
