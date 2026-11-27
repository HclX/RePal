#![cfg(feature = "disk")]
//! Cross-language Python `tools/build_asset_packs.py` <-> Rust `OverlayVfs` / `RpkReader`
//! integration tests (#237 T1–T3).

use pal_runtime::crypto::parse_key_hex;
use pal_runtime::disk::{
    boot_runtime_from_vfs, load_scene_assets_from_vfs, pump_render_assets_from_vfs,
};
use pal_runtime::pack_vfs::OverlayVfs;
use std::path::PathBuf;

fn run_full_boot_from_packs(packs_dir: PathBuf, master_key_hex: Option<&str>) {
    let key = master_key_hex.map(|h| parse_key_hex(h).expect("valid hex key"));
    let mut vfs =
        OverlayVfs::from_packs_dir(packs_dir, key, None, true).expect("mount packs_dir");
    assert!(vfs.is_pack_mounted("boot"));
    assert!(vfs.is_pack_mounted("shared"));
    assert!(!vfs.is_pack_mounted("ch01a-yuhang"));

    // Boot full PalRuntime (fonts + 7 YAML tables + i18n + sprite_visuals + 11 script chapters)
    // directly from `.rpk` with ZERO loose directory access!
    let mut rt = boot_runtime_from_vfs(&mut vfs, true).expect("boot PalRuntime from RPK1");
    let g_data = &rt.game.as_ref().unwrap().data;
    assert!(g_data.scenes.len() > 200, "got {} scenes", g_data.scenes.len());
    assert!(
        g_data.sprite_visuals.len() >= 600,
        "got {} sprite_visuals",
        g_data.sprite_visuals.len()
    );

    // Start New Game -> Scene 1 (Map 12) -> auto-mounts ch01a-yuhang & map-hd-012!
    rt.new_game(0x1995_0710);
    {
        let g = rt.game.as_mut().unwrap();
        let mid = g.world.scenes.get(&1).map(|s| s.map_id).unwrap_or(12);
        load_scene_assets_from_vfs(&mut vfs, g, mid);
    }
    assert!(vfs.is_pack_mounted("ch01a-yuhang"));
    assert!(vfs.is_pack_mounted("map-hd-012"));

    // Render frame & pump textures straight out of `.rpk` packs into AssetCache!
    rt.render_frame();
    let loaded = pump_render_assets_from_vfs(&mut vfs, &mut rt.compositor.cache);
    assert!(loaded > 0, "expected render assets loaded from RPK1 packs");
}

#[test]
fn rpk_python_built_plain_and_encrypted_rotated_packs_boot_runtime() {
    let plain_dir = PathBuf::from("/tmp/repal_packs_plain");
    if plain_dir.join("pack-manifest.json").is_file() {
        run_full_boot_from_packs(plain_dir, None);
    }
    let enc_dir = PathBuf::from("/tmp/repal_packs_enc");
    if enc_dir.join("pack-manifest.json").is_file() {
        // Key was rotated from 0123... to fedcba... in T2 verification!
        let old_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let new_key = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        assert!(
            OverlayVfs::from_packs_dir(
                enc_dir.clone(),
                Some(parse_key_hex(old_key).unwrap()),
                None,
                true,
            )
            .is_err(),
            "revoked old master key must fail to mount rotated packs"
        );
        run_full_boot_from_packs(enc_dir, Some(new_key));
    }
}
