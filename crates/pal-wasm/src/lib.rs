//! `pal-wasm`: the browser build of PalRemake (`site/wasm/pal.wasm`).
//!
//! Thin C-ABI wrapper over the shared `pal-runtime` crate (`pal-core` + `pal-render` + `pal-ui`
//! + `DualSynth`). JavaScript only feeds assets, input, and audio output through these exports.

use pal_core::game::{HostReply, TickInput};
use pal_core::synth::DualSynth;
use pal_render::cache::AssetKind;
use pal_runtime::PalRuntime;
use pal_ui::{PointerEvent, PointerKind, UiAction, UiKey, UiView};
use std::cell::RefCell;
use std::collections::VecDeque;

mod agent;
mod core_host;

struct EngineState {
    rt: PalRuntime,
    present_fb: pal_render::framebuffer::Framebuffer,
    audio_l: Vec<f32>,
    audio_r: Vec<f32>,
    scratch: Vec<u8>,
    key_scratch: Vec<u8>,
    req_buf: Vec<u8>,
    core_buf: Vec<u8>,
    pending_ui_actions: VecDeque<UiAction>,
    last_action_args: [u32; 2],
    sha256: pal_runtime::crypto::Sha256,
    agent: agent::WebAgentState,
}

impl EngineState {
    fn new() -> Self {
        Self {
            rt: PalRuntime::new(true),
            present_fb: pal_render::framebuffer::Framebuffer::new(1, 1),
            audio_l: vec![0.0; 4096],
            audio_r: vec![0.0; 4096],
            scratch: vec![0u8; 131_072],
            key_scratch: vec![0u8; 1024],
            req_buf: Vec::with_capacity(256),
            core_buf: Vec::with_capacity(256),
            pending_ui_actions: VecDeque::new(),
            last_action_args: [0, 0],
            sha256: pal_runtime::crypto::Sha256::new(),
            agent: agent::WebAgentState::new(),
        }
    }
}

thread_local! {
    static STATE: RefCell<EngineState> = RefCell::new(EngineState::new());
}

fn shrink_scratch_if_huge(scratch: &mut Vec<u8>) {
    const MAX_RETAINED_SCRATCH: usize = 4 * 1024 * 1024;
    const BASELINE_SCRATCH: usize = 256 * 1024;
    if scratch.capacity() > MAX_RETAINED_SCRATCH {
        scratch.truncate(BASELINE_SCRATCH);
        scratch.shrink_to(BASELINE_SCRATCH);
    }
}

#[no_mangle]
pub extern "C" fn wasm_scratch_ptr(min_len: usize) -> *mut u8 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if st.scratch.len() < min_len {
            st.scratch.resize(min_len, 0);
        }
        st.scratch.as_mut_ptr()
    })
}

#[no_mangle]
pub extern "C" fn wasm_synth_init(sample_rate: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let hd = st.rt.synth.hd_mode;
        let vol = st.rt.synth.master_volume;
        st.rt.synth = DualSynth::new(sample_rate);
        st.rt.synth.hd_mode = hd;
        st.rt.synth.master_volume = vol;
    });
}

/// Load MIDI file of `len` bytes from `wasm_scratch_ptr(len)` and start playing.
#[no_mangle]
pub extern "C" fn wasm_synth_load_midi(len: usize, loop_play: u32) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if len == 0 || len > st.scratch.len() {
            return 0;
        }
        let midi_copy = st.scratch[..len].to_vec();
        u32::from(st.rt.synth.load_midi(&midi_copy, loop_play != 0))
    })
}

#[no_mangle]
pub extern "C" fn wasm_synth_stop() {
    STATE.with(|cell| cell.borrow_mut().rt.synth.stop());
}

/// Switch live between HD Ancient Chinese Acoustic Synth (`hd = 1`) and 1995 OPL2 FM Synth (`hd = 0`).
#[no_mangle]
pub extern "C" fn wasm_synth_set_hd_mode(hd: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.synth.hd_mode = hd != 0;
    });
}

#[no_mangle]
pub extern "C" fn wasm_synth_set_volume(vol: f32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.synth.master_volume = vol.clamp(0.0, 1.2);
    });
}

#[no_mangle]
pub extern "C" fn wasm_synth_render(frames: usize) -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let n = frames.min(4096);
        if st.audio_l.len() < n {
            st.audio_l.resize(n, 0.0);
            st.audio_r.resize(n, 0.0);
        }
        let EngineState {
            ref mut rt,
            ref mut audio_l,
            ref mut audio_r,
            ..
        } = *st;
        rt.synth.render_stereo(&mut audio_l[..n], &mut audio_r[..n]);
        n
    })
}

#[no_mangle]
pub extern "C" fn wasm_audio_left_ptr() -> *const f32 {
    STATE.with(|cell| cell.borrow().audio_l.as_ptr())
}

#[no_mangle]
pub extern "C" fn wasm_audio_right_ptr() -> *const f32 {
    STATE.with(|cell| cell.borrow().audio_r.as_ptr())
}

// ============================================================================
// Full-WASM Framebuffer Renderer, Bounded Asset Cache & UI Input C ABI Exports
// ============================================================================

fn encode_ui_action(action: &UiAction) -> (u32, u32, u32) {
    match *action {
        UiAction::None => (0, 0, 0),
        UiAction::NewGame => (1, 0, 0),
        UiAction::LoadSlot(s) => (2, s as u32, 0),
        UiAction::SaveSlot(s) => (3, s as u32, 0),
        UiAction::ToggleHdMode => (4, 0, 0),
        UiAction::ToggleMusic => (5, 0, 0),
        UiAction::ToggleSfx => (6, 0, 0),
        UiAction::ToggleLanguage => (20, 0, 0),
        UiAction::CopyReplayReport => (21, 0, 0),
        UiAction::ExportSave => (22, 0, 0),
        UiAction::ImportSave => (23, 0, 0),
        UiAction::TakeScreenshot => (24, 0, 0),
        UiAction::ToggleFullscreen => (25, 0, 0),
        UiAction::ToggleAgentMode => (26, 0, 0),
        UiAction::CopyAgentToken | UiAction::RefreshAgentToken => (0, 0, 0),
        UiAction::CheckForUpdate => (37, 0, 0),
        UiAction::DownloadAllAssets => (0, 0, 0),
        UiAction::RestartRecording => (27, 0, 0),
        UiAction::ExportReplay => (28, 0, 0),
        UiAction::OpenReplay => (29, 0, 0),
        UiAction::ReplayJumpChapter(idx) => (30, idx as u32, 0),
        UiAction::ReplayTakeOver => (31, 0, 0),
        UiAction::ReplayRestart => (32, 0, 0),
        UiAction::ReturnToTitle => (7, 0, 0),
        UiAction::DialogAdvance => (8, 0, 0),
        UiAction::DialogPickChoice(i) => (9, i as u32, 0),
        UiAction::UseItem { item_id, role_idx } => (10, item_id as u32, role_idx as u32),
        UiAction::EquipItem { item_id, slot, .. } => (11, item_id as u32, slot as u32),
        UiAction::CastMagic { magic_id, role_idx } => (12, magic_id as u32, role_idx as u32),
        UiAction::BuyItem { item_id, price } => (13, item_id as u32, price),
        UiAction::SellItem { item_id, price } => (14, item_id as u32, price),
        UiAction::BattleAttack { enemy_idx } => (15, enemy_idx as u32, 0),
        UiAction::BattleCastMagic { magic_id, enemy_idx } => (16, magic_id as u32, enemy_idx as u32),
        UiAction::BattleCoop { enemy_idx } => (33, enemy_idx as u32, 0),
        UiAction::BattleUseItem { item_id } => (17, item_id as u32, 0),
        UiAction::BattleUseItemTarget { item_id, target } => (34, item_id as u32, target as u32),
        UiAction::BattleDefend => (18, 0, 0),
        UiAction::BattleFlee => (19, 0, 0),
        UiAction::BattleAuto => (35, 0, 0),
        UiAction::BattleCancelAuto => (36, 0, 0),
    }
}

#[no_mangle]
pub extern "C" fn wasm_key_scratch_ptr(min_len: usize) -> *mut u8 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if st.key_scratch.len() < min_len {
            st.key_scratch.resize(min_len, 0);
        }
        st.key_scratch.as_mut_ptr()
    })
}

#[no_mangle]
pub extern "C" fn wasm_set_hd_mode(hd: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.set_hd_mode(hd != 0);
    });
}

#[no_mangle]
pub extern "C" fn wasm_set_fbp_override(fbp_id: i32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.world.fbp_override =
            if fbp_id >= 0 { Some(fbp_id as u16) } else { None };
    });
}

#[no_mangle]
pub extern "C" fn wasm_set_rng_frame(active: u32, rng_id: u32, frame_idx: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.world.active_rng = if active != 0 {
            Some((rng_id as u16, frame_idx as u16))
        } else {
            None
        };
    });
}

#[no_mangle]
pub extern "C" fn wasm_set_click_ripple(active: u32, wx: i32, wy: i32, age_milli: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.world.click_ripple = if active != 0 {
            Some((wx, wy, (age_milli.min(1000) as f32) / 1000.0))
        } else {
            None
        };
    });
}

#[no_mangle]
pub extern "C" fn wasm_fb_width() -> u32 {
    STATE.with(|cell| cell.borrow().rt.compositor.fb.width)
}

#[no_mangle]
pub extern "C" fn wasm_fb_height() -> u32 {
    STATE.with(|cell| cell.borrow().rt.compositor.fb.height)
}

#[no_mangle]
pub extern "C" fn wasm_render_frame() -> *const u8 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if st.rt.ui.dirty {
            st.rt.compositor.mark_ui_dirty();
            st.rt.ui.dirty = false;
        }
        st.rt.render_frame();
        if st.rt.has_visible_ai_thought_overlay() {
            let EngineState {
                ref mut rt,
                ref mut present_fb,
                ..
            } = *st;
            let w = rt.compositor.fb.width;
            let h = rt.compositor.fb.height;
            if present_fb.width != w || present_fb.height != h {
                present_fb.resize(w, h);
            }
            present_fb
                .pixels
                .copy_from_slice(&rt.compositor.fb.pixels);
            rt.draw_ai_thought_overlay_onto(present_fb);
            present_fb.pixels.as_ptr()
        } else {
            st.rt.compositor.fb.pixels.as_ptr()
        }
    })
}

/// Poll next missing asset request. Returns `AssetKind as u32` or `0` if none pending.
#[no_mangle]
pub extern "C" fn wasm_poll_asset_request() -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(req) = st.rt.compositor.cache.pop_request() {
            st.req_buf.clear();
            st.req_buf.extend_from_slice(req.key.as_bytes());
            req.kind as u32
        } else {
            st.req_buf.clear();
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn wasm_asset_req_ptr() -> *const u8 {
    STATE.with(|cell| cell.borrow().req_buf.as_ptr())
}

#[no_mangle]
pub extern "C" fn wasm_asset_req_len() -> usize {
    STATE.with(|cell| cell.borrow().req_buf.len())
}

#[no_mangle]
pub extern "C" fn wasm_upload_texture(key_len: usize, w: u32, h: u32, rgba_len: usize, pinned: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if key_len == 0 || key_len > st.key_scratch.len() || rgba_len > st.scratch.len() {
            return;
        }
        let Ok(key) = std::str::from_utf8(&st.key_scratch[..key_len]) else {
            return;
        };
        let key = key.to_string();
        let px_len = rgba_len;
        let pixels = if px_len > 1024 * 1024 {
            let mut moved = std::mem::take(&mut st.scratch);
            moved.truncate(px_len);
            moved.shrink_to_fit();
            moved
        } else {
            let v = st.scratch[..px_len].to_vec();
            shrink_scratch_if_huge(&mut st.scratch);
            v
        };
        st.rt
            .compositor
            .cache
            .upload_texture(key, w, h, pixels, pinned != 0);
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn wasm_upload_hd_map_json(key_len: usize, json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if key_len == 0 || key_len > st.key_scratch.len() || json_len > st.scratch.len() {
            return 0;
        }
        let Ok(key) = std::str::from_utf8(&st.key_scratch[..key_len]) else {
            return 0;
        };
        let key = key.to_string();
        let Ok(json_str) = std::str::from_utf8(&st.scratch[..json_len]) else {
            return 0;
        };
        let json_owned = json_str.to_string();
        shrink_scratch_if_huge(&mut st.scratch);
        let ok = st.rt.compositor.cache.upload_hd_map_json(key, &json_owned);
        u32::from(ok)
    })
}

#[no_mangle]
pub extern "C" fn wasm_upload_atlas_json(key_len: usize, json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if key_len == 0 || key_len > st.key_scratch.len() || json_len > st.scratch.len() {
            return 0;
        }
        let Ok(key) = std::str::from_utf8(&st.key_scratch[..key_len]) else {
            return 0;
        };
        let key = key.to_string();
        let Ok(json_str) = std::str::from_utf8(&st.scratch[..json_len]) else {
            return 0;
        };
        let json_owned = json_str.to_string();
        let ok = st.rt.compositor.cache.upload_atlas_json(key, &json_owned);
        if ok {
            st.rt.compositor.mark_ui_dirty();
            1
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn wasm_upload_map_bin(key_len: usize, bin_len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if key_len == 0 || key_len > st.key_scratch.len() || bin_len > st.scratch.len() {
            return;
        }
        let Ok(key) = std::str::from_utf8(&st.key_scratch[..key_len]) else {
            return;
        };
        let key = key.to_string();
        let bytes = st.scratch[..bin_len].to_vec();
        st.rt.compositor.cache.upload_map_binary(key, bytes);
    });
}

#[no_mangle]
pub extern "C" fn wasm_upload_font_ttf(len: usize, is_fallback: u32) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if len == 0 || len > st.scratch.len() {
            return 0;
        }
        let bytes = if len > 1024 * 1024 {
            let mut moved = std::mem::take(&mut st.scratch);
            moved.truncate(len);
            moved
        } else {
            let v = st.scratch[..len].to_vec();
            shrink_scratch_if_huge(&mut st.scratch);
            v
        };
        let ok = if is_fallback != 0 {
            st.rt.compositor.fonts.load_fallback_ttf(&bytes)
        } else {
            st.rt.compositor.fonts.load_primary_ttf(&bytes)
        };
        if ok {
            st.rt.compositor.mark_ui_dirty();
            1
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn wasm_mark_asset_failed(kind: u32, key_len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let k = match kind {
            1 => AssetKind::Texture,
            2 => AssetKind::AtlasMeta,
            3 => AssetKind::MapBinary,
            4 => AssetKind::HdMapMeta,
            _ => AssetKind::FontTtf,
        };
        let key_slice = &st.key_scratch[..key_len.min(st.key_scratch.len())];
        if let Ok(key) = std::str::from_utf8(key_slice) {
            let key = key.to_string();
            st.rt.compositor.cache.mark_failed(k, &key);
        }
    });
}

#[no_mangle]
pub extern "C" fn wasm_has_texture(key_len: usize) -> u32 {
    STATE.with(|cell| {
        let st = cell.borrow();
        let key_slice = &st.key_scratch[..key_len.min(st.key_scratch.len())];
        match std::str::from_utf8(key_slice) {
            Ok(key) if st.rt.compositor.cache.has_texture(key) => 1,
            _ => 0,
        }
    })
}

#[no_mangle]
pub extern "C" fn wasm_mark_asset_missing_permanent(kind: u32, key_len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let k = match kind {
            1 => AssetKind::Texture,
            2 => AssetKind::AtlasMeta,
            3 => AssetKind::MapBinary,
            4 => AssetKind::HdMapMeta,
            _ => AssetKind::FontTtf,
        };
        let key_slice = &st.key_scratch[..key_len.min(st.key_scratch.len())];
        if let Ok(key) = std::str::from_utf8(key_slice) {
            let key = key.to_string();
            st.rt.compositor.cache.mark_missing_permanent(k, &key);
        }
    });
}

#[no_mangle]
pub extern "C" fn wasm_ui_set_view(view_code: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let v = match view_code {
            1 => UiView::Title,
            2 => UiView::RootMenu,
            3 => UiView::StatusFbp000,
            4 => UiView::EquipFbp001,
            5 => UiView::MagicGrid,
            6 => UiView::ItemGrid,
            7 => UiView::SystemSubmenu,
            8 => UiView::PawnshopSell,
            _ => UiView::None,
        };
        if v == UiView::Title {
            st.rt.world.fbp_override = Some(60);
        }
        st.rt.ui.set_view(v);
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn wasm_ui_is_modal_open() -> u32 {
    STATE.with(|cell| u32::from(cell.borrow().rt.ui.is_modal_open()))
}

#[no_mangle]
pub extern "C" fn wasm_ui_on_key(key_code: u32) -> u32 {
    let Some(key) = UiKey::from_u32(key_code) else {
        return 0;
    };
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let Some(action) = st.rt.on_key(key) else {
            return 0;
        };
        let (code, a0, a1) = encode_ui_action(&action);
        if code != 0 {
            st.last_action_args = [a0, a1];
            st.pending_ui_actions.push_back(action);
        }
        code
    })
}

#[no_mangle]
pub extern "C" fn wasm_ui_on_pointer(kind: u32, x: i32, y: i32) -> u32 {
    let ev = PointerEvent {
        kind: PointerKind::from_u32(kind),
        x,
        y,
    };
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let Some(action) = st.rt.on_pointer(ev) else {
            return 0;
        };
        let (code, a0, a1) = encode_ui_action(&action);
        if code != 0 {
            st.last_action_args = [a0, a1];
            st.pending_ui_actions.push_back(action);
        }
        code
    })
}

#[no_mangle]
pub extern "C" fn wasm_ui_poll_action() -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(act) = st.pending_ui_actions.pop_front() {
            let (code, a0, a1) = encode_ui_action(&act);
            st.last_action_args = [a0, a1];
            code
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn wasm_ui_last_action_arg0() -> u32 {
    STATE.with(|cell| cell.borrow().last_action_args[0])
}

#[no_mangle]
pub extern "C" fn wasm_ui_last_action_arg1() -> u32 {
    STATE.with(|cell| cell.borrow().last_action_args[1])
}

#[no_mangle]
pub extern "C" fn wasm_ui_set_toast(text_len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if text_len == 0 {
            st.rt.ui.toast_text = None;
        } else {
            let text = std::str::from_utf8(&st.key_scratch[..text_len.min(st.key_scratch.len())])
                .unwrap_or("")
                .to_string();
            st.rt.ui.toast_text = Some(text);
        }
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn wasm_ui_set_save_slot(slot: u32, exists: u32, detail_len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let detail = std::str::from_utf8(&st.key_scratch[..detail_len.min(st.key_scratch.len())])
            .unwrap_or("空")
            .to_string();
        if let Some(s) = st.rt.ui.save_slots.get_mut(slot as usize) {
            s.exists = exists != 0;
            s.detail = detail;
            st.rt.compositor.mark_ui_dirty();
        }
    });
}

// =====================================================================================
// Core runtime exports (backed by `pal-runtime`)
// =====================================================================================

fn scratch_str(st: &EngineState, len: usize) -> String {
    String::from_utf8_lossy(&st.scratch[..len.min(st.scratch.len())]).into_owned()
}

fn key_str(st: &EngineState, len: usize) -> String {
    String::from_utf8_lossy(&st.key_scratch[..len.min(st.key_scratch.len())]).into_owned()
}

fn core_write(st: &mut EngineState, bytes: &[u8]) -> usize {
    st.core_buf.clear();
    st.core_buf.extend_from_slice(bytes);
    bytes.len()
}

#[no_mangle]
pub extern "C" fn core_buf_ptr() -> *const u8 {
    STATE.with(|cell| cell.borrow().core_buf.as_ptr())
}

/// Write the compiled `pal-core` engine semver (`ENGINE_VERSION`, e.g. `"0.4.0"`) into `core_buf`.
#[no_mangle]
pub extern "C" fn core_engine_version() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        core_write(&mut st, pal_core::game::replay_rec::ENGINE_VERSION.as_bytes())
    })
}

#[no_mangle]
pub extern "C" fn core_load_table(name_len: usize, json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let name = key_str(&st, name_len);
        match serde_json::from_str::<serde_json::Value>(&scratch_str(&st, json_len)) {
            Ok(v) => {
                st.rt.data.load_table(&name, &v);
                1
            }
            Err(_) => 0,
        }
    })
}

#[no_mangle]
pub extern "C" fn core_start() {
    STATE.with(|cell| {
        cell.borrow_mut().rt.init_game();
    })
}

#[no_mangle]
pub extern "C" fn core_is_active() -> u32 {
    STATE.with(|cell| u32::from(cell.borrow().rt.active()))
}

#[no_mangle]
pub extern "C" fn core_load_chapter(ch: u32, json_len: usize) -> i32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let text = scratch_str(&st, json_len);
        let Some(g) = st.rt.game.as_mut() else { return -1 };
        match serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| e.to_string())
            .and_then(|v| g.load_chapter(ch, &v))
        {
            Ok(n) => n as i32,
            Err(_) => -1,
        }
    })
}

#[no_mangle]
pub extern "C" fn core_load_override(label_len: usize, json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let label = key_str(&st, label_len);
        let text = scratch_str(&st, json_len);
        let Some(g) = st.rt.game.as_mut() else { return 0 };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => u32::from(g.prog.load_override(&label, &v).is_ok()),
            Err(_) => 0,
        }
    })
}

#[no_mangle]
pub extern "C" fn core_clear_overrides() {
    STATE.with(|cell| {
        if let Some(g) = cell.borrow_mut().rt.game.as_mut() {
            g.prog.clear_overrides();
        }
    })
}

#[no_mangle]
pub extern "C" fn core_new_game(seed: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.new_game(seed);
    })
}

#[no_mangle]
pub extern "C" fn core_set_lang(code: u32) {
    STATE.with(|cell| {
        cell.borrow_mut().rt.set_lang(pal_core::Language::from_code(code));
    })
}

#[no_mangle]
pub extern "C" fn core_load_i18n_en(json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let text = scratch_str(&st, json_len);
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => {
                st.rt.load_i18n_en(&v);
                1
            }
            Err(_) => 0,
        }
    })
}

#[no_mangle]
pub extern "C" fn core_set_in_title(flag: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let in_title = flag != 0;
        if in_title {
            st.rt.reset_session();
            st.rt.ui.debug_mode = false;
        }
        if let Some(g) = st.rt.game.as_mut() {
            g.in_title = in_title;
            if in_title {
                g.abort_foreground();
                g.debug_mode = false;
                g.god_mode = false;
                g.recorder.debug_mode = false;
                g.recorder.tainted_by_debug = false;
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn core_set_map(len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if len == 0 || len > st.scratch.len() {
            return 0;
        }
        let bytes = st.scratch[..len].to_vec();
        st.rt
            .game
            .as_mut()
            .map(|g| u32::from(g.set_map_collision(&bytes)))
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn core_set_sprite_frames(sprite_id: u32, frames: u32) {
    STATE.with(|cell| {
        if let Some(g) = cell.borrow_mut().rt.game.as_mut() {
            g.set_sprite_frame_count(sprite_id as u16, frames as i32);
        }
    })
}

#[no_mangle]
pub extern "C" fn core_scene_sprite_ids() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let ids = st.rt.scene_sprite_ids();
        let bytes: Vec<u8> = ids.iter().flat_map(|v| v.to_le_bytes()).collect();
        core_write(&mut st, &bytes);
        ids.len()
    })
}

#[no_mangle]
pub extern "C" fn core_reply(kind: u32, arg: u32) {
    STATE.with(|cell| {
        let r = match kind {
            1 => HostReply::DialogueDone,
            2 => HostReply::Choice(arg != 0),
            3 => HostReply::ModalDone,
            4 => HostReply::SceneLoaded,
            _ => HostReply::Done,
        };
        cell.borrow_mut().rt.reply(r);
    })
}

#[no_mangle]
pub extern "C" fn core_tick(held_dir: i32, flags: u32, cx: i32, cy: i32, sx: i32, sy: i32) {
    STATE.with(|cell| {
        let input = TickInput {
            held_dir: held_dir.clamp(-1, 3) as i8,
            interact: flags & 1 != 0,
            click: (flags & 2 != 0).then_some((cx, cy)),
            steer: (flags & 4 != 0).then_some((sx, sy)),
        };
        cell.borrow_mut().rt.tick(input);
    })
}

#[no_mangle]
pub extern "C" fn core_poll_cmd() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        while let Some(cmd) = st.rt.out_cmds.pop_front() {
            if let Some(s) = core_host::host_cmd_to_json(cmd) {
                return core_write(&mut st, s.as_bytes());
            }
        }
        0
    })
}

#[no_mangle]
pub extern "C" fn core_is_busy() -> u32 {
    STATE.with(|cell| {
        cell.borrow()
            .rt
            .game
            .as_ref()
            .map(|g| u32::from(g.is_busy() || g.is_loading_scene()))
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn core_save(note_len: usize, ts_len: usize) -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let note = key_str(&st, note_len);
        let ts = scratch_str(&st, ts_len);
        let Some(text) = st.rt.save_json_str(&note, &ts) else {
            return 0;
        };
        core_write(&mut st, text.as_bytes())
    })
}

#[no_mangle]
pub extern "C" fn core_load_save(json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let text = scratch_str(&st, json_len);
        u32::from(st.rt.load_save_json_str(&text))
    })
}

#[no_mangle]
pub extern "C" fn core_load_rpg(len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if len == 0 || len > st.scratch.len() {
            return 0;
        }
        let bytes = st.scratch[..len].to_vec();
        u32::from(st.rt.load_dos_rpg(&bytes))
    })
}

#[no_mangle]
pub extern "C" fn core_canonical_state() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let Some(g) = st.rt.game.as_ref() else { return 0 };
        let text = g.canonical_state().to_string();
        core_write(&mut st, text.as_bytes())
    })
}

#[no_mangle]
pub extern "C" fn core_leader_pos() -> u32 {
    STATE.with(|cell| {
        cell.borrow()
            .rt
            .game
            .as_ref()
            .map(|g| {
                let l = g.world.leader();
                ((l.x as u32 & 0xffff) << 16) | (l.y as u32 & 0xffff)
            })
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn core_camera_pos() -> u32 {
    STATE.with(|cell| {
        let st = cell.borrow();
        let Some(g) = st.rt.game.as_ref() else { return 0 };
        let (cx, cy) = match g.world.view.cam_override {
            Some((x, y)) => (x, y),
            None => {
                let l = g.world.leader();
                (l.x - 160, l.y - 100)
            }
        };
        let cx = cx.clamp(0, 2048 - 320) as u32;
        let cy = cy.clamp(0, 2048 - 200) as u32;
        (cx << 16) | cy
    })
}

#[no_mangle]
pub extern "C" fn core_tick_count() -> u32 {
    STATE.with(|cell| {
        cell.borrow()
            .rt
            .game
            .as_ref()
            .map(|g| g.tick_count as u32)
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn core_is_blocked() -> u32 {
    STATE.with(|cell| {
        cell.borrow()
            .rt
            .game
            .as_ref()
            .map(|g| u32::from(g.is_blocked_on_host()))
            .unwrap_or(0)
    })
}

// ============================================================================
// Web Segmented `.repal` Replay Theater & Chapter Thumbnail C ABI Exports
// ============================================================================

#[no_mangle]
pub extern "C" fn core_set_replay_mode(on: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let active = on != 0;
        st.rt.ui.replay_mode = active;
        if !active {
            st.rt.ui.replay_chapters.clear();
            st.rt.ui.replay_chapter_cursor = 0;
        }
        if let Some(g) = st.rt.game.as_mut() {
            g.recorder.enabled = !active;
        }
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn core_replay_menu_open() -> u32 {
    STATE.with(|cell| {
        let st = cell.borrow();
        let open = matches!(
            st.rt.ui.view,
            UiView::RootMenu
                | UiView::ReplayChapters
                | UiView::ReplayFinished
                | UiView::SystemSubmenu
                | UiView::RemakeSubmenu
                | UiView::SaveLoadSlots(_)
        );
        u32::from(open)
    })
}

#[no_mangle]
pub extern "C" fn core_open_replay_finished() {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.rt.ui.replay_finished_cursor = 0;
        st.rt.ui.set_view(UiView::ReplayFinished);
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn core_split_segment_manual() {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(g) = st.rt.game.as_mut() {
            g.split_segment_on_manual_save();
        }
    });
}

#[no_mangle]
pub extern "C" fn core_open_replay_chapters(cur_idx: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.rt.ui.replay_chapter_cursor = cur_idx as usize;
        st.rt.ui.set_view(UiView::ReplayChapters);
        st.rt.compositor.mark_ui_dirty();
    });
}

#[no_mangle]
pub extern "C" fn core_set_replay_chapters(json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if json_len == 0 || json_len > st.scratch.len() {
            return 0;
        }
        let Ok(text) = std::str::from_utf8(&st.scratch[..json_len]) else {
            return 0;
        };
        let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(text) else {
            return 0;
        };
        st.rt.ui.replay_chapters = arr
            .iter()
            .enumerate()
            .map(|(i, v)| pal_ui::ReplayChapterSummary {
                index: v["index"].as_u64().unwrap_or(i as u64) as usize,
                start_tick: v["start_tick"].as_u64().unwrap_or(0),
                scene_id: v["scene_id"].as_u64().unwrap_or(0) as u16,
                label: v["label"].as_str().unwrap_or("Chapter").to_string(),
                thumb_rgba: None,
            })
            .collect();
        st.rt.ui.replay_chapter_cursor = 0;
        st.rt.compositor.mark_ui_dirty();
        1
    })
}

#[no_mangle]
pub extern "C" fn core_set_replay_chapter_thumb(
    idx: usize,
    w: u32,
    h: u32,
    rgba_len: usize,
) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if rgba_len == 0 || rgba_len > st.scratch.len() {
            return 0;
        }
        let bytes = st.scratch[..rgba_len].to_vec();
        if let Some(ch) = st.rt.ui.replay_chapters.get_mut(idx) {
            ch.thumb_rgba = Some((w, h, bytes));
            st.rt.compositor.mark_ui_dirty();
            1
        } else {
            0
        }
    })
}

/// Hard-load a segment `save.json` during replay without re-enabling the recorder (#243).
#[no_mangle]
pub extern "C" fn core_load_save_replay(json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if json_len == 0 || json_len > st.scratch.len() {
            return 0;
        }
        let Ok(text) = std::str::from_utf8(&st.scratch[..json_len]) else {
            return 0;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
            return 0;
        };
        let ok = pal_runtime::replay_player::load_replay_anchor(
            &mut st.rt,
            &pal_core::game::replay_rec::ReplayAnchor::SaveJson(val),
        );
        if ok {
            st.rt.compositor.mark_ui_dirty();
        }
        u32::from(ok)
    })
}

/// Dispatch a single `RePal_CoreReplay_v3` `ReplayEvent` JSON object during Web playback.
#[no_mangle]
pub extern "C" fn core_replay_event(json_len: usize) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if json_len == 0 || json_len > st.scratch.len() {
            return 0;
        }
        let Ok(text) = std::str::from_utf8(&st.scratch[..json_len]) else {
            return 0;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
            return 0;
        };
        let Some((_, ev)) = pal_core::game::replay_rec::ReplayEvent::from_json(&val) else {
            return 0;
        };
        pal_runtime::replay_player::dispatch_replay_event(&mut st.rt, &ev);
        st.rt.route_cmds();
        st.rt.project();
        1
    })
}

/// Sync replay `debug_mode` / `tainted_by_debug` flags from the Web replay host (#235).
#[no_mangle]
pub extern "C" fn core_set_replay_debug_flags(debug_mode: u32, tainted: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let dm = debug_mode != 0;
        let td = tainted != 0;
        if let Some(g) = st.rt.game.as_mut() {
            g.debug_mode = dm;
            g.god_mode = false;
            g.recorder.debug_mode = dm;
            g.recorder.tainted_by_debug = td;
        }
        st.rt.ui.debug_mode = dm || td;
        st.rt.compositor.mark_ui_dirty();
    });
}

/// Transition cleanly from Web replay playback into live Take Over play (#241):
/// clears active `debug_mode`/`god_mode`, re-enables recording anchored here, and preserves
/// `tainted_by_debug` + watermark when taking over from a debug-tainted replay.
#[no_mangle]
pub extern "C" fn core_take_over_replay(was_tainted: u32) -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let tainted = was_tainted != 0;
        st.rt.ui.replay_mode = false;
        st.rt.ui.replay_chapters.clear();
        st.rt.ui.replay_chapter_cursor = 0;
        st.rt.ui.debug_mode = tainted;
        st.rt.compositor.mark_ui_dirty();
        if let Some(g) = st.rt.game.as_mut() {
            g.take_over_from_replay(tainted);
            1
        } else {
            0
        }
    })
}

/// Set or clear the HD-only AI thought subtitle (`len == 0` clears) from `wasm_scratch_ptr`.
#[no_mangle]
pub extern "C" fn core_set_ai_thought(len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if len == 0 {
            st.rt.set_ai_thought(None);
            return;
        }
        if len > st.scratch.len() {
            return;
        }
        let text = std::str::from_utf8(&st.scratch[..len])
            .ok()
            .map(|s| pal_core::game::replay_rec::sanitize_ai_thought(s).0)
            .filter(|s| !s.is_empty());
        st.rt.set_ai_thought(text);
    });
}

/// Show (`show != 0`) or hide (`show == 0`) the HD AI thought subtitle overlay.
#[no_mangle]
pub extern "C" fn core_set_show_ai_thought(show: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.rt.ui.show_ai_thought = show != 0;
    });
}

/// Set whether the AI thought subtitle is dimmed (~45% opacity after 12 s idle).
#[no_mangle]
pub extern "C" fn core_set_ai_thought_dimmed(dimmed: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.rt.ui.ai_thought_dimmed = dimmed != 0;
    });
}

#[no_mangle]
pub extern "C" fn core_export_replay_v3() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let Some(g) = st.rt.game.as_ref() else {
            return 0;
        };
        let bytes = serde_json::to_vec(&g.export_replay_json()).unwrap_or_default();
        st.core_buf = bytes;
        st.core_buf.len()
    })
}

#[no_mangle]
pub extern "C" fn core_restart_recording() -> u32 {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(g) = st.rt.game.as_mut() {
            g.recorder.enabled = true;
            let _ = g.restart_recording_here();
            1
        } else {
            0
        }
    })
}

/// Reset streaming SHA-256 hasher for chunked browser `.rpk` verification (#237).
#[no_mangle]
pub extern "C" fn wasm_sha256_init() {
    STATE.with(|cell| {
        cell.borrow_mut().sha256 = pal_runtime::crypto::Sha256::new();
    });
}

/// Feed `len` bytes from `wasm_scratch_ptr(len)` into the streaming SHA-256 hasher (#237).
#[no_mangle]
pub extern "C" fn wasm_sha256_update(len: usize) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let n = len.min(st.scratch.len());
        if n > 0 {
            let mut hasher = std::mem::replace(&mut st.sha256, pal_runtime::crypto::Sha256::new());
            hasher.update(&st.scratch[..n]);
            st.sha256 = hasher;
        }
    });
}

/// Finalize streaming SHA-256, write 64-byte lowercase hex digest to `core_buf_ptr()`, return 64.
#[no_mangle]
pub extern "C" fn wasm_sha256_final() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let hasher = std::mem::replace(&mut st.sha256, pal_runtime::crypto::Sha256::new());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        st.core_buf = hex.into_bytes();
        st.core_buf.len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_ui_action_all_codes() {
        assert_eq!(encode_ui_action(&UiAction::None), (0, 0, 0));
        assert_eq!(encode_ui_action(&UiAction::ToggleLanguage), (20, 0, 0));
        assert_eq!(encode_ui_action(&UiAction::CopyReplayReport), (21, 0, 0));
        assert_eq!(
            encode_ui_action(&UiAction::BattleAttack { enemy_idx: 2 }),
            (15, 2, 0)
        );
        assert_eq!(
            encode_ui_action(&UiAction::BattleCastMagic {
                magic_id: 349,
                enemy_idx: 1
            }),
            (16, 349, 1)
        );
    }

    #[test]
    fn test_wasm_key_scratch_ptr_bounds() {
        let p0 = wasm_key_scratch_ptr(0);
        let p1 = wasm_key_scratch_ptr(128);
        assert_eq!(p0, p1);
        assert!(!p0.is_null());
    }
}
