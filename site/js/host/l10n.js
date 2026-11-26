/**
 * Tiny UI-string localiser for host-side (JS) toasts and labels.
 * Language follows the HUD (`hud.lang`), falling back to the persisted `pal_lang` choice so
 * boot-time messages (before the HUD exists) match the player's language too.
 */
export function uiLang() {
  const live = globalThis.__pal?.hud?.lang;
  if (live) return live;
  try {
    const saved = globalThis.localStorage?.getItem("pal_lang");
    if (saved === "hans" || saved === "hant" || saved === "en") return saved;
  } catch {}
  return "hans";
}

/** Pick the string for the current UI language (same argument order as Rust `tl`). */
export function L(hans, hant, en) {
  const lang = uiLang();
  return lang === "en" ? en : lang === "hant" ? hant : hans;
}
