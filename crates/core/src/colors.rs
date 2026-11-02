//! 方块 ID → 俯视地图配色。
//!
//! 用于把存档渲染成类似游戏内地图的俯视图。颜色取自方块的平均外观色，
//! 未收录的方块按命名空间前缀回退，保证任何模组方块也有稳定颜色。

use std::collections::HashMap;

/// 单个方块在图上的颜色（RGB）。
pub type Rgb = [u8; 3];

/// 精确匹配的方块配色表（覆盖原版常见地表方块）。
fn table() -> HashMap<&'static str, Rgb> {
    let mut m = HashMap::new();
    let mut put = |names: &[&'static str], c: Rgb| {
        for n in names {
            m.insert(*n, c);
        }
    };

    // 草/土
    put(&["grass_block", "grass"], [106, 145, 66]);
    put(&["dirt", "coarse_dirt", "rooted_dirt"], [134, 96, 67]);
    put(&["podzol"], [110, 82, 48]);
    put(&["mycelium"], [126, 116, 122]);
    put(&["mud"], [60, 57, 61]);
    put(&["clay"], [160, 166, 179]);
    put(&["gravel"], [132, 127, 124]);
    put(&["sand"], [219, 207, 163]);
    put(&["red_sand"], [190, 102, 33]);
    put(&["sandstone", "smooth_sandstone", "cut_sandstone"], [216, 203, 155]);
    put(&["red_sandstone"], [184, 97, 32]);
    put(&["snow", "snow_block", "powder_snow"], [249, 254, 254]);
    // 测试 26.3 大存档时发现缺配色的方块（浮萍、仙人掌花等）
    put(&["lily_pad"], [32, 88, 40]);
    put(&["cactus_flower"], [228, 116, 168]);
    put(&["bone_block"], [229, 225, 203]);
    // 16 色地毯：默认按白毯处理，其它颜色差异不大，地图上看不出
    put(&["white_carpet"], [233, 236, 236]);
    put(&["ice", "frosted_ice", "packed_ice", "blue_ice"], [160, 190, 220]);

    // 石类
    put(&["stone"], [125, 125, 125]);
    put(&["cobblestone", "mossy_cobblestone"], [110, 110, 110]);
    put(&["granite"], [149, 103, 85]);
    put(&["diorite", "polished_diorite"], [188, 188, 190]);
    put(&["andesite", "polished_andesite"], [136, 136, 137]);
    put(&["deepslate", "cobbled_deepslate", "tuff", "bedrock"], [80, 80, 86]);
    put(&["calcite"], [223, 224, 220]);
    put(&["dripstone_block"], [134, 107, 92]);
    put(&["obsidian", "crying_obsidian"], [21, 18, 30]);

    // 水/岩浆
    put(&["water", "bubble_column", "kelp", "seagrass", "tall_seagrass"], [63, 118, 228]);
    put(&["lava"], [207, 92, 20]);

    // 植被
    put(&["oak_leaves", "jungle_leaves", "oak_log", "oak_wood"], [60, 110, 40]);
    put(&["birch_leaves", "birch_log"], [124, 152, 82]);
    put(&["spruce_leaves", "spruce_log"], [58, 86, 52]);
    put(&["dark_oak_leaves", "dark_oak_log"], [44, 82, 32]);
    put(&["acacia_leaves", "acacia_log"], [104, 128, 52]);
    put(&["mangrove_leaves", "mangrove_log"], [76, 122, 44]);
    put(&["cherry_leaves", "cherry_log"], [226, 168, 186]);
    put(&["azalea_leaves", "flowering_azalea_leaves"], [106, 132, 60]);
    put(&["short_grass", "grass_block_side", "tall_grass", "fern", "large_fern"], [120, 158, 74]);
    put(&["dandelion", "sunflower", "poppy", "cornflower", "azure_bluet", "oxeye_daisy",
          "lily_of_the_valley", "allium", "blue_orchid", "orange_tulip", "red_tulip",
          "white_tulip", "pink_tulip"], [150, 170, 80]);

    // ---- 26.3 / 26.4 新增的植物类 ----
    // pale_oak（苍白橡木 / 苍白之园）：整体灰白，不是普通绿叶
    // 这是 26.4 里占比最高的"未收录"方块（12%），必须单独配色
    put(&["pale_oak_leaves", "pale_oak_sapling"], [163, 168, 158]);
    put(&["pale_oak_log", "pale_oak_wood", "stripped_pale_oak_log"], [122, 120, 112]);
    put(&["pale_oak_planks", "pale_oak_slab", "pale_oak_stairs"], [186, 184, 174]);
    // eyeblossom：苍白之园的特色花，闭合时是青灰，开放时偏白
    put(&["closed_eyeblossom"], [150, 142, 132]);
    put(&["open_eyeblossom"], [214, 208, 196]);
    // wildflowers：一大片浅色小花，整体偏明亮的黄绿
    put(&["wildflowers", "pink_petals", "torchflower", "pitcher_plant"], [188, 196, 108]);
    // leaf_litter：地面落叶层，枯黄褐色
    put(&["leaf_litter"], [138, 108, 58]);
    // bush：灌木丛，中绿
    put(&["bush", "firefly_bush", "short_dry_grass", "tall_dry_grass"], [104, 142, 62]);
    // vine / 藤蔓：深绿
    put(&["vine", "glow_lichen", "hanging_roots", "pale_hanging_moss"], [78, 116, 52]);
    // 苔藓 / 苔藓块
    put(&["moss_block", "moss_carpet", "pale_moss_block", "pale_moss_carpet"], [92, 122, 46]);
    // 蘑菇（小）
    put(&["brown_mushroom", "red_mushroom"], [150, 116, 104]);
    // 蘑菇块（大蘑菇的菌盖）
    put(&["brown_mushroom_block", "mushroom_stem"], [150, 124, 104]);
    put(&["red_mushroom_block"], [172, 62, 58]);
    // 甘蔗
    put(&["sugar_cane"], [140, 176, 104]);
    // 南瓜 / 西瓜
    put(&["pumpkin", "carved_pumpkin", "jack_o_lantern"], [198, 128, 38]);
    put(&["melon", "melon_slice"], [110, 158, 58]);
    // 甜浆果丛 / 枯木
    put(&["sweet_berry_bush"], [92, 118, 58]);
    put(&["dead_bush"], [146, 118, 70]);
    // 杜鹃
    put(&["azalea", "flowering_azalea"], [120, 148, 62]);
    // 泥土小径 / 耕地
    put(&["dirt_path", "farmland"], [150, 116, 70]);
    put(&["dripstone_block", "pointed_dripstone"], [134, 112, 98]);
    // 铜矿
    put(&["copper_ore", "deepslate_copper_ore", "raw_copper_block"], [156, 116, 92]);
    put(&["copper_block", "oxidized_copper", "weathered_copper"], [186, 116, 82]);
    // 石砖类（各地砖石变种）
    put(&["stone_bricks", "stone_brick_stairs", "stone_brick_slab",
          "stone_brick_wall", "mossy_stone_bricks", "mossy_stone_brick_stairs",
          "mossy_stone_brick_slab", "mossy_stone_brick_wall",
          "cracked_stone_bricks", "chiseled_stone_bricks"], [122, 122, 122]);
    // 木台阶 / 楼梯（按树种走，这里补几个常见遗漏）
    put(&["acacia_stairs", "acacia_slab", "acacia_planks"], [168, 100, 52]);
    put(&["stone_slab", "smooth_stone_slab", "stone_stairs"], [126, 126, 126]);
    // 箱子 / 火把 / 传送门
    put(&["chest", "trapped_chest", "barrel"], [146, 112, 66]);
    put(&["torch", "wall_torch", "soul_torch", "soul_wall_torch"], [226, 190, 96]);
    put(&["end_portal", "end_portal_frame", "end_gateway"], [40, 30, 52]);
    put(&["nether_portal"], [96, 48, 148]);

    // ---- 下界植物补充 ----
    put(&["crimson_roots", "crimson_fungus"], [142, 40, 58]);
    put(&["warped_roots", "warped_fungus"], [40, 122, 112]);
    put(&["twisting_vines", "weeping_vines"], [40, 122, 112]);
    put(&["shroomlight"], [216, 150, 88]);
    put(&["nether_gold_ore"], [176, 132, 74]);
    put(&["fire", "campfire", "soul_fire"], [224, 148, 48]);
    put(&["nether_sprouts"], [88, 160, 130]);

    // ---- 其他遗漏的常见方块 ----
    put(&["lilac", "peony", "rose_bush", "sunflower"], [140, 158, 92]);
    put(&["cobweb"], [220, 220, 220]);
    put(&["cactus"], [88, 138, 62]);
    put(&["bamboo"], [150, 178, 76]);
    put(&["hay_block", "wheat"], [196, 166, 62]);
    put(&["melon_stem", "pumpkin_stem", "attached_melon_stem", "attached_pumpkin_stem"], [104, 138, 62]);
    put(&["brown_wall_banner", "brown_banner", "brown_wool", "brown_carpet"], [114, 72, 46]);

    // 下界
    put(&["netherrack"], [110, 52, 52]);
    put(&["nether_bricks", "nether_brick"], [44, 22, 26]);
    put(&["soul_sand", "soul_soil"], [82, 62, 50]);
    put(&["basalt", "smooth_basalt", "blackstone"], [70, 68, 72]);
    put(&["magma_block"], [146, 63, 28]);
    put(&["crimson_nylium", "crimson_stem", "nether_wart_block"], [126, 40, 52]);
    put(&["warped_nylium", "warped_stem", "warped_wart_block"], [43, 115, 106]);
    put(&["glowstone"], [190, 155, 96]);
    put(&["nether_quartz_ore"], [140, 100, 95]);

    // 末地
    put(&["end_stone", "end_stone_bricks"], [219, 222, 168]);
    put(&["purpur_block", "purpur_pillar"], [169, 125, 169]);
    put(&["chorus_plant", "chorus_flower"], [120, 90, 120]);

    // 建筑
    put(&["oak_planks", "birch_planks", "spruce_planks", "jungle_planks", "acacia_planks",
          "dark_oak_planks", "mangrove_planks", "cherry_planks", "bamboo_planks",
          "crimson_planks", "warped_planks", "oak_slab", "oak_stairs"], [162, 130, 78]);
    put(&["bricks", "brick"], [150, 97, 83]);
    put(&["glass", "tinted_glass", "glass_pane"], [200, 220, 230]);
    put(&["iron_block", "iron_bars", "iron_ore", "iron_door", "iron_trapdoor"], [220, 220, 220]);
    put(&["gold_block", "gold_ore"], [247, 214, 90]);
    put(&["diamond_block", "diamond_ore", "deepslate_diamond_ore"], [98, 237, 228]);
    put(&["emerald_block", "emerald_ore"], [42, 203, 88]);
    put(&["copper_block", "oxidized_copper", "weathered_copper"], [192, 107, 79]);
    put(&["coal_ore", "deepslate_copper_ore", "deepslate_coal_ore"], [105, 105, 105]);
    put(&["sculk", "sculk_catalyst", "sculk_sensor", "sculk_shrieker", "sculk_vein"], [22, 42, 48]);
    put(&["moss_block", "moss_carpet"], [90, 122, 60]);

    m
}

/// 返回方块资源位置的显示色。
///
/// `name` 可带或不带 `minecraft:` 前缀。
/// 该方块是否在颜色表里**显式收录**（用于诊断覆盖率）。
pub fn is_known(name: &str) -> bool {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    table().contains_key(short)
}

pub fn block_color(name: &str) -> Rgb {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let table = table();
    if let Some(c) = table.get(short) {
        return *c;
    }
    // 常见后缀推断，提升未收录方块的辨识度
    let infer: &[(&str, Rgb)] = &[
        ("_leaves", [70, 118, 48]),
        ("_log", [110, 84, 52]),
        ("_planks", [162, 130, 78]),
        ("_wool", [200, 200, 200]),
        ("_concrete", [190, 190, 190]),
        ("_terracotta", [152, 94, 67]),
        ("_glass", [200, 220, 230]),
        ("_ore", [120, 120, 120]),
        ("_bricks", [150, 97, 83]),
        ("_sand", [219, 207, 163]),
        ("_stone", [125, 125, 125]),
        ("_dirt", [134, 96, 67]),
        ("_water", [63, 118, 228]),
        ("_lava", [207, 92, 20]),
        ("_snow", [249, 254, 254]),
        ("_ice", [160, 190, 220]),
        ("_coral", [230, 120, 150]),
        ("_flower", [220, 160, 90]),
        ("_sapling", [110, 150, 70]),
    ];
    for (suffix, c) in infer {
        if short.ends_with(suffix) {
            return *c;
        }
    }
    // 稳定的伪随机回退色：同一方块恒定
    let h = fnv1a(short);
    [
        (90 + (h & 0x3F)) as u8,
        (90 + ((h >> 8) & 0x3F)) as u8,
        (90 + ((h >> 16) & 0x3F)) as u8,
    ]
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 是否应视为"透明/可看穿"（用于地表采样时向下穿透）。
pub fn is_air_like(name: &str) -> bool {
    let s = name.strip_prefix("minecraft:").unwrap_or(name);
    matches!(
        s,
        "air"
            | "cave_air"
            | "void_air"
            | "light"
            | "barrier"
            | "structure_void"
            | "moving_piston"
            | "bubble_column"
    )
}

/// 是否是需要"看穿"的封面方块。
///
/// 下界顶部有一层（有时两层）基岩作为天花板，如果把它当作地表，
/// 整张下界地图会变成一片基岩灰。俯视图应该显示**基岩下面**的真实地形。
pub fn is_cover_block(name: &str) -> bool {
    let s = name.strip_prefix("minecraft:").unwrap_or(name);
    matches!(s, "bedrock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_blocks_are_greenish_or_expected() {
        assert_eq!(block_color("minecraft:grass_block"), [106, 145, 66]);
        assert_eq!(block_color("grass_block"), [106, 145, 66]);
        assert_eq!(block_color("minecraft:water"), [63, 118, 228]);
    }

    #[test]
    fn unknown_blocks_get_stable_color() {
        let a = block_color("somemod:weird_block");
        let b = block_color("somemod:weird_block");
        assert_eq!(a, b, "同一方块必须稳定同色");
        let c = block_color("somemod:other_block");
        assert_ne!(a, c, "不同方块应有区分度");
    }

    #[test]
    fn suffix_inference_works() {
        assert_eq!(block_color("somemod:magic_leaves"), [70, 118, 48]);
        assert_eq!(block_color("somemod:magic_ore"), [120, 120, 120]);
    }

    #[test]
    fn air_detection() {
        assert!(is_air_like("minecraft:air"));
        assert!(is_air_like("cave_air"));
        assert!(!is_air_like("minecraft:stone"));
    }

    #[test]
    fn bedrock_is_cover_block() {
        assert!(is_cover_block("minecraft:bedrock"));
        assert!(is_cover_block("bedrock"));
        assert!(!is_cover_block("minecraft:netherrack"));
        // 基岩不是空气，不能被当成空气跳过（它下面可能有地形）
        assert!(!is_air_like("minecraft:bedrock"));
    }
}
