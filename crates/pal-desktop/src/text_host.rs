//! Thin `TextHost` implementation wiring `DesktopApp` (`repal` GUI) directly into the shared
//! `pal_headless::text_mode::TextEngine` and `ChannelServer`.

use crate::DesktopApp;
use pal_headless::render_settled_frame_vfs;
use pal_headless::text_mode::TextHost;
use pal_runtime::PalRuntime;
use pal_ui::UiAction;
use std::path::Path;

impl TextHost for DesktopApp {
    fn runtime(&self) -> &PalRuntime {
        self.rt
            .as_ref()
            .expect("TextHost called before PalRuntime finished booting")
    }

    fn runtime_mut(&mut self) -> &mut PalRuntime {
        self.rt
            .as_mut()
            .expect("TextHost called before PalRuntime finished booting")
    }

    fn assets_root(&self) -> &Path {
        &self.cfg.assets_root
    }

    fn out_dir(&self) -> &Path {
        &self.cfg.out_dir
    }

    fn service_host_cmds(&mut self) {
        DesktopApp::service_host_cmds(self);
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        DesktopApp::handle_ui_action(self, action);
    }

    fn render_settled_frame(&mut self) {
        if let Some(rt) = self.rt.as_mut() {
            render_settled_frame_vfs(rt, &mut self.vfs);
        }
    }

    fn on_toast(&mut self, msg: &str) {
        if let Some(rt) = self.rt.as_mut() {
            rt.show_toast(msg.to_string(), 85);
        }
    }

    fn on_transition_banner(&mut self, msg: &str, ms: u64) {
        self.show_transition_banner(msg.to_string(), ms);
    }

    fn on_click_ripple(&mut self, wx: i32, wy: i32) {
        if let Some(rt) = self.rt.as_mut() {
            rt.world.click_ripple = Some((wx, wy, 0.0));
        }
    }

    fn on_session_transition(&mut self, stop_music: bool, enable_recorder: bool) {
        self.core_replay = None;
        self.rng_play = None;
        if stop_music {
            if let Some(audio) = self.audio.as_mut() {
                audio.stop_music();
            }
        }
        if let Some(rt) = self.rt.as_mut() {
            rt.world.active_rng = None;
            rt.ui.replay_mode = false;
            rt.ui.replay_chapters.clear();
            if enable_recorder {
                if let Some(g) = rt.game.as_mut() {
                    g.recorder.enabled = true;
                }
            }
        }
    }

    fn on_hd_changed(&mut self, hd: bool) {
        if let Some(audio) = self.audio.as_mut() {
            audio.set_hd_mode(hd);
        }
        if let Some(rt) = self.rt.as_mut() {
            let msg = if hd {
                crate::tl(rt.ui.lang, "画质切换：HD 高清", "畫質切換：HD 高清", "Graphics: HD")
            } else {
                crate::tl(rt.ui.lang, "画质切换：1995 复古", "畫質切換：1995 復古", "Graphics: 1995 Retro")
            };
            rt.show_toast(msg, 110);
        }
    }

    fn on_lang_changed(&mut self, lang: pal_core::Language) {
        if let Some(rt) = self.rt.as_mut() {
            self.saves.refresh_slots_vfs(&self.vfs, rt);
            let msg = match lang {
                pal_core::Language::ZhHant => "文字已切換：繁體中文",
                pal_core::Language::ZhHans => "文字已切换：简体中文",
                pal_core::Language::En => "Language: English",
            };
            rt.show_toast(msg, 110);
        }
    }

    fn skip_rng_if_active(&mut self) -> bool {
        if self.rng_play.is_some() {
            self.finish_rng();
            true
        } else {
            false
        }
    }

    fn rng_status(&self) -> Option<pal_headless::text_mode::RngStatus> {
        self.rng_play
            .as_ref()
            .map(|r| pal_headless::text_mode::RngStatus {
                id: r.id,
                frame: r.frame,
                end: r.end,
                skippable: true,
            })
    }

    fn export_bugreport(&mut self, desc: &str) -> Result<serde_json::Value, String> {
        self.render_settled_frame();
        let note = if desc.trim().is_empty() {
            None
        } else {
            Some(desc)
        };
        let path = self.export_bug_report_with_note(note);
        Ok(serde_json::json!({
            "repal": path.map(|p| p.display().to_string()),
            "submitting_gh": true,
        }))
    }
}
