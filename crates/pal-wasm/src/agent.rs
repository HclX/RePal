//! Browser WebAssembly bridge for the unified `pal-headless` [`TextEngine`].
//!
//! Enables BOTH:
//! - In-page JS API (`window.repalAgent.send(cmd)`), for browser-capable AI agents;
//! - Outbound WebSocket bridge (`?agent_ws=ws://...` / `wss://...`), for CLI/remote AI agents.
//!
//! Runs the exact same Rust `TextEngine` and JSON-Lines protocol as `repal --text --json` and
//! `repal --listen`, stepped cleanly inside the browser's 50 Hz `requestAnimationFrame` loop.

use crate::STATE;
use pal_core::game::TickInput;
use pal_headless::text_mode::{
    ClientOrigin, ExecOutcome, MinimapMode, ObsMode, TextEngine, TextHost, TextReply,
};
use pal_runtime::PalRuntime;
use pal_ui::UiAction;
use std::collections::VecDeque;
use std::path::Path;

pub(crate) struct WebAgentState {
    pub engine: TextEngine,
    pub reply_buf: Vec<u8>,
    pub pending_input: Option<TickInput>,
}

impl WebAgentState {
    pub fn new() -> Self {
        Self {
            engine: TextEngine::with_obs(ObsMode::Fair, 0),
            reply_buf: Vec::new(),
            pending_input: None,
        }
    }
}

struct WasmTextHost<'a> {
    rt: &'a mut PalRuntime,
    pending_ui_actions: &'a mut VecDeque<UiAction>,
}

impl<'a> TextHost for WasmTextHost<'a> {
    fn runtime(&self) -> &PalRuntime {
        self.rt
    }

    fn runtime_mut(&mut self) -> &mut PalRuntime {
        self.rt
    }

    fn assets_root(&self) -> &Path {
        Path::new("assets")
    }

    fn out_dir(&self) -> &Path {
        Path::new(".")
    }

    fn service_host_cmds(&mut self) {
        // In browser WASM, JS (`core-host.js`) drains `out_cmds` asynchronously each frame.
        self.rt.route_cmds();
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        self.pending_ui_actions.push_back(action);
    }

    fn render_settled_frame(&mut self) {
        self.rt.project();
        self.rt.render_frame();
    }

    fn on_hd_changed(&mut self, hd: bool) {
        self.rt.set_hd_mode(hd);
    }

    fn on_lang_changed(&mut self, lang: pal_core::Language) {
        self.rt.set_lang(lang);
    }

    fn export_bugreport(&mut self, _desc: &str) -> Result<serde_json::Value, String> {
        Err("bugreport is not available in web wasm; use the F9 snapshot tool".to_string())
    }
}

fn encode_reply(buf: &mut Vec<u8>, reply: &TextReply) -> usize {
    buf.clear();
    if let Ok(s) = serde_json::to_string(&reply.response) {
        buf.extend_from_slice(s.as_bytes());
    }
    buf.len()
}

#[no_mangle]
pub extern "C" fn core_agent_buf_ptr() -> *const u8 {
    STATE.with(|cell| cell.borrow().agent.reply_buf.as_ptr())
}

#[no_mangle]
pub extern "C" fn core_agent_is_busy() -> u32 {
    STATE.with(|cell| u32::from(cell.borrow().agent.engine.has_active_task()))
}

#[no_mangle]
pub extern "C" fn core_agent_set_obs(fair: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let mode = if fair != 0 { ObsMode::Fair } else { ObsMode::Debug };
        st.agent.engine.obs_mode = mode;
        st.agent.engine.fog = mode.is_fair();
    });
}

#[no_mangle]
pub extern "C" fn core_agent_set_minimap(mode: u32) {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.agent.engine.minimap_mode = match mode {
            1 => MinimapMode::Always,
            2 => MinimapMode::Off,
            _ => MinimapMode::Auto,
        };
    });
}

#[no_mangle]
pub extern "C" fn core_agent_greeting() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let st = &mut *st;
        let host = WasmTextHost {
            rt: &mut st.rt,
            pending_ui_actions: &mut st.pending_ui_actions,
        };
        let reply = st.agent.engine.initial_look(&host);
        encode_reply(&mut st.agent.reply_buf, &reply)
    })
}

/// Submit a UTF-8 command from `wasm_scratch_ptr(cmd_len)`.
/// Returns `> 0` (byte length in `core_agent_buf_ptr`) if completed synchronously,
/// or `0` if an async multi-tick task is now active (poll via `core_agent_step` each tick).
#[no_mangle]
pub extern "C" fn core_agent_submit(cmd_len: usize) -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let raw = if cmd_len == 0 || cmd_len > st.scratch.len() {
            String::new()
        } else {
            String::from_utf8_lossy(&st.scratch[..cmd_len]).into_owned()
        };
        let st = &mut *st;
        let mut host = WasmTextHost {
            rt: &mut st.rt,
            pending_ui_actions: &mut st.pending_ui_actions,
        };
        match st
            .agent
            .engine
            .submit_from(&mut host, &raw, false, ClientOrigin::Local)
        {
            ExecOutcome::Done(r) | ExecOutcome::Quit(r) => {
                encode_reply(&mut st.agent.reply_buf, &r)
            }
            ExecOutcome::Pending => 0,
        }
    })
}

/// Advance an active multi-tick agent task by one 50 Hz tick.
/// Returns `> 0` (byte length in `core_agent_buf_ptr`) when the command finishes, or `0` while running.
/// Note: returns `0` without stepping while the core is blocked awaiting async JS scene/chapter loads.
#[no_mangle]
pub extern "C" fn core_agent_step() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.agent.engine.has_active_task() {
            return 0;
        }
        if st.rt.game.as_ref().is_some_and(|g| g.is_blocked_on_host()) {
            return 0;
        }
        let st = &mut *st;
        let mut host = WasmTextHost {
            rt: &mut st.rt,
            pending_ui_actions: &mut st.pending_ui_actions,
        };
        let (input, done) = st.agent.engine.step_tick(&mut host);
        if let Some(reply) = done {
            st.agent.pending_input = None;
            return encode_reply(&mut st.agent.reply_buf, &reply);
        }
        st.agent.pending_input = Some(input);
        0
    })
}

/// Consume the `TickInput` produced by `core_agent_step` (used by `core_tick` when an agent task is active).
#[no_mangle]
pub extern "C" fn core_agent_tick_with_pending() {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let input = st.agent.pending_input.take().unwrap_or_default();
        st.rt.tick(input);
    });
}

/// Cancel an in-flight agent task (e.g. when a human presses `Esc` to take over).
#[no_mangle]
pub extern "C" fn core_agent_cancel() -> usize {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.agent.engine.has_active_task() {
            return 0;
        }
        let st = &mut *st;
        let host = WasmTextHost {
            rt: &mut st.rt,
            pending_ui_actions: &mut st.pending_ui_actions,
        };
        let reply = st
            .agent
            .engine
            .cancelled_reply_with_reason(&host, "human", "interrupted by human");
        st.agent.engine.cancel_active_task();
        st.agent.pending_input = None;
        encode_reply(&mut st.agent.reply_buf, &reply)
    })
}
