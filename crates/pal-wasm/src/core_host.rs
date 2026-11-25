//! JSON serialization of platform-facing `HostCmd`s for the browser JS host.
//!
//! All core↔UI↔render orchestration lives in the shared `pal-runtime` crate.

use pal_core::game::HostCmd;
use serde_json::json;

pub(crate) fn host_cmd_to_json(cmd: HostCmd) -> Option<String> {
    let v = match cmd {
        HostCmd::PlayMusic { track, looped } => json!({"cmd": "music", "track": track, "loop": looped}),
        HostCmd::StopMusic => json!({"cmd": "music_stop"}),
        HostCmd::PlaySfx(id) => json!({"cmd": "sfx", "id": id}),
        HostCmd::PlayRng { id, start, end, speed } => {
            json!({"cmd": "rng", "id": id, "start": start, "end": end, "speed": speed})
        }
        HostCmd::LoadScene { scene_id, map_id } => {
            json!({"cmd": "load_scene", "scene": scene_id, "map": map_id})
        }
        HostCmd::LoadChapter(ch) => json!({"cmd": "load_chapter", "ch": ch}),
        HostCmd::ReturnToTitle => json!({"cmd": "title"}),
        HostCmd::GameOver => json!({"cmd": "game_over"}),
        HostCmd::SceneChanged(s) => json!({"cmd": "scene_changed", "scene": s}),
        _ => return None,
    };
    Some(v.to_string())
}
