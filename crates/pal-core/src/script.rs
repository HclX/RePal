//! Script program model: assembled instructions and lazily-loaded label blocks.
//!
//! The decompiled `site/assets/data/scripts/*.yaml` files describe each basic block as a list of
//! structured steps (`op: say`, `op: jump` + `target: s_1234`, `op: npc_walk_to` + `args: [..]`).
//! [`Program::load_block`] re-assembles those steps into uniform [`Instr`] records whose operands
//! follow the original 1995 `SCRIPTENTRY` layout (the inverse of
//! `tools/pal_pipeline/decompiler.py::format_step`), so the VM never looks at YAML field names.
//!
//! Blocks are keyed by label (`s_XXXX`, where `XXXX` is the original script address). Chapters are
//! loaded on demand, so jump targets are stored as interned [`LabelId`]s and resolved at run time.

use crate::script_ops::OPCODES;
use serde_json::Value;
use std::collections::HashMap;

/// Interned label handle. `0` means "no label" (an empty jump target).
pub type LabelId = u32;
pub const NO_LABEL: LabelId = 0;

/// Well-known opcodes referenced by the VM and the assembler.
pub mod op {
    pub const END: u16 = 0x0000;
    pub const PAUSE_YIELD: u16 = 0x0001;
    pub const REPLACE_AND_YIELD: u16 = 0x0002;
    pub const JUMP: u16 = 0x0003;
    pub const CALL_SCRIPT: u16 = 0x0004;
    pub const BATTLE_START: u16 = 0x0007;
    pub const CONFIRM_CHOICE: u16 = 0x000A;
    pub const CASH_ADD: u16 = 0x001E;
    pub const ITEM_ADD: u16 = 0x001F;
    pub const ITEM_REMOVE: u16 = 0x0020;
    pub const EVENT_SET_AUTO_SCRIPT: u16 = 0x0024;
    pub const EVENT_SET_TRIGGER_SCRIPT: u16 = 0x0025;
    pub const DIALOGUE_POS_CENTER: u16 = 0x003B;
    pub const DIALOGUE_POS_UPPER: u16 = 0x003C;
    pub const DIALOGUE_POS_LOWER: u16 = 0x003D;
    pub const SCRIPT_FAIL: u16 = 0x0041;
    pub const MUSIC_PLAY: u16 = 0x0043;
    pub const PARTY_SET_POS: u16 = 0x0046;
    pub const SFX_PLAY: u16 = 0x0047;
    pub const EVENT_SET_STATE: u16 = 0x0049;
    pub const BRANCH_IF_ITEM_LESS: u16 = 0x0058;
    pub const SCENE_CHANGE: u16 = 0x0059;
    pub const QUIT_GAME: u16 = 0x00A0;
    pub const SAY: u16 = 0xFFFF;
}

/// Look up an opcode by its decompiler mnemonic (or a raw `op_XXXX` fallback name).
pub fn opcode_by_name(name: &str) -> Option<u16> {
    if let Some(&(code, _, _)) = OPCODES.iter().find(|(_, n, _)| *n == name) {
        return Some(code);
    }
    name.strip_prefix("op_").and_then(|hex| u16::from_str_radix(hex, 16).ok())
}

/// Mnemonic for an opcode (for traces and diagnostics).
pub fn opcode_name(code: u16) -> &'static str {
    OPCODES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, n, _)| *n)
        .unwrap_or("op_unknown")
}

/// Bitmask of operand slots that hold script labels for `code`.
pub fn jump_mask(code: u16) -> u8 {
    OPCODES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, _, m)| *m)
        .unwrap_or(0)
}

/// One `say` step's dialogue payload (Simplified + Traditional + primary DOS `msg_id`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SayText {
    pub speaker: String,
    pub speaker_hant: String,
    pub text: String,
    pub text_hant: String,
    pub msg_id: u32,
}

/// One assembled script instruction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instr {
    pub op: u16,
    /// Numeric operands in original `SCRIPTENTRY` order (`a[3]` only exists for hand-written mods).
    pub a: [i32; 4],
    /// Label operands; `NO_LABEL` where the slot is not a jump target.
    pub lbl: [LabelId; 3],
    /// Number of explicit generic `args:` the YAML step carried. The decompiler strips trailing
    /// zero operands, and several legacy handlers distinguish "absent" from "0", so we keep it.
    pub argc: u8,
    /// Bitmask of operand slots explicitly present in the YAML (generic `args` or named fields).
    pub has: u8,
    /// Index into [`Program::says`] for `say` steps.
    pub text: u32,
}

impl Instr {
    /// Operand `i` if it was explicitly present, else `default` (JS `args[i] ?? default`).
    #[inline]
    pub fn arg_or(&self, i: usize, default: i32) -> i32 {
        if self.has & (1 << i) != 0 {
            self.a[i]
        } else {
            default
        }
    }

    /// Operand `i` unless it is zero/absent, else `default` (JS `args[i] || default`).
    #[inline]
    pub fn arg_nz(&self, i: usize, default: i32) -> i32 {
        if self.a[i] != 0 {
            self.a[i]
        } else {
            default
        }
    }

    /// Operand `i` reinterpreted as a signed 16-bit value.
    #[inline]
    pub fn s16(&self, i: usize) -> i32 {
        (self.a[i] as u16) as i16 as i32
    }
}

/// Position inside the script program: a block label plus a step index within that block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptPos {
    pub label: LabelId,
    pub idx: u32,
}

impl ScriptPos {
    pub const NONE: ScriptPos = ScriptPos { label: NO_LABEL, idx: 0 };

    pub fn new(label: LabelId, idx: u32) -> Self {
        Self { label, idx }
    }

    pub fn is_none(&self) -> bool {
        self.label == NO_LABEL
    }
}

/// All script blocks known so far, plus mod overrides.
#[derive(Clone, Debug)]
pub struct Program {
    names: Vec<String>,
    ids: HashMap<String, LabelId>,
    blocks: Vec<Option<Vec<Instr>>>,
    overrides: Vec<Option<Vec<Instr>>>,
    pub says: Vec<SayText>,
    loaded_chapters: u32,
    /// Sorted `(address, label)` index of loaded `s_XXXX` blocks for mid-block address resolution.
    addr_index: Vec<(u32, LabelId)>,
    addr_index_dirty: bool,
}

impl Default for Program {
    fn default() -> Self {
        Self::new()
    }
}

/// Error raised while assembling a YAML step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsmError(pub String);

impl Program {
    pub fn new() -> Self {
        Self {
            names: vec![String::new()],
            ids: HashMap::new(),
            blocks: vec![None],
            overrides: vec![None],
            says: vec![SayText::default()],
            loaded_chapters: 0,
            addr_index: Vec::new(),
            addr_index_dirty: false,
        }
    }

    /// Intern `name` and return its id (`NO_LABEL` for the empty string).
    pub fn intern(&mut self, name: &str) -> LabelId {
        if name.is_empty() {
            return NO_LABEL;
        }
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as LabelId;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        self.blocks.push(None);
        self.overrides.push(None);
        id
    }

    /// Existing id for `name` without interning.
    pub fn label_id(&self, name: &str) -> Option<LabelId> {
        if name.is_empty() {
            return Some(NO_LABEL);
        }
        self.ids.get(name).copied()
    }

    pub fn label_name(&self, id: LabelId) -> &str {
        self.names.get(id as usize).map(String::as_str).unwrap_or("")
    }

    /// Chapter file index (`0..=10`) that holds `label`, from its `s_XXXX` address.
    pub fn chapter_for_label(name: &str) -> Option<u32> {
        let hex = name.strip_prefix("s_")?;
        let addr = u32::from_str_radix(hex, 16).ok()?;
        Some((addr / 0x1000).min(10))
    }

    pub fn is_chapter_loaded(&self, ch: u32) -> bool {
        ch < 32 && self.loaded_chapters & (1 << ch) != 0
    }

    pub fn mark_chapter_loaded(&mut self, ch: u32) {
        if ch < 32 {
            self.loaded_chapters |= 1 << ch;
        }
    }

    /// Block steps for `id`, honouring mod overrides.
    pub fn block(&self, id: LabelId) -> Option<&[Instr]> {
        let i = id as usize;
        if let Some(Some(ov)) = self.overrides.get(i) {
            return Some(ov);
        }
        self.blocks.get(i).and_then(|b| b.as_deref())
    }

    /// Whether a dialogue line tells the player an item had no effect.
    pub fn says_item_failed(s: &SayText) -> bool {
        const NEEDLES: [&str; 6] = ["无法使用", "無法使用", "没有效果", "沒有效果", "无任何效果", "無任何效果"];
        let t = if s.text.is_empty() { &s.text_hant } else { &s.text };
        NEEDLES.iter().any(|n| t.contains(n))
    }

    pub fn has_block(&self, id: LabelId) -> bool {
        self.block(id).is_some()
    }

    pub fn instr(&self, pos: ScriptPos) -> Option<&Instr> {
        self.block(pos.label).and_then(|b| b.get(pos.idx as usize))
    }

    /// Assemble and store one block (`steps` = YAML list of step mappings as JSON).
    pub fn load_block(&mut self, label: &str, steps: &Value) -> Result<LabelId, AsmError> {
        let code = self.assemble_steps(steps)?;
        let id = self.intern(label);
        self.blocks[id as usize] = Some(code);
        self.addr_index_dirty = true;
        Ok(id)
    }

    /// Install (or replace) a mod override block.
    pub fn load_override(&mut self, label: &str, steps: &Value) -> Result<LabelId, AsmError> {
        let code = self.assemble_steps(steps)?;
        let id = self.intern(label);
        self.overrides[id as usize] = Some(code);
        Ok(id)
    }

    pub fn clear_overrides(&mut self) {
        self.overrides.fill(None);
    }

    /// Load a whole chapter document (`{ scripts: { s_XXXX: [steps..] } }`).
    pub fn load_chapter(&mut self, ch: u32, doc: &Value) -> Result<usize, AsmError> {
        let mut n = 0;
        if let Some(map) = doc.get("scripts").and_then(Value::as_object) {
            for (lbl, steps) in map {
                if steps.is_array() {
                    self.load_block(lbl, steps)?;
                    n += 1;
                }
            }
        }
        self.mark_chapter_loaded(ch);
        Ok(n)
    }

    /// Resolve a label that may point into the middle of a block (e.g. raw addresses from DOS
    /// `.rpg` saves). Mirrors `GameDataRepository.resolveScriptEntry` in the legacy JS runtime.
    pub fn resolve_entry(&mut self, name: &str) -> Option<ScriptPos> {
        if let Some(id) = self.label_id(name) {
            if self.has_block(id) {
                return Some(ScriptPos::new(id, 0));
            }
        }
        let addr = u32::from_str_radix(name.strip_prefix("s_")?, 16).ok()?;
        if self.addr_index_dirty {
            self.rebuild_addr_index();
        }
        let pos = self.addr_index.partition_point(|&(a, _)| a <= addr);
        if pos == 0 {
            return None;
        }
        let (base, id) = self.addr_index[pos - 1];
        let len = self.block(id).map(|b| b.len()).unwrap_or(0) as u32;
        if addr - base < len {
            Some(ScriptPos::new(id, addr - base))
        } else {
            None
        }
    }

    fn rebuild_addr_index(&mut self) {
        self.addr_index.clear();
        for (i, name) in self.names.iter().enumerate() {
            if self.blocks[i].as_ref().map(|b| !b.is_empty()).unwrap_or(false) {
                if let Some(addr) = name
                    .strip_prefix("s_")
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                {
                    self.addr_index.push((addr, i as LabelId));
                }
            }
        }
        self.addr_index.sort_unstable();
        self.addr_index_dirty = false;
    }

    fn assemble_steps(&mut self, steps: &Value) -> Result<Vec<Instr>, AsmError> {
        let arr = steps
            .as_array()
            .ok_or_else(|| AsmError("script block is not a list".into()))?;
        arr.iter().map(|s| self.assemble_step(s)).collect()
    }

    fn label_operand(&mut self, v: Option<&Value>) -> LabelId {
        // Presence of label slots is tracked by the caller through `lbl != NO_LABEL`.
        match v {
            Some(Value::String(s)) => self.intern(s),
            _ => NO_LABEL,
        }
    }

    /// Assemble one structured YAML step into an [`Instr`].
    pub fn assemble_step(&mut self, step: &Value) -> Result<Instr, AsmError> {
        let name = step
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| AsmError(format!("step without op: {step}")))?;
        let code = opcode_by_name(name).ok_or_else(|| AsmError(format!("unknown op `{name}`")))?;
        let mut ins = Instr {
            op: code,
            ..Default::default()
        };

        // Generic `args: [..]` (numbers, or label strings in jump slots).
        if let Some(args) = step.get("args").and_then(Value::as_array) {
            ins.argc = args.len().min(4) as u8;
            for (i, v) in args.iter().take(4).enumerate() {
                ins.has |= 1 << i;
                match v {
                    Value::String(s) if i < 3 => ins.lbl[i] = self.intern(s),
                    _ => ins.a[i] = num(v),
                }
            }
        }

        let f = |k: &str| step.get(k);
        let set_num = |ins: &mut Instr, i: usize, k: &str| {
            if let Some(v) = step.get(k) {
                ins.a[i] = num(v);
                ins.has |= 1 << i;
            }
        };
        match code {
            op::JUMP => {
                if f("target").is_some() {
                    ins.lbl[0] = self.label_operand(f("target"));
                }
                set_num(&mut ins, 1, "count");
                if f("else_jump").is_some() {
                    ins.lbl[2] = self.label_operand(f("else_jump"));
                }
            }
            op::CONFIRM_CHOICE => {
                if f("on_no").is_some() {
                    ins.lbl[0] = self.label_operand(f("on_no"));
                }
            }
            op::REPLACE_AND_YIELD => {
                ins.lbl[0] = self.label_operand(f("target"));
                set_num(&mut ins, 1, "count");
            }
            op::CALL_SCRIPT => {
                ins.lbl[0] = self.label_operand(f("target"));
                set_num(&mut ins, 1, "event_id");
            }
            op::MUSIC_PLAY => {
                set_num(&mut ins, 0, "track");
                if let Some(lp) = f("loop").and_then(Value::as_bool) {
                    ins.a[1] = if lp { 0 } else { 1 };
                }
            }
            op::SFX_PLAY => set_num(&mut ins, 0, "sfx"),
            op::DIALOGUE_POS_CENTER => set_num(&mut ins, 0, "font_color"),
            op::DIALOGUE_POS_UPPER | op::DIALOGUE_POS_LOWER => {
                set_num(&mut ins, 0, "portrait_id");
                set_num(&mut ins, 1, "font_color");
            }
            op::ITEM_ADD => {
                set_num(&mut ins, 0, "item_id");
                set_num(&mut ins, 1, "count");
            }
            op::ITEM_REMOVE | op::BRANCH_IF_ITEM_LESS => {
                set_num(&mut ins, 0, "item_id");
                set_num(&mut ins, 1, "count");
                let key = if code == op::ITEM_REMOVE { "else_jump" } else { "jump_if_less" };
                if f(key).is_some() {
                    ins.lbl[2] = self.label_operand(f(key));
                }
            }
            op::CASH_ADD => {
                set_num(&mut ins, 0, "amount");
                if f("else_jump").is_some() {
                    ins.lbl[1] = self.label_operand(f("else_jump"));
                }
            }
            op::BATTLE_START => {
                set_num(&mut ins, 0, "team_id");
                if f("on_defeat").is_some() {
                    ins.lbl[1] = self.label_operand(f("on_defeat"));
                }
                if f("on_flee").is_some() {
                    ins.lbl[2] = self.label_operand(f("on_flee"));
                }
            }
            op::SCENE_CHANGE => set_num(&mut ins, 0, "scene_id"),
            op::PARTY_SET_POS => {
                set_num(&mut ins, 0, "x");
                set_num(&mut ins, 1, "y");
                set_num(&mut ins, 2, "h");
            }
            op::EVENT_SET_STATE => {
                set_num(&mut ins, 0, "event_id");
                set_num(&mut ins, 1, "state");
            }
            op::EVENT_SET_AUTO_SCRIPT | op::EVENT_SET_TRIGGER_SCRIPT => {
                set_num(&mut ins, 0, "event_id");
                if f("target").is_some() {
                    ins.lbl[1] = self.label_operand(f("target"));
                }
            }
            op::SAY => {
                let s = |k: &str| step.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let msg_id = step
                    .get("msg_ids")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                self.says.push(SayText {
                    speaker: s("speaker"),
                    speaker_hant: s("speaker_hant"),
                    text: s("text"),
                    text_hant: s("text_hant"),
                    msg_id,
                });
                ins.text = (self.says.len() - 1) as u32;
            }
            _ => {}
        }
        if code == op::EVENT_SET_STATE
            || code == op::EVENT_SET_AUTO_SCRIPT
            || code == op::EVENT_SET_TRIGGER_SCRIPT
        {
            // The decompiler writes `event_id: -1` for the raw `0xFFFF` "current event" marker.
            if ins.a[0] == -1 {
                ins.a[0] = 0xFFFF;
            }
        }
        Ok(ins)
    }
}

fn num(v: &Value) -> i32 {
    match v {
        Value::Number(n) => n.as_i64().map(|x| x as i32).unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i32),
        Value::Bool(b) => i32::from(*b),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembles_named_fields_into_original_operand_layout() {
        let mut p = Program::new();
        let id = p
            .load_block(
                "s_0100",
                &json!([
                    {"op": "cash_add", "amount": -50, "else_jump": "s_0200"},
                    {"op": "item_remove", "item_id": 61, "count": 2, "else_jump": "s_0300"},
                    {"op": "battle_start", "team_id": 7, "on_flee": "s_0400"},
                    {"op": "npc_walk_to", "args": [12, 34, 1]},
                    {"op": "branch_random", "args": [30, "s_0500"]},
                    {"op": "say", "speaker": "李逍遥", "text": "嗯", "text_hant": "嗯"},
                    {"op": "op_0054"},
                    {"op": "end"}
                ]),
            )
            .unwrap();
        let b = p.block(id).unwrap();
        assert_eq!(b[0].op, 0x001E);
        assert_eq!(b[0].a[0], -50);
        assert_eq!(p.label_name(b[0].lbl[1]), "s_0200");
        assert_eq!((b[1].a[0], b[1].a[1]), (61, 2));
        assert_eq!(p.label_name(b[1].lbl[2]), "s_0300");
        assert_eq!(b[2].lbl[1], NO_LABEL);
        assert_eq!(p.label_name(b[2].lbl[2]), "s_0400");
        assert_eq!((b[3].a, b[3].argc), ([12, 34, 1, 0], 3));
        assert_eq!(b[3].arg_or(2, 9), 1);
        assert_eq!(b[3].arg_or(3, 9), 9);
        assert_eq!(p.label_name(b[4].lbl[1]), "s_0500");
        assert_eq!(p.says[b[5].text as usize].speaker, "李逍遥");
        assert_eq!(b[6].op, 0x0054);
        assert!(!p.has_block(b[0].lbl[1]), "jump targets resolve lazily");
    }

    #[test]
    fn resolves_mid_block_addresses_like_legacy_runtime() {
        let mut p = Program::new();
        p.load_block("s_0010", &json!([{"op": "end"}, {"op": "end"}, {"op": "end"}])).unwrap();
        let pos = p.resolve_entry("s_0012").unwrap();
        assert_eq!((p.label_name(pos.label), pos.idx), ("s_0010", 2));
        assert!(p.resolve_entry("s_0013").is_none());
        assert_eq!(Program::chapter_for_label("s_3a10"), Some(3));
    }
}
