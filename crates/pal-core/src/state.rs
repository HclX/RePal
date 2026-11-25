//! Render/UI-facing `GameState` (projected from `game::World` every tick) and the 1995 DOS
//! `.rpg` binary save parser. Game rules live in `game`, not here.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartyMember {
    pub role_id: u16,
    pub x: i32,
    pub y: i32,
    pub dir: u8,
    pub frame: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleState {
    pub role_id: u16,
    pub avatar_rgm: u16,
    pub battle_sprite_f: u16,
    pub walk_sprite_mgo: u16,
    pub level: u16,
    pub exp: u32,
    pub max_hp: u16,
    pub max_mp: u16,
    pub hp: u16,
    pub mp: u16,
    /// [hat, cloak, armor, weapon, boots, ornament]
    pub equipment: [u16; 6],
    /// [attack, magic_power, defense, dexterity, flee]
    pub stats: [u16; 5],
    pub coop_magic: u16,
    pub magics: Vec<u16>,
    pub poisons: Vec<u16>,
    pub statuses: BTreeMap<u16, u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneState {
    pub id: u16,
    pub map_id: u16,
    pub script_enter: u16,
    pub script_teleport: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventState {
    pub id: u16,
    pub vanish_time: i16,
    pub x: u16,
    pub y: u16,
    pub layer: i16,
    pub script_trigger: u16,
    pub script_auto: u16,
    pub state: i16,
    pub trigger_mode: u16,
    pub sprite_id: u16,
    pub sprite_frames: u16,
    pub dir: u16,
    pub current_frame: u16,
}

#[derive(Clone, Debug)]
pub struct GameState {
    pub scene_id: u16,
    pub palette_id: u16,
    pub night_palette: bool,
    pub music_track: u16,
    pub battle_music_track: u16,
    pub battlefield_id: u16,
    pub screen_wave: u16,
    pub cash: u32,
    pub party: Vec<PartyMember>,
    /// Historical party trail `(world_x, world_y, dir)` loaded from DOS `.rpg` `rgTrail[5]`.
    pub trail: Vec<(i32, i32, u8)>,
    pub party_layer: i32,
    pub roles: [RoleState; 6],
    pub inventory: BTreeMap<u16, u16>,
    pub scenes: Vec<SceneState>,
    pub events: Vec<EventState>,
    pub gourd_essences: Vec<u16>,
    pub follower_sprites: Vec<u16>,
    pub follower_positions: Vec<(i32, i32, u8)>,
    pub object_scripts: BTreeMap<u16, u16>,
    pub ending_triggered: bool,
    pub game_quit: bool,
    /// `wChaseRange` from DOS saves (monster alert range multiplier).
    pub chase_range: u16,
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

impl GameState {
    pub fn new() -> Self {
        let mut st = Self {
            scene_id: 1,
            palette_id: 0,
            night_palette: false,
            music_track: 31,
            battle_music_track: 37,
            battlefield_id: 0,
            screen_wave: 0,
            cash: 0,
            party: vec![PartyMember {
                role_id: 0,
                x: 41 * 32,
                y: 18 * 16,
                dir: 0,
                frame: 0,
            }],
            trail: Vec::new(),
            party_layer: 0,
            roles: core::array::from_fn(|i| RoleState {
                role_id: i as u16,
                avatar_rgm: i as u16,
                battle_sprite_f: i as u16,
                walk_sprite_mgo: match i {
                    0 => 2, // 李逍遙
                    1 => 3, // 趙靈兒
                    2 => 4, // 林月如
                    _ => 2,
                },
                max_hp: 100,
                max_mp: 50,
                hp: 100,
                mp: 50,
                level: 1,
                ..Default::default()
            }),
            inventory: BTreeMap::new(),
            scenes: (1..=294)
                .map(|id| SceneState {
                    id,
                    map_id: if id == 1 { 12 } else { 0 },
                    ..Default::default()
                })
                .collect(),
            events: Vec::new(),
            gourd_essences: Vec::new(),
            follower_sprites: Vec::new(),
            follower_positions: Vec::new(),
            object_scripts: BTreeMap::new(),
            ending_triggered: false,
            game_quit: false,
            chase_range: 0,
        };
        st.reset_new_game();
        st
    }

    pub fn reset_new_game(&mut self) {
        self.scene_id = 1;
        self.palette_id = 0;
        self.night_palette = false;
        self.music_track = 31;
        self.battle_music_track = 37;
        self.battlefield_id = 0;
        self.screen_wave = 0;
        self.cash = 0;
        self.party = vec![PartyMember {
            role_id: 0,
            x: 41 * 32,
            y: 18 * 16,
            dir: 0,
            frame: 0,
        }];
        self.trail.clear();
        for (i, r) in self.roles.iter_mut().enumerate() {
            *r = RoleState {
                role_id: i as u16,
                avatar_rgm: (i + 1) as u16,
                battle_sprite_f: i as u16,
                walk_sprite_mgo: match i {
                    0 => 2, // 李逍遙
                    1 => 3, // 趙靈兒
                    2 => 4, // 林月如
                    _ => 2,
                },
                max_hp: 150,
                max_mp: 100,
                hp: 150,
                mp: 100,
                level: 1,
                ..Default::default()
            };
        }
        self.inventory.clear();
        self.gourd_essences.clear();
        self.follower_sprites.clear();
        self.object_scripts.clear();
        self.ending_triggered = false;
        self.game_quit = false;
    }

    #[inline]
    fn u16_le(buf: &[u8], off: usize) -> u16 {
        u16::from_le_bytes([buf[off], buf[off + 1]])
    }

    #[inline]
    fn i16_le(buf: &[u8], off: usize) -> i16 {
        i16::from_le_bytes([buf[off], buf[off + 1]])
    }

    #[inline]
    fn u32_le(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    /// Parse and load an authentic 1995 DOS `.rpg` binary save (`>= 180,000` bytes).
    pub fn load_dos_rpg(&mut self, buf: &[u8]) -> Result<(), &'static str> {
        if buf.len() < 180_000 {
            return Err("Invalid DOS .rpg buffer length");
        }
        self.reset_new_game();

        let vp_x = Self::u16_le(buf, 2) as i32;
        let vp_y = Self::u16_le(buf, 4) as i32;
        let n_party = Self::u16_le(buf, 6).min(4) as usize;
        let scene_id = Self::u16_le(buf, 8);
        let pal_off = Self::u16_le(buf, 10);
        let party_dir = (Self::u16_le(buf, 12) & 3) as u8;
        let music_track = Self::u16_le(buf, 14);
        let battle_music = Self::u16_le(buf, 16);
        let battlefield_id = Self::u16_le(buf, 18);
        let screen_wave = Self::u16_le(buf, 20);
        let cash = Self::u32_le(buf, 40);

        self.scene_id = if (1..=294).contains(&scene_id) {
            scene_id
        } else {
            1
        };
        self.night_palette = pal_off != 0;
        self.music_track = music_track;
        self.battle_music_track = if battle_music > 0 { battle_music } else { 4 };
        self.battlefield_id = battlefield_id;
        self.screen_wave = if screen_wave < 200 { screen_wave } else { 0 };
        self.cash = cash;

        self.party.clear();
        for i in 0..=n_party {
            let base = 44 + i * 10;
            let role_id = Self::u16_le(buf, base).min(5);
            let rel_x = Self::i16_le(buf, base + 2) as i32;
            let rel_y = Self::i16_le(buf, base + 4) as i32;
            let raw_frame = Self::u16_le(buf, base + 6);
            let dir = if i == 0 {
                party_dir
            } else {
                ((raw_frame / 3) & 3) as u8
            };
            let frame = (raw_frame % 3) as u8;
            self.party.push(PartyMember {
                role_id,
                x: vp_x + rel_x,
                y: vp_y + rel_y,
                dir,
                frame,
            });
        }

        // Authentic 1995 DOS `SAVEDGAME.rgTrail[5]` at offset 94 (`5 * 6` bytes: x, y, wDirection).
        self.trail.clear();
        for i in 0..5 {
            let base = 94 + i * 6;
            let tx = Self::u16_le(buf, base) as i32;
            let ty = Self::u16_le(buf, base + 2) as i32;
            let td = (Self::u16_le(buf, base + 4) & 3) as u8;
            if tx != 0 || ty != 0 {
                self.trail.push((tx, ty, td));
            }
        }

        // PlayerRoles column-major table (`75 rows * 6 u16` at offset 508)
        let r_mat = |row: usize, col: usize| -> u16 { Self::u16_le(buf, 508 + (row * 6 + col) * 2) };
        for col in 0..6 {
            let r = &mut self.roles[col];
            r.avatar_rgm = r_mat(0, col);
            r.battle_sprite_f = r_mat(1, col);
            r.walk_sprite_mgo = r_mat(2, col);
            r.level = r_mat(6, col);
            r.max_hp = r_mat(7, col).max(1);
            r.max_mp = r_mat(8, col);
            r.hp = r_mat(9, col);
            r.mp = r_mat(10, col);
            for slot in 0..6 {
                r.equipment[slot] = r_mat(11 + slot, col);
            }
            for st in 0..5 {
                r.stats[st] = r_mat(17 + st, col);
            }
            r.magics.clear();
            for sr in 32..64 {
                let mid = r_mat(sr, col);
                if mid > 0 {
                    r.magics.push(mid);
                }
            }
            r.coop_magic = r_mat(65, col);
            // ALLEXPERIENCE.rgPrimaryExp[col] = { u16 wExp, u16 reserved, u16 wLevel, u16 wCount } at 124.
            r.exp = Self::u16_le(buf, 124 + col * 8) as u32;
        }

        // rgPoisonStatus[16][5] = { u16 wPoisonID, u16 wPoisonScript } at 1408, indexed by party slot.
        for slot in 0..16 {
            for p in 0..5 {
                let pid = Self::u16_le(buf, 1408 + (slot * 5 + p) * 4);
                if pid == 0 {
                    continue;
                }
                if let Some(role) = self.party.get(p).map(|m| m.role_id as usize) {
                    let r = &mut self.roles[role];
                    if !r.poisons.contains(&pid) {
                        r.poisons.push(pid);
                    }
                }
            }
        }
        self.chase_range = Self::u16_le(buf, 28);

        // Inventory[256] (`256 * 6` bytes at offset 1728)
        self.inventory.clear();
        for i in 0..256 {
            let off = 1728 + i * 6;
            let item_id = Self::u16_le(buf, off);
            let amt = Self::u16_le(buf, off + 2);
            if item_id > 0 && amt > 0 {
                let entry = self.inventory.entry(item_id).or_insert(0);
                *entry = entry.saturating_add(amt);
            }
        }

        // Scene[294] (`8` bytes each at offset 3264)
        for s0 in 0..294 {
            let off = 3264 + s0 * 8;
            self.scenes[s0] = SceneState {
                id: (s0 + 1) as u16,
                map_id: Self::u16_le(buf, off),
                script_enter: Self::u16_le(buf, off + 2),
                script_teleport: Self::u16_le(buf, off + 4),
            };
        }

        // rgObject[600] (`12` bytes = 6 u16 each at offset 5664..12864, #264).
        // `rgwData[2]` holds `wScriptOnTurnStart` / `wScriptOnUse` (modified by `object_set_script`).
        self.object_scripts.clear();
        for obj_id in 0..600u16 {
            let off = 5664 + (obj_id as usize) * 12;
            if off + 12 > buf.len() {
                break;
            }
            let w2 = Self::u16_le(buf, off + 4);
            if w2 > 0 {
                self.object_scripts.insert(obj_id, w2);
            }
        }

        // EventObject[5332] (`32` bytes each at offset 12864)
        let max_ev = 5332.min((buf.len() - 12864) / 32);
        self.events.clear();
        self.events.reserve(max_ev);
        for idx0 in 0..max_ev {
            let off = 12864 + idx0 * 32;
            self.events.push(EventState {
                id: (idx0 + 1) as u16,
                vanish_time: Self::i16_le(buf, off),
                x: Self::u16_le(buf, off + 2),
                y: Self::u16_le(buf, off + 4),
                layer: Self::i16_le(buf, off + 6),
                script_trigger: Self::u16_le(buf, off + 8),
                script_auto: Self::u16_le(buf, off + 10),
                state: Self::i16_le(buf, off + 12),
                trigger_mode: Self::u16_le(buf, off + 14),
                sprite_id: Self::u16_le(buf, off + 16),
                sprite_frames: Self::u16_le(buf, off + 18),
                dir: Self::u16_le(buf, off + 20),
                current_frame: Self::u16_le(buf, off + 22),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_dos_rpg_binary_layout() {
        let mut buf = vec![0u8; 183_488];
        // vpX=1280, vpY=256, nParty=0 (1 member), sceneId=3, cash=777
        buf[2..4].copy_from_slice(&1280u16.to_le_bytes());
        buf[4..6].copy_from_slice(&256u16.to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes());
        buf[8..10].copy_from_slice(&3u16.to_le_bytes());
        buf[40..44].copy_from_slice(&777u32.to_le_bytes());
        // Party[0]: role=0, relX=32, relY=16
        buf[46..48].copy_from_slice(&32i16.to_le_bytes());
        buf[48..50].copy_from_slice(&16i16.to_le_bytes());
        // Inventory[0]: item=272, amt=3
        buf[1728..1730].copy_from_slice(&272u16.to_le_bytes());
        buf[1730..1732].copy_from_slice(&3u16.to_le_bytes());

        let mut st = GameState::new();
        st.load_dos_rpg(&buf).unwrap();
        assert_eq!(st.scene_id, 3);
        assert_eq!(st.cash, 777);
        assert_eq!(st.party[0].x, 1312);
        assert_eq!(st.party[0].y, 272);
        assert_eq!(st.inventory.get(&272), Some(&3));
        assert_eq!(st.events.len(), 5332);
    }
}
