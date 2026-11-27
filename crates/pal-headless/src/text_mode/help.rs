//! Interactive `help [command|topic]` reference for RePal Unified Text Mode (#276).
//!
//! Provides localized (`ZhHans` / `ZhHant` / `En`) overviews and per-command / per-topic
//! help with syntax, aliases, examples, and common caveats — automatically filtering out
//! forbidden debug commands in `--obs fair` mode.

use pal_core::Language;

struct HelpEntry {
    topic: &'static str,
    aliases: &'static [&'static str],
    category: &'static str,
    fair_allowed: bool,
    syntax_zh: &'static str,
    syntax_en: &'static str,
    summary_hans: &'static str,
    summary_hant: &'static str,
    summary_en: &'static str,
    details_hans: &'static [&'static str],
    details_hant: &'static [&'static str],
    details_en: &'static [&'static str],
    examples: &'static [&'static str],
}

const ENTRIES: &[HelpEntry] = &[
    HelpEntry {
        topic: "explore",
        aliases: &["look", "l", "map", "minimap", "sw", "nw", "ne", "se", "goto", "go", "walk", "talk", "search", "wait", "nav", "地图"],
        category: "explore",
        fair_allowed: true,
        syntax_zh: "look | map | sw/nw/ne/se [步数] | goto <ev_id | x y> | talk [ev_id] | search | wait [ticks]",
        syntax_en: "look | map | sw/nw/ne/se [steps] | goto <ev_id | x y> | talk [ev_id] | search | wait [ticks]",
        summary_hans: "探索、观察视野、小地图与移动交互",
        summary_hant: "探索、觀察視野、小地圖與移動交互",
        summary_en: "Explore scenes, inspect viewport minimap, walk, and interact with NPCs/exits",
        details_hans: &[
            "• look (l)：显示当前场景、坐标、队员血蓝、4 方向可走步数 (walkable_dist)、视野事件及小地图。",
            "• map：强制打印 20x25 等距视野小地图（@ 自己，E 可见事件，> 可见出口/楼梯，. 可走，# 障碍，? 遮挡）。",
            "• sw/nw/ne/se [1..64]：沿等距 4 方向行走（注意：ne 是右上，n 是对话翻页 next）。",
            "• goto <ev_id | x y>：模拟玩家点击走往可见事件或屏幕坐标；对出口/楼梯使用 goto 会直接踩上触发切图/上下楼（若被门/栅栏等物件阻挡，先对阻挡物件执行 talk <id> 打开再 goto）。",
            "• talk [ev_id] / search：与指定 NPC 靠近交谈，或直接调查面前格子（给 NPC 送任务道具需靠近后用 use <物品>；关闭的木门/帘子/机关也可通过 talk/search 打开）。",
        ],
        details_hant: &[
            "• look (l)：顯示當前場景、座標、隊員血藍、4 方向可走步數 (walkable_dist)、視野事件及小地圖。",
            "• map：強制列印 20x25 等距視野小地圖（@ 自己，E 可見事件，> 可見出口/樓梯，. 可走，# 障礙，? 遮擋）。",
            "• sw/nw/ne/se [1..64]：沿等距 4 方向行走（注意：ne 是右上，n 是對話翻頁 next）。",
            "• goto <ev_id | x y>：模擬玩家點擊走往可見事件或螢幕座標；對出口/樓梯使用 goto 會直接踩上觸發切圖/上下樓（若被門/柵欄等物件阻擋，先對阻擋物件執行 talk <id> 打開再 goto）。",
            "• talk [ev_id] / search：與指定 NPC 靠近交談，或直接調查面前格子（給 NPC 送任務道具需靠近後用 use <物品>；關閉的木門/簾子/機關也可透過 talk/search 打開）。",
        ],
        details_en: &[
            "• look (l): Show scene, coordinates, party HP/MP, 4-direction walkable_dist, visible events, and minimap.",
            "• map: Always emit the 20x25 isometric viewport minimap (@ party, E event, > exit/stairs, . floor, # wall, ? occluded).",
            "• sw/nw/ne/se [1..64]: Walk along isometric diagonals (use 'ne', never 'n' which is 'next').",
            "• goto <ev_id | x y>: Click-walk to a visible event or on-screen coords; 'goto <exit_id>' steps onto doorways/stairs (if blocked by adjacent objects, try 'talk <id>' / 'search' on them first — many closed doors, curtains & fences open when examined).",
            "• talk [ev_id] / search: Walk up and talk/examine an NPC or object, or interact in front of the leader (for quest items, stand next to the NPC and run 'use <item>').",
        ],
        examples: &["look", "map", "se 4", "goto 46", "talk 12", "search"],
    },
    HelpEntry {
        topic: "dialog",
        aliases: &["next", "n", "ok", "continue", "c", "nextall", "yes", "y", "no", "close", "leave"],
        category: "dialog",
        fair_allowed: true,
        syntax_zh: "next(n/ok) | continue(c) | yes(1) | no(2) | close",
        syntax_en: "next(n/ok) | continue(c) | yes(1) | no(2) | close",
        summary_hans: "推进对话、回答选项与关闭弹窗",
        summary_hant: "推進對話、回答選項與關閉彈窗",
        summary_en: "Advance dialogues, answer Yes/No choices, and close modals",
        details_hans: &[
            "• next (n/ok)：推进一页对话或关闭战斗胜利结算。",
            "• continue (c)：连续推进整段对话并在 log 中返回所有台词，直到出现选项、战斗、商店或恢复自由探索。",
            "• yes (1/是) / no (2/否)：回答当前「是/否」选项（多页选项会自动翻到末页再确认）。",
            "• close (leave)：关闭当前商店、当铺或菜单。",
        ],
        details_hant: &[
            "• next (n/ok)：推進一頁對話或關閉戰鬥勝利結算。",
            "• continue (c)：連續推進整段對話並在 log 中返回所有台詞，直到出現選項、戰鬥、商店或恢復自由探索。",
            "• yes (1/是) / no (2/否)：回答當前「是/否」選項（多頁選項會自動翻到末頁再確認）。",
            "• close (leave)：關閉當前商店、當鋪或選單。",
        ],
        details_en: &[
            "• next (n/ok): Advance one dialogue page or dismiss battle victory.",
            "• continue (c): Drain an entire consecutive dialogue chain (logging every line) until a choice, battle, shop, or explore.",
            "• yes (1) / no (2): Answer a Yes/No prompt (auto-advances remaining pagination pages first).",
            "• close (leave): Close the active shop, pawnshop, or menu modal.",
        ],
        examples: &["next", "continue", "yes", "no", "close"],
    },
    HelpEntry {
        topic: "battle",
        aliases: &["attack", "a", "atk", "coop", "defend", "def", "flee", "run", "auto", "cancel-auto"],
        category: "battle",
        fair_allowed: true,
        syntax_zh: "attack(a) [slot] | magic <id|名称> [slot] | use/item <id|名称> [slot] | coop [slot] | defend | flee | auto | cancel-auto",
        syntax_en: "attack(a) [slot] | magic <id|name> [slot] | use/item <id|name> [slot] | coop [slot] | defend | flee | auto | cancel-auto",
        summary_hans: "回合制战斗指令（支持道具与仙术 ID 或名称）",
        summary_hant: "回合制戰鬥指令（支援道具與仙術 ID 或名稱）",
        summary_en: "Turn-based battle actions (magic & item commands accept either ID or name)",
        details_hans: &[
            "• 命令始终发给当前 prompting 指向的英雄（先用 status 查看各队员已学仙术及效果，用 bag 查看可用/可投掷道具）。",
            "• attack [slot]：普通攻击（省略 slot 默认打第一个存活敌人）。",
            "• magic <id|名称> [slot]：施放仙术；敌方技能 slot 为敌人槽位，治疗/增益技能 slot 为我方英雄槽位（省略则自动选血最少的队友或首名存活敌人）。",
            "• use <id|名称> [slot] 或 item <id|名称> [slot]：战斗中使用恢复/辅助道具或投掷暗器（两者完全等价）。",
            "• coop [slot] / defend / flee / auto / cancel-auto：合体技、防御、逃跑、自动围攻与取消自动。",
        ],
        details_hant: &[
            "• 命令始終發給當前 prompting 指向的英雄（先用 status 查看各隊員已學仙術及效果，用 bag 查看可用/可投擲道具）。",
            "• attack [slot]：普通攻擊（省略 slot 預設打第一個存活敵人）。",
            "• magic <id|名稱> [slot]：施放仙術；敵方技能 slot 為敵人槽位，治療/增益技能 slot 為我方英雄槽位（省略則自動選血最少的隊友或首名存活敵人）。",
            "• use <id|名稱> [slot] 或 item <id|名稱> [slot]：戰鬥中使用恢復/輔助道具或投擲暗器（兩者完全等價）。",
            "• coop [slot] / defend / flee / auto / cancel-auto：合體技、防禦、逃跑、自動圍攻與取消自動。",
        ],
        details_en: &[
            "• Commands target the currently prompting hero (run 'status' to see each hero's spells + effects, and 'bag' for battle-usable/throwable items).",
            "• attack [slot]: Basic attack (defaults to first alive enemy).",
            "• magic <id|name> [slot]: Cast a spell; enemy spells index battle.enemies[], support spells index battle.heroes[] (omitting slot picks lowest-HP ally or first alive enemy).",
            "• use <id|name> [slot] or item <id|name> [slot]: Use a healing/support item or throw a weapon in battle (both commands are equivalent in battle).",
            "• coop [slot] / defend / flee / auto / cancel-auto: Party coop skill, defend, flee, toggle auto-battle.",
        ],
        examples: &["attack 0", "magic 气疗术", "magic 345 0", "use 观音符 0", "item 153 0", "defend", "flee"],
    },
    HelpEntry {
        topic: "magic",
        aliases: &["cast"],
        category: "magic",
        fair_allowed: true,
        syntax_zh: "magic <id|名称> [target_slot] [caster_idx]",
        syntax_en: "magic <id|name> [target_slot] [caster_idx]",
        summary_hans: "战斗或野外施放仙术（支持数字 ID 或中文/英文名称）",
        summary_hant: "戰鬥或野外施放仙術（支援數字 ID 或中文/英文名稱）",
        summary_en: "Cast a spell in battle or explore mode (accepts numeric ID or spell name)",
        details_hans: &[
            "• 运行 status 可查看全队已学仙术的 #ID、耗蓝、可用场景（野/战）、目标范围及效果说明。",
            "• 战斗中：magic <id|名称> [slot] 由当前行动英雄施放。",
            "• 野外探索：magic <id|名称> [目标队员序号] [施法队员序号] 可施放标记为「野」的恢复或传送仙术（如气疗术、土灵珠）。",
        ],
        details_hant: &[
            "• 運行 status 可查看全隊已學仙術的 #ID、耗藍、可用場景（野/戰）、目標範圍及效果說明。",
            "• 戰鬥中：magic <id|名稱> [slot] 由當前行動英雄施放。",
            "• 野外探索：magic <id|名稱> [目標隊員序號] [施法隊員序號] 可施放標記為「野」的恢復或傳送仙術（如氣療術、土靈珠）。",
        ],
        details_en: &[
            "• Run 'status' to list every learned spell with its #ID, MP cost, field/battle usability, target scope, and effect description.",
            "• In battle: 'magic <id|name> [slot]' casts from the prompting hero.",
            "• In explore mode: 'magic <id|name> [target_party_idx] [caster_party_idx]' casts field-usable healing/utility spells.",
        ],
        examples: &["status", "magic 气疗术 0", "magic 296 0 0", "magic 御剑术 0"],
    },
    HelpEntry {
        topic: "items",
        aliases: &["use", "item", "i", "equip", "bag", "inv", "buy", "sell", "shop"],
        category: "items",
        fair_allowed: true,
        syntax_zh: "bag | use <id|名称> [slot] | item <id|名称> [slot] | equip <id|名称> [party_idx] | buy <shop_idx> [qty] | sell <id|名称> [qty]",
        syntax_en: "bag | use <id|name> [slot] | item <id|name> [slot] | equip <id|name> [party_idx] | buy <shop_idx> [qty] | sell <id|name> [qty]",
        summary_hans: "查看背包道具效果、使用/投掷道具、装备与买卖",
        summary_hant: "查看背包道具效果、使用/投擲道具、裝備與買賣",
        summary_en: "Inspect inventory effects, use/throw items, equip gear, and buy/sell in shops",
        details_hans: &[
            "• bag (items/inv)：列出背包所有物品的 #ID、名称、数量、用途标签（野/战/投掷/装备）及效果说明。",
            "• use <id|名称> [slot]：野外用于吃药/对面前 NPC 使用剧情道具（如桂花酒、破天锤）；战斗中自动等同于 item，可使用恢复道具或投掷暗器。",
            "• equip <id|名称> [party_idx]：为指定队员穿戴武器或防具（party_idx 默认 0 为队长）。",
            "• buy <shop_idx> [qty] / sell <id|名称> [qty]：普通商店 (ui_view: shop，如铁匠铺/药铺) 仅支持购买 (buy)；变卖物品 (sell) 仅在当铺 (ui_view: pawnshop) 有效。",
        ],
        details_hant: &[
            "• bag (items/inv)：列出背包所有物品的 #ID、名稱、數量、用途標籤（野/戰/投擲/裝備）及效果說明。",
            "• use <id|名稱> [slot]：野外用於吃藥/對面前 NPC 使用劇情道具（如桂花酒、破天錘）；戰鬥中自動等同於 item，可使用恢復道具或投擲暗器。",
            "• equip <id|名稱> [party_idx]：為指定隊員穿戴武器或防具（party_idx 預設 0 為隊長）。",
            "• buy <shop_idx> [qty] / sell <id|名稱> [qty]：普通商店 (ui_view: shop，如鐵匠鋪/藥鋪) 僅支援購買 (buy)；變賣物品 (sell) 僅在當鋪 (ui_view: pawnshop) 有效。",
        ],
        details_en: &[
            "• bag (items/inv): List every inventory item with #ID, name, count, usability/equip tags, and effect description.",
            "• use <id|name> [slot]: In explore mode uses consumables or quest items on the faced NPC/object; in battle automatically routes to BattleItem (use or throw).",
            "• equip <id|name> [party_idx]: Equip gear onto party slot party_idx (default 0 = leader).",
            "• buy <shop_idx> [qty] / sell <id|name> [qty]: Regular shops (ui_view: shop, e.g. blacksmith/pharmacy) are buy-only; 'sell' only works at a Pawnshop (ui_view: pawnshop / 当铺).",
        ],
        examples: &["bag", "use 金创药 0", "use 61", "equip 木剑 0", "buy 0 2", "sell 净衣符 1"],
    },
    HelpEntry {
        topic: "status",
        aliases: &["st", "party"],
        category: "status",
        fair_allowed: true,
        syntax_zh: "status (st)",
        syntax_en: "status (st)",
        summary_hans: "查看队伍属性、装备与所有已学仙术（含 ID、耗蓝与效果）",
        summary_hant: "查看隊伍屬性、裝備與所有已學仙術（含 ID、耗藍與效果）",
        summary_en: "Show party stats, equipped gear, and all learned spells with IDs, MP costs & effects",
        details_hans: &[
            "• 显示每位队员的序号 [#idx]、等级、经验、HP/MP、五维属性、装备列表及全部仙术（含 #ID、耗蓝、野/战标签与效果说明）。",
            "• 在 --json 模式下，artifacts.status 包含结构化字段：id、name、cost_mp、desc、target、usable_field、usable_battle、affordable。",
        ],
        details_hant: &[
            "• 顯示每位隊員的序號 [#idx]、等級、經驗、HP/MP、五維屬性、裝備列表及全部仙術（含 #ID、耗藍、野/戰標籤與效果說明）。",
            "• 在 --json 模式下，artifacts.status 包含結構化欄位：id、name、cost_mp、desc、target、usable_field、usable_battle、affordable。",
        ],
        details_en: &[
            "• Shows each party member's [#idx], level, EXP, HP/MP, 5 core stats, equipment, and learned spells (#ID, MP cost, field/battle tags, and effect desc).",
            "• In --json mode, artifacts.status provides structured magics[] with id, name, cost_mp, desc, target, usable_field, usable_battle, and affordable.",
        ],
        examples: &["status", "st"],
    },
    HelpEntry {
        topic: "menu",
        aliases: &["m", "ui", "key", "up", "down", "left", "right", "back", "cancel", "tab", "confirm", "enter", "esc", "click"],
        category: "menu",
        fair_allowed: true,
        syntax_zh: "menu(m) | ui <up|down|left|right|pgup|pgdn|home|end|ok|back|menu> | click <x> <y>",
        syntax_en: "menu(m) | ui <up|down|left|right|pgup|pgdn|home|end|ok|back|menu> | click <x> <y>",
        summary_hans: "操作原版游戏内层级菜单与界面按键",
        summary_hant: "操作原版遊戲內層級選單與介面按鍵",
        summary_en: "Navigate the real in-game hierarchical RPG menus and UI keys",
        details_hans: &[
            "• menu (无参数 m) 在探索模式打开主菜单；带参数时 m <id|名称> 等同于 magic。",
            "• ui <up|down|left|right|pgup|pgdn|home|end|ok|back|menu>（或直接输入 up/down/left/right/ok/back）操作菜单光标、列表翻页与战斗八卦盘（战斗中也支持 repeat/auto/defend/useitem/throwitem/flee/force/status）。",
            "• click <x> <y>：按 320x200 逻辑坐标点击画面。",
        ],
        details_hant: &[
            "• menu (無參數 m) 在探索模式打開主選單；帶參數時 m <id|名稱> 等同於 magic。",
            "• ui <up|down|left|right|pgup|pgdn|home|end|ok|back|menu>（或直接輸入 up/down/left/right/ok/back）操作選單游標、列表翻頁與戰鬥八卦盤（戰鬥中也支援 repeat/auto/defend/useitem/throwitem/flee/force/status）。",
            "• click <x> <y>：按 320x200 邏輯座標點擊畫面。",
        ],
        details_en: &[
            "• menu (bare 'm') opens the in-game Root Menu in explore mode ('m <id|name>' casts magic).",
            "• ui <up|down|left|right|pgup|pgdn|home|end|ok|back|menu> (or bare up/down/left/right/back) drives menu cursors, 7-row paging & the battle Bagua wheel (battle shortcuts: repeat/auto/defend/useitem/throwitem/flee/force/status).",
            "• click <x> <y>: Click logical 320x200 screen coordinates.",
        ],
        examples: &["menu", "ui down", "ui ok", "ui back"],
    },
    HelpEntry {
        topic: "save",
        aliases: &["load", "new", "replay-export", "export-replay", "replay", "bugreport", "report", "bug"],
        category: "session",
        fair_allowed: true,
        syntax_zh: "new [seed] | save [path.json] | load <path> | replay-export [path.repal] | bugreport [desc]",
        syntax_en: "new [seed] | save [path.json] | load <path> | replay-export [path.repal] | bugreport [desc]",
        summary_hans: "存档、读档、开新局与导出录像",
        summary_hant: "存檔、讀檔、開新局與匯出錄像",
        summary_en: "Start new game, save/load checkpoints, and export deterministic replays",
        details_hans: &[
            "• save [path.json]：仅在 explore（自由探索）模式下允许存档；对话或战斗中会明确拒绝并返回 wrong_mode 错误。",
            "• load <path>：读取 JSON 存档或原版 DOS .rpg 存档（会新开一段录像分段，不会丢失先前录制）。",
            "• replay-export [path.repal]：导出当前会话的确定性 .repal 录像（包含所有分段与 AI think 字幕轨道）。",
        ],
        details_hant: &[
            "• save [path.json]：僅在 explore（自由探索）模式下允許存檔；對話或戰鬥中會明確拒絕並返回 wrong_mode 錯誤。",
            "• load <path>：讀取 JSON 存檔或原版 DOS .rpg 存檔（會新開一段錄像分段，不會遺失先前錄製）。",
            "• replay-export [path.repal]：匯出當前會話的確定性 .repal 錄像（包含所有分段與 AI think 字幕軌道）。",
        ],
        details_en: &[
            "• save [path.json]: Only allowed in 'explore' mode (rejected cleanly during dialogue/battle instead of writing corrupt state).",
            "• load <path>: Load a JSON checkpoint or original DOS .rpg save (creates a clean new replay segment without discarding prior segments).",
            "• replay-export [path.repal]: Export the session's deterministic .repal archive (including segments & AI thought track).",
        ],
        examples: &["save checkpoint.json", "load checkpoint.json", "replay-export run.repal"],
    },
    HelpEntry {
        topic: "display",
        aliases: &["hd", "retro", "lang", "think", "thought", "shot", "screenshot", "ss", "take_screenshot", "golden"],
        category: "display",
        fair_allowed: true,
        syntax_zh: "hd | retro | lang <hans|hant|en> | think <text|clear> | shot [--small] [--with-thought] [path.png]",
        syntax_en: "hd | retro | lang <hans|hant|en> | think <text|clear> | shot [--small] [--with-thought] [path.png]",
        summary_hans: "画质切换、语言、AI 思考字幕与 4x HD 截图",
        summary_hant: "畫質切換、語言、AI 思考字幕與 4x HD 截圖",
        summary_en: "HD/Retro display toggle, language, AI thought subtitle bar, and 4x HD screenshots",
        details_hans: &[
            "• shot [--with-thought] [path.png]：导出 1280x800 4x HD 截图（默认不含 think 字幕条）。",
            "• shot --small [path.png]：导出 640x400 小图（固定为原版 320x200 视野，公平模式可用）；文字地图分不清地形（水面/石堤）时推荐使用。",
            "• think <text | clear>：在 HD 画面顶部显示简短 AI 思考字幕并写入录像 annotations.json。",
            "• hd / retro / lang <hans|hant|en>：切换高清/复古渲染与简繁英文字。",
        ],
        details_hant: &[
            "• shot [--with-thought] [path.png]：匯出 1280x800 4x HD 截圖（預設不含 think 字幕條）。",
            "• shot --small [path.png]：匯出 640x400 小圖（固定為原版 320x200 視野，公平模式可用）；文字地圖分不清地形（水面/石堤）時推薦使用。",
            "• think <text | clear>：在 HD 畫面頂部顯示簡短 AI 思考字幕並寫入錄像 annotations.json。",
            "• hd / retro / lang <hans|hant|en>：切換高畫質/復古渲染與簡繁英文字。",
        ],
        details_en: &[
            "• shot [--with-thought] [path.png]: Render a settled 1280x800 4x HD PNG (excludes thought bar unless --with-thought is passed).",
            "• shot --small [path.png]: 640x400 PNG of the canonical 320x200 viewport (allowed in fair mode); the recommended way to read terrain (water vs causeway) when the text map is ambiguous.",
            "• think <text | clear>: Display a short HD commentary subtitle and record it into replay annotations.json.",
            "• hd / retro / lang <hans|hant|en>: Switch HD/Retro mode and language.",
        ],
        examples: &["shot frame.png", "shot --small", "think \"先回村找张四借船\"", "lang en", "hd"],
    },
    HelpEntry {
        topic: "debug",
        aliases: &["/tp", "/heal", "/god", "/cash", "/item", "/event", "/battle", "/script", "/state", "view", "dump-state", "core-replay"],
        category: "debug",
        fair_allowed: false,
        syntax_zh: "/tp <sc> [x y] | /heal | /god | /cash <n> | /item <id> [n] | /event <id> [state <n>] | /battle <g> | /script <lbl> | /state | view <name>",
        syntax_en: "/tp <sc> [x y] | /heal | /god | /cash <n> | /item <id> [n] | /event <id> [state <n>] | /battle <g> | /script <lbl> | /state | view <name>",
        summary_hans: "开发者调试与测试指令（仅 --obs debug 本地模式可用）",
        summary_hant: "開發者除錯與測試指令（僅 --obs debug 本地模式可用）",
        summary_en: "Developer diagnostic & sandbox commands (disabled in --obs fair and remote TCP)",
        details_hans: &[
            "• 这些指令在 --obs fair 公平游玩模式下完全禁用；修改类调试指令会标记 debug_mode 与 tainted_by_debug。",
        ],
        details_hant: &[
            "• 這些指令在 --obs fair 公平遊玩模式下完全禁用；修改類除錯指令會標記 debug_mode 與 tainted_by_debug。",
        ],
        details_en: &[
            "• All debug/view/dump-state commands are disabled in --obs fair mode; mutating debug commands set debug_mode & tainted_by_debug.",
        ],
        examples: &["/state", "/tp 1", "view status"],
    },
];

fn pick<'a>(lang: Language, hans: &'a str, hant: &'a str, en: &'a str) -> &'a str {
    match lang {
        Language::En => en,
        Language::ZhHant => hant,
        Language::ZhHans => hans,
    }
}

fn pick_lines<'a>(
    lang: Language,
    hans: &'a [&'static str],
    hant: &'a [&'static str],
    en: &'a [&'static str],
) -> &'a [&'static str] {
    match lang {
        Language::En => en,
        Language::ZhHant => hant,
        Language::ZhHans => hans,
    }
}

/// Build localized plain-text lines and structured JSON artifact for `help [topic|cmd]`.
pub fn render_help(
    topic: Option<&str>,
    lang: Language,
    fair: bool,
) -> Result<(Vec<String>, serde_json::Value), String> {
    if let Some(raw) = topic {
        let key = raw.trim().to_ascii_lowercase();
        let entry = ENTRIES.iter().find(|e| {
            e.topic == key
                || e.category == key
                || e.aliases.iter().any(|a| a.eq_ignore_ascii_case(&key))
        });
        let Some(e) = entry else {
            let topics: Vec<&str> = ENTRIES
                .iter()
                .filter(|x| !fair || x.fair_allowed)
                .map(|x| x.topic)
                .collect();
            return Err(format!(
                "unknown help topic '{raw}' (try: {})",
                topics.join(", ")
            ));
        };
        if fair && !e.fair_allowed {
            let msg = pick(
                lang,
                "调试指令在公平模式 (--obs fair) 下已禁用。",
                "除錯指令在公平模式 (--obs fair) 下已禁用。",
                "Debug/view/dump-state commands are disabled in fair mode (--obs fair).",
            );
            let lines = vec![msg.to_string()];
            let art = serde_json::json!({
                "topic": e.topic,
                "fair_disabled": true,
                "summary": msg,
                "lines": lines,
            });
            return Ok((lines, art));
        }

        let syntax = if lang == Language::En { e.syntax_en } else { e.syntax_zh };
        let summary = pick(lang, e.summary_hans, e.summary_hant, e.summary_en);
        let details = pick_lines(lang, e.details_hans, e.details_hant, e.details_en);

        let mut lines = Vec::new();
        lines.push(format!("=== help {} — {} ===", e.topic, summary));
        lines.push(format!(
            "{}: {}",
            pick(lang, "用法", "用法", "Usage"),
            syntax
        ));
        for d in details {
            lines.push((*d).to_string());
        }
        if !e.examples.is_empty() {
            lines.push(format!(
                "{}: {}",
                pick(lang, "示例", "範例", "Examples"),
                e.examples.join("  |  ")
            ));
        }

        let art = serde_json::json!({
            "topic": e.topic,
            "category": e.category,
            "syntax": syntax,
            "summary": summary,
            "details": details,
            "examples": e.examples,
            "aliases": e.aliases,
        });
        return Ok((lines, art));
    }

    // Overview mode: concise grouped reference, filtered by `--obs fair`.
    let mut lines = Vec::new();
    lines.push(
        pick(
            lang,
            "=== RePal Unified Text Mode ===",
            "=== RePal Unified Text Mode ===",
            "=== RePal Unified Text Mode ===",
        )
        .to_string(),
    );

    let mut entries_json = Vec::new();
    for e in ENTRIES {
        if fair && !e.fair_allowed {
            continue;
        }
        let syntax = if lang == Language::En { e.syntax_en } else { e.syntax_zh };
        let summary = pick(lang, e.summary_hans, e.summary_hant, e.summary_en);
        let cat_lbl = match e.topic {
            "explore" => pick(lang, "探索", "探索", "Explore"),
            "dialog" => pick(lang, "对话", "對話", "Dialog"),
            "menu" => pick(lang, "菜单", "選單", "Menu"),
            "battle" => pick(lang, "战斗", "戰鬥", "Battle"),
            "magic" => pick(lang, "仙术", "仙術", "Magic"),
            "items" => pick(lang, "物品", "物品", "Items"),
            "status" => pick(lang, "状态", "狀態", "Status"),
            "save" => pick(lang, "存档", "存檔", "Session"),
            "display" => pick(lang, "视觉", "視覺", "Display"),
            "debug" => pick(lang, "调试", "除錯", "Debug"),
            other => other,
        };
        lines.push(format!("{cat_lbl}: {syntax}"));
        entries_json.push(serde_json::json!({
            "topic": e.topic,
            "syntax": syntax,
            "summary": summary,
        }));
    }
    lines.push(
        pick(
            lang,
            "退出: quit(q)",
            "退出: quit(q)",
            "Quit: quit(q)",
        )
        .to_string(),
    );
    lines.push(
        pick(
            lang,
            "提示: 输入 'help <命令或主题>'（如 help battle / help magic / help items / help save）查看详细用法与示例；status 与 bag 会列出所有仙术/道具的 #ID 与效果说明。",
            "提示: 輸入 'help <命令或主題>'（如 help battle / help magic / help items / help save）查看詳細用法與範例；status 與 bag 會列出所有仙術/道具的 #ID 與效果說明。",
            "Tip: Run 'help <command|topic>' (e.g. help battle, help magic, help items, help save) for detailed usage & examples; 'status' and 'bag' list every spell/item #ID and effect.",
        )
        .to_string(),
    );

    let art = serde_json::json!({
        "topic": serde_json::Value::Null,
        "obs_mode": if fair { "fair" } else { "debug" },
        "topics": entries_json.iter().filter_map(|v| v["topic"].as_str()).collect::<Vec<_>>(),
        "entries": entries_json,
    });
    Ok((lines, art))
}
