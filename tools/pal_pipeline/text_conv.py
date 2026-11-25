"""Big5 decoding, PAL dialogue control-tag parsing, and Xianjian-aware Traditional->Simplified conversion."""

from __future__ import annotations
import re
import sys
from pathlib import Path

# Ensure workspace .pylibs is importable in sandbox
_PYLIBS = Path(__file__).resolve().parents[2] / ".pylibs"
if _PYLIBS.exists() and str(_PYLIBS) not in sys.path:
    sys.path.insert(0, str(_PYLIBS))

from opencc import OpenCC  # type: ignore

_CC = OpenCC("t2s")

# Pre-conversion & post-conversion overrides for authentic Legend of Sword and Fairy terminology
_PRE_PROTECT = {
    "巫后": "__PAL_WUHOU__",
    "皇后": "__PAL_HUANGHOU__",
    "太后": "__PAL_TAIHOU__",
}

_POST_FIXES = {
    "__PAL_WUHOU__": "巫后",
    "__PAL_HUANGHOU__": "皇后",
    "__PAL_TAIHOU__": "太后",
    "干坤一掷": "乾坤一掷",
    "干坤": "乾坤",
    "婶娘": "婶娘",
}

_CTRL_RE = re.compile(r"(~[0-9]*|'[0-9]*|\"[0-9]*|\$[0-9]*|-[0-9]*|\([0-9]+\))")


def decode_big5(raw: bytes) -> str:
    """Decode DOS Big5 bytes cleanly, stripping trailing NUL/spaces."""
    # Replace DOS PAL custom glyph/box bytes gracefully if any fall outside standard Big5
    text = raw.decode("big5-hkscs", errors="replace").replace("\ufffd", "·")
    return text.rstrip("\x00 ")


def to_simplified(hant: str) -> str:
    """Convert Traditional Chinese to Simplified Chinese with PAL terminology protection."""
    if not hant:
        return ""
    s = hant
    for k, tok in _PRE_PROTECT.items():
        s = s.replace(k, tok)
    s = _CC.convert(s)
    for tok, v in _POST_FIXES.items():
        s = s.replace(tok, v)
    return s


def clean_dialogue_text(raw_msg: str) -> tuple[str, dict]:
    """Extract PAL inline formatting codes and return clean display text + metadata."""
    meta: dict = {}
    s = raw_msg
    # Leading/inline color tags in DOS PAL:
    # `'` = yellow/highlight, `"` = red/important, `-` = cyan/system
    if "'" in s:
        meta["highlight"] = True
    if '"' in s:
        meta["accent"] = "red"
    # Extract emotion icon `(n)` if present at start/end
    m_icon = re.search(r"\?*\(([0-9]+)\)", s)
    if m_icon:
        meta["icon"] = int(m_icon.group(1))
        s = re.sub(r"\?*\([0-9]+\)", "", s)
    # Strip speed/delay control codes `$nn` and `~nn`
    s = re.sub(r"[\$~][0-9]*", "", s)
    # Remove standalone quote color toggles `'` `"` `-` used as PAL inline color switches,
    # converting paired `'word'` into Chinese corner brackets 「word」 for clean modern typography!
    s = re.sub(r"'([^']+)'", r"「\1」", s)
    s = re.sub(r'"([^"]+)"', r"『\1』", s)
    s = s.replace("'", "").replace('"', "")
    if s.startswith("-"):
        meta["system_tip"] = True
        s = s.lstrip("-")
    return s.strip(), meta
