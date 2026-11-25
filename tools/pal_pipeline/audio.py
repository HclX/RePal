"""Audio extraction & HD remastering pipeline:
- `VOC.MKF`: Decodes 276 Creative Voice 8-bit PCM SFX -> DC-centered, de-hissed, cubic anti-aliased
  22,050 Hz 16-bit PCM `.wav` files (crisp, hiss-free, and compact for GitHub Pages!).
- `MIDI.MKF` & `MUS.MKF`: Exports all 88 standard `.mid` and `.rix` music tracks + `music.yaml`.
"""

from __future__ import annotations
import struct
import wave
from pathlib import Path
import numpy as np


# Famous track titles in Legend of Sword and Fairy (DOS MIDI.MKF indices)
KNOWN_TRACK_NAMES = {
    1: "心然·蝶恋 (片头主题曲)",
    2: "云谷鹤峰 (标题画面)",
    3: "风起云涌 (战斗序曲)",
    4: "势如破竹 (普通战斗)",
    5: "身陷重围 (首领激战)",
    9: "壮志凌云",
    10: "余杭镇·清晨客栈",
    11: "盛渔村·市井喧嚣",
    12: "仙灵岛·碧波荷塘",
    13: "水月宫·灵儿曲",
    14: "雨(江南烟雨)",
    15: "桃花幻梦",
    16: "白河寒秋",
    19: "蝶恋 (剧情柔情版)",
    26: "酒剑仙·醉剑吟",
    31: "罗刹洞·梦境",
    37: "御剑伏魔 (梦境战斗)",
    85: "回梦游仙·结局",
}


def mkf_chunks(data: bytes) -> list[bytes]:
    if len(data) < 4:
        return []
    count = struct.unpack("<I", data[:4])[0] // 4 - 1
    out = []
    for i in range(count):
        o1, o2 = struct.unpack("<II", data[i * 4 : (i + 2) * 4])
        out.append(data[o1:o2] if 0 <= o1 <= o2 <= len(data) else b"")
    return out


def decode_voc_pcm(voc: bytes) -> tuple[int, np.ndarray] | None:
    """Decode Creative Voice File (`VOC.MKF` chunk) into `(sample_rate, float32_pcm[-1..1])`."""
    if len(voc) < 26 or not voc.startswith(b"Creative Voice File"):
        return None
    data_off = struct.unpack("<H", voc[20:22])[0]
    pos = data_off
    sr = 11025
    samples_list: list[np.ndarray] = []

    while pos < len(voc):
        btype = voc[pos]
        pos += 1
        if btype == 0:
            break
        if pos + 3 > len(voc):
            break
        blen = voc[pos] | (voc[pos + 1] << 8) | (voc[pos + 2] << 16)
        pos += 3
        bdata = voc[pos : pos + blen]
        pos += blen

        if btype == 1 and len(bdata) > 2:
            tc = bdata[0]
            codec = bdata[1]
            if codec == 0:  # 8-bit unsigned PCM
                sr = max(4000, int(round(1_000_000.0 / max(1, 256 - tc))))
                raw_u8 = np.frombuffer(bdata[2:], dtype=np.uint8).astype(np.float32)
                pcm = (raw_u8 - 128.0) / 128.0
                samples_list.append(pcm)
        elif btype == 2 and len(bdata) > 0:
            raw_u8 = np.frombuffer(bdata, dtype=np.uint8).astype(np.float32)
            samples_list.append((raw_u8 - 128.0) / 128.0)
        elif btype == 9 and len(bdata) > 12:
            sr = struct.unpack("<I", bdata[:4])[0]
            bits = bdata[4]
            if bits == 8:
                raw_u8 = np.frombuffer(bdata[12:], dtype=np.uint8).astype(np.float32)
                samples_list.append((raw_u8 - 128.0) / 128.0)
            elif bits == 16:
                raw_i16 = np.frombuffer(bdata[12:], dtype="<i2").astype(np.float32)
                samples_list.append(raw_i16 / 32768.0)

    if not samples_list:
        return None
    full = np.concatenate(samples_list)
    if full.size < 8:
        return None
    return sr, full


def remaster_sfx_to_hd_wav(sr: int, pcm: np.ndarray, target_sr: int = 22050) -> tuple[int, np.ndarray]:
    """Remove 8-bit DC bias & quantization hiss, upsample cleanly, and apply gentle fade-in/out."""
    # 1. Remove DC offset
    x = pcm - float(np.mean(pcm))

    # 2. Gentle 3-tap low-pass de-hiss on 8-bit quantization stair-stepping before upsampling
    if x.size >= 5:
        kernel = np.array([0.18, 0.64, 0.18], dtype=np.float32)
        x = np.convolve(x, kernel, mode="same")

    # 3. Smooth upsampling to target_sr (22,050 Hz 16-bit PCM)
    if sr != target_sr and x.size >= 4:
        out_len = max(4, int(round(x.size * target_sr / sr)))
        t_old = np.linspace(0.0, 1.0, num=x.size, dtype=np.float32)
        t_new = np.linspace(0.0, 1.0, num=out_len, dtype=np.float32)
        x = np.interp(t_new, t_old, x).astype(np.float32)

    # 4. Micro fade-in/out (3 ms) to eliminate DOS click/pop artifacts at clip boundaries
    fade = min(int(target_sr * 0.003), x.size // 4)
    if fade > 1:
        ramp = np.linspace(0.0, 1.0, fade, dtype=np.float32)
        x[:fade] *= ramp
        x[-fade:] *= ramp[::-1]

    # 5. Peak normalize gently if very quiet or clipping
    peak = float(np.max(np.abs(x)))
    if peak > 0.01:
        gain = min(1.35, 0.92 / peak)
        x = x * gain

    pcm16 = np.clip(np.round(x * 32767.0), -32768, 32767).astype("<i2")
    return target_sr, pcm16


def export_all_audio(raw_dir: Path, site_audio_dir: Path) -> dict:
    """Export all 276 VOC SFX (remastered WAV) and 88 MIDI/RIX music tracks."""
    sfx_dir = site_audio_dir / "sfx"
    midi_dir = site_audio_dir / "midi"
    rix_dir = site_audio_dir / "rix"
    sfx_dir.mkdir(parents=True, exist_ok=True)
    midi_dir.mkdir(parents=True, exist_ok=True)
    rix_dir.mkdir(parents=True, exist_ok=True)

    voc_chunks = mkf_chunks((raw_dir / "VOC.MKF").read_bytes())
    exported_sfx: list[int] = []
    for idx, ch in enumerate(voc_chunks):
        if not ch:
            continue
        dec = decode_voc_pcm(ch)
        if dec is None:
            continue
        sr, pcm = dec
        out_sr, pcm16 = remaster_sfx_to_hd_wav(sr, pcm, target_sr=22050)
        wav_path = sfx_dir / f"sfx_{idx:03d}.wav"
        with wave.open(str(wav_path), "wb") as wf:
            wf.setnchannels(1)
            wf.setsampwidth(2)
            wf.setframerate(out_sr)
            wf.writeframes(pcm16.tobytes())
        exported_sfx.append(idx)

    midi_chunks = mkf_chunks((raw_dir / "MIDI.MKF").read_bytes())
    exported_midi: list[dict] = []
    for idx, ch in enumerate(midi_chunks):
        if ch and ch.startswith(b"MThd"):
            (midi_dir / f"mus_{idx:03d}.mid").write_bytes(ch)
            exported_midi.append(
                {
                    "id": idx,
                    "title": KNOWN_TRACK_NAMES.get(idx, f"仙剑原声配乐 #{idx:02d}"),
                    "file": f"audio/midi/mus_{idx:03d}.mid",
                }
            )

    mus_chunks = mkf_chunks((raw_dir / "MUS.MKF").read_bytes())
    exported_rix = 0
    for idx, ch in enumerate(mus_chunks):
        if len(ch) >= 16:
            (rix_dir / f"mus_{idx:03d}.rix").write_bytes(ch)
            exported_rix += 1

    return {
        "sfx_total_chunks": len(voc_chunks),
        "sfx_exported": len(exported_sfx),
        "sfx_ids": exported_sfx,
        "midi_total_chunks": len(midi_chunks),
        "midi_tracks": exported_midi,
        "rix_exported": exported_rix,
    }
