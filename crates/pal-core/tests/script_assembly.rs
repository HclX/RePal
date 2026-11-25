//! Assemble every decompiled script block and check that all jump targets resolve.

mod common;

use pal_core::script::{jump_mask, Program, NO_LABEL};

#[test]
fn all_chapters_assemble_and_every_jump_target_exists() {
    let mut prog = Program::new();
    let mut blocks = 0;
    for ch in 0..=10 {
        blocks += prog.load_chapter(ch, &common::load_yaml(&common::chapter_file(ch))).unwrap();
    }
    let index = common::load_yaml("data/scripts_index.yaml");
    assert_eq!(blocks as u64, index["total_blocks"].as_u64().unwrap());

    let mut missing = Vec::new();
    let mut steps = 0usize;
    for ch in 0..=10 {
        let doc = common::load_yaml(&common::chapter_file(ch));
        for (lbl, _) in doc["scripts"].as_object().unwrap() {
            let id = prog.label_id(lbl).unwrap();
            for ins in prog.block(id).unwrap() {
                steps += 1;
                for (slot, &l) in ins.lbl.iter().enumerate() {
                    if l != NO_LABEL && !prog.has_block(l) {
                        missing.push(format!("{lbl}: {} slot {slot}", prog.label_name(l)));
                    }
                    // Label operands may only appear in jump slots of the opcode table.
                    if l != NO_LABEL {
                        assert!(jump_mask(ins.op) & (1 << slot) != 0, "{lbl}: op {:#06x} slot {slot}", ins.op);
                    }
                }
            }
        }
    }
    assert!(missing.is_empty(), "unresolved targets: {missing:?}");
    assert!(steps > 30_000, "only {steps} steps assembled");
}
