//! Offline HD BGM renderer using RePal's native `pal_core::synth::DualSynth` (`hd_mode = true`).
//!
//! Renders `audio/midi/mus_XXX.mid` to 44.1 kHz 16-bit stereo PCM WAV with seamless reverb-tail
//! wrap-around so Assets Agent can encode pre-rendered `audio/bgm_hd/mus_XXX.mp3` tracks that sound
//! 100% identical to RePal's authentic HD MIDI voice while running at 0% synth CPU at runtime.
//!
//! Usage:
//!   cargo run --release -p pal-headless --bin render_bgm_hd -- --midi-dir site/assets/audio/midi --out-dir /tmp/bgm_wav

use pal_core::synth::DualSynth;
use std::fs;
use std::path::{Path, PathBuf};

fn write_wav_stereo_i16(path: &Path, sample_rate: u32, left: &[f32], right: &[f32]) -> std::io::Result<()> {
    let frames = left.len().min(right.len());
    let data_bytes = (frames * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for i in 0..frames {
        let l = (left[i].clamp(-1.0, 1.0) * 32767.0).round() as i16;
        let r = (right[i].clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    fs::write(path, out)
}

fn render_one(midi_bytes: &[u8], sample_rate: u32, hd_mode: bool) -> Option<(Vec<f32>, Vec<f32>)> {
    let mut synth = DualSynth::new(sample_rate);
    synth.hd_mode = hd_mode;
    synth.master_volume = 0.72;
    if !synth.load_midi(midi_bytes, false) {
        return None;
    }
    let dur_frames = ((synth.duration_sec() * sample_rate as f64).round() as usize).max(sample_rate as usize / 2);
    let tail_frames = ((2.5 * sample_rate as f64) as usize).min(dur_frames);
    let total_frames = dur_frames + tail_frames;

    let mut all_l = vec![0.0f32; total_frames];
    let mut all_r = vec![0.0f32; total_frames];
    let chunk = 4096;
    let mut pos = 0;
    while pos < total_frames {
        let end = (pos + chunk).min(total_frames);
        synth.render_stereo(&mut all_l[pos..end], &mut all_r[pos..end]);
        pos = end;
    }

    // Wrap the reverb decay tail back into t=0..tail for a seamless loop boundary.
    let mut left = all_l[..dur_frames].to_vec();
    let mut right = all_r[..dur_frames].to_vec();
    for i in 0..tail_frames {
        left[i] = (left[i] + all_l[dur_frames + i]).clamp(-1.0, 1.0);
        right[i] = (right[i] + all_r[dur_frames + i]).clamp(-1.0, 1.0);
    }
    // 3ms micro-taper at loop boundary to eliminate any residual click.
    let taper = ((sample_rate as f64 * 0.003) as usize).min(dur_frames / 8);
    for i in 0..taper {
        let g = i as f32 / taper.max(1) as f32;
        left[i] *= g;
        right[i] *= g;
        let ri = dur_frames - 1 - i;
        left[ri] *= g;
        right[ri] *= g;
    }
    Some((left, right))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut midi_dir = PathBuf::from("site/assets/audio/midi");
    let mut out_dir = PathBuf::from("/tmp/repal_bgm_hd_wav");
    let mut sample_rate = 44100u32;
    let mut hd_mode = true;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--midi-dir" if i + 1 < args.len() => {
                midi_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--out-dir" if i + 1 < args.len() => {
                out_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--sample-rate" if i + 1 < args.len() => {
                sample_rate = args[i + 1].parse().unwrap_or(44100);
                i += 2;
            }
            "--opl2" => {
                hd_mode = false;
                i += 1;
            }
            _ => i += 1,
        }
    }

    fs::create_dir_all(&out_dir).expect("create out_dir");
    let mut entries: Vec<PathBuf> = fs::read_dir(&midi_dir)
        .expect("read midi_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("mid"))
        .collect();
    entries.sort();

    let mut rendered = 0usize;
    for path in entries {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let Ok(bytes) = fs::read(&path) else { continue };
        let Some((l, r)) = render_one(&bytes, sample_rate, hd_mode) else {
            eprintln!("[skip] failed to parse {}", path.display());
            continue;
        };
        let out_path = out_dir.join(format!("{stem}.wav"));
        write_wav_stereo_i16(&out_path, sample_rate, &l, &r).expect("write wav");
        rendered += 1;
    }
    println!(
        "Rendered {rendered} tracks ({} Hz, hd_mode={hd_mode}) to {}",
        sample_rate,
        out_dir.display()
    );
}
