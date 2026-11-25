/**
 * Non-intrusive YAML Story Mod Loader (`./assets/mods/manifest.yaml`).
 * Allows players/creators to override any `s_XXXX` script block, add `#` comments,
 * or tweak dialogues by editing plain `.yaml` files — zero compilation required!
 */

import { parseYaml } from "../vendor/yaml.js";
import { resolveAssetBaseUrl, withAssetVersion } from "../host/asset-provider.js";

export class ModLoader {
  constructor() {
    this.assetBaseUrl = resolveAssetBaseUrl();
    this.manifest = [];
    this.loadedMods = new Map(); // modId -> { meta, parsed }
    this.activeOverrides = new Map(); // scriptLabel -> steps[]
  }

  async init() {
    try {
      const resp = await fetch(withAssetVersion(`${this.assetBaseUrl}/mods/manifest.yaml`));
      if (!resp.ok) return;
      const text = await resp.text();
      const doc = parseYaml(text);
      this.manifest = Array.isArray(doc?.mods) ? doc.mods : [];
      for (const m of this.manifest) {
        if (!m?.enabled) continue;
        await this.preloadMod(m);
      }
      this.rebuildOverrides();
    } catch (err) {
      console.warn("[ModLoader] Optional mod manifest skipped:", err);
    }
  }

  async preloadMod(modMeta) {
    if (!modMeta?.file) return;
    try {
      const resp = await fetch(withAssetVersion(`${this.assetBaseUrl}/${modMeta.file}`));
      if (!resp.ok) return;
      const parsed = parseYaml(await resp.text());
      this.loadedMods.set(modMeta.id, { meta: modMeta, parsed });
    } catch (err) {
      console.warn("[ModLoader] Failed loading mod file:", modMeta.file, err);
    }
  }

  rebuildOverrides() {
    this.activeOverrides.clear();
    for (const m of this.manifest) {
      if (!m.enabled) continue;
      const bundle = this.loadedMods.get(m.id);
      const ov = bundle?.parsed?.scripts_override;
      if (ov && typeof ov === "object") {
        for (const [label, steps] of Object.entries(ov)) {
          if (Array.isArray(steps)) {
            this.activeOverrides.set(label, steps);
          }
        }
      }
    }
  }

}
