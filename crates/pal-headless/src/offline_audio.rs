//! Deterministic offline stereo audio mixer for video capture & `.repal` video export (#11).
//!
//! Renders exact 20 ms chunks (`882` stereo frames at `44,100 Hz`, zero fractional drift against
//! the 50 Hz simulation tick) combining:
//! - HD BGM (`audio/bgm_hd/mus_XXX.{mp3,ogg}`) decoded via `ffmpeg` when `hd_mode == true`
//! - 1995 OPL3 / MIDI BGM (`audio/midi/mus_XXX.mid`) via `pal_core::synth::DualSynth`
//! - Local PCM WAV sound effects (`audio/sfx/sfx_XXX.wav`, up to 16 simultaneous voices)

use crate::AudioEvent;
use pal_core::synth::DualSynth;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

pub const OFFLINE_SAMPLE_RATE: u32 = 44_100;
/// Exact stereo frames per 20 ms simulation tick (`44100 / 50 == 882`).
pub const FRAMES_PER_TICK: usize = (OFFLINE_SAMPLE_RATE as usize) / 50;

struct SfxVoice {
    samples: Arc<Vec<f32>>,
    pos: f64,
    step: f64,
}

struct HdBgmStream {
    interleaved: Arc<Vec<f32>>, // stereo L,R pairs at OFFLINE_SAMPLE_RATE
    frame_pos: usize,
    looped: bool,
}

pub struct OfflineAudioMixer {
    synth: DualSynth,
    hd_mode: bool,
    current_track: i32,
    current_loop: bool,
    hd_bgm: Option<HdBgmStream>,
    hd_bgm_cache: HashMap<i32, Arc<Vec<f32>>>,
    hd_bgm_order: Vec<i32>,
    hd_bgm_missing: HashSet<i32>,
    midi_cache: HashMap<i32, Vec<u8>>,
    sfx_cache: HashMap<i32, (u32, Arc<Vec<f32>>)>,
    voices: Vec<SfxVoice>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl OfflineAudioMixer {
    pub fn new(hd_mode: bool) -> Self {
        let mut synth = DualSynth::new(OFFLINE_SAMPLE_RATE);
        synth.hd_mode = hd_mode;
        synth.master_volume = 0.72;
        Self {
            synth,
            hd_mode,
            current_track: 0,
            current_loop: true,
            hd_bgm: None,
            hd_bgm_cache: HashMap::new(),
            hd_bgm_order: Vec::new(),
            hd_bgm_missing: HashSet::new(),
            midi_cache: HashMap::new(),
            sfx_cache: HashMap::new(),
            voices: Vec::new(),
            scratch_l: vec![0.0; FRAMES_PER_TICK],
            scratch_r: vec![0.0; FRAMES_PER_TICK],
        }
    }

    pub fn apply_events<F>(&mut self, events: &[AudioEvent], hd_mode: bool, mut read_asset: F)
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if self.hd_mode != hd_mode {
            self.hd_mode = hd_mode;
            self.synth.hd_mode = hd_mode;
            if self.current_track > 0 {
                let track = self.current_track;
                let looped = self.current_loop;
                self.current_track = 0;
                self.start_music(track, looped, &mut read_asset);
            }
        }
        for ev in events {
            match *ev {
                AudioEvent::PlayMusic { track, looped } => {
                    self.start_music(track, looped, &mut read_asset);
                }
                AudioEvent::StopMusic => {
                    self.current_track = 0;
                    self.hd_bgm = None;
                    self.synth.stop();
                }
                AudioEvent::PlaySfx(id) => {
                    self.trigger_sfx(id, &mut read_asset);
                }
            }
        }
    }

    fn start_music<F>(&mut self, track_id: i32, looped: bool, read_asset: &mut F)
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if track_id <= 0 {
            self.current_track = 0;
            self.hd_bgm = None;
            self.synth.stop();
            return;
        }
        if self.current_track == track_id && self.current_loop == looped {
            return;
        }
        self.current_track = track_id;
        self.current_loop = looped;

        let midi_bytes = if let Some(b) = self.midi_cache.get(&track_id) {
            Some(b.clone())
        } else {
            let key = format!("audio/midi/mus_{:03}.mid", track_id);
            if let Some(b) = read_asset(&key) {
                self.midi_cache.insert(track_id, b.clone());
                Some(b)
            } else {
                None
            }
        };

        let hd_pcm = if self.hd_mode {
            self.load_hd_bgm(track_id, read_asset)
        } else {
            None
        };

        if let Some(interleaved) = hd_pcm {
            self.hd_bgm = Some(HdBgmStream {
                interleaved,
                frame_pos: 0,
                looped,
            });
            self.synth.stop();
        } else {
            self.hd_bgm = None;
            if let Some(bytes) = midi_bytes {
                self.synth.load_midi(&bytes, looped);
            } else {
                self.synth.stop();
            }
        }
    }

    fn load_hd_bgm<F>(&mut self, track_id: i32, read_asset: &mut F) -> Option<Arc<Vec<f32>>>
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if self.hd_bgm_missing.contains(&track_id) {
            return None;
        }
        if let Some(cached) = self.hd_bgm_cache.get(&track_id) {
            return Some(cached.clone());
        }
        for ext in ["mp3", "ogg"] {
            let key = format!("audio/bgm_hd/mus_{:03}.{ext}", track_id);
            let Some(bytes) = read_asset(&key) else {
                continue;
            };
            if let Some(pcm) = decode_compressed_via_ffmpeg(&bytes, OFFLINE_SAMPLE_RATE) {
                let arc = Arc::new(pcm);
                if self.hd_bgm_cache.len() >= 4 {
                    if let Some(old) = self.hd_bgm_order.first().copied() {
                        self.hd_bgm_order.remove(0);
                        self.hd_bgm_cache.remove(&old);
                    }
                }
                self.hd_bgm_order.push(track_id);
                self.hd_bgm_cache.insert(track_id, arc.clone());
                return Some(arc);
            }
        }
        self.hd_bgm_missing.insert(track_id);
        None
    }

    fn trigger_sfx<F>(&mut self, sfx_id: i32, read_asset: &mut F)
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if sfx_id < 0 {
            return;
        }
        let cached = if let Some((sr, samples)) = self.sfx_cache.get(&sfx_id) {
            Some((*sr, samples.clone()))
        } else {
            let key = format!("audio/sfx/sfx_{:03}.wav", sfx_id);
            let bytes = read_asset(&key);
            if let Some((sr, pcm)) = bytes.as_deref().and_then(parse_wav_mono) {
                let arc = Arc::new(pcm);
                self.sfx_cache.insert(sfx_id, (sr, arc.clone()));
                Some((sr, arc))
            } else {
                None
            }
        };
        if let Some((src_rate, samples)) = cached {
            let step = (src_rate as f64 / OFFLINE_SAMPLE_RATE as f64).max(0.05);
            if self.voices.len() >= 16 {
                self.voices.remove(0);
            }
            self.voices.push(SfxVoice {
                samples,
                pos: 0.0,
                step,
            });
        }
    }

    /// Advance the mixer by exactly one 20 ms tick (`FRAMES_PER_TICK` stereo frames) and append
    /// interleaved `f32le` stereo PCM bytes into `out_bytes`.
    pub fn render_tick_f32le(&mut self, out_bytes: &mut Vec<u8>) {
        let frames = FRAMES_PER_TICK;
        let l = &mut self.scratch_l[..frames];
        let r = &mut self.scratch_r[..frames];
        l.fill(0.0);
        r.fill(0.0);

        if self.hd_mode {
            if let Some(bgm) = self.hd_bgm.as_mut() {
                let total_frames = bgm.interleaved.len() / 2;
                if total_frames > 0 {
                    let vol = 0.85f32;
                    for i in 0..frames {
                        if bgm.frame_pos >= total_frames {
                            if bgm.looped {
                                bgm.frame_pos %= total_frames;
                            } else {
                                break;
                            }
                        }
                        let base = bgm.frame_pos * 2;
                        l[i] = bgm.interleaved[base] * vol;
                        r[i] = bgm.interleaved[base + 1] * vol;
                        bgm.frame_pos += 1;
                    }
                }
            } else {
                self.synth.render_stereo(l, r);
            }
        } else {
            self.synth.render_stereo(l, r);
        }

        if !self.voices.is_empty() {
            for i in 0..frames {
                let mut sfx_sum = 0.0f32;
                for v in &mut self.voices {
                    let idx = v.pos as usize;
                    if idx < v.samples.len() {
                        sfx_sum += v.samples[idx];
                        v.pos += v.step;
                    }
                }
                let s = (sfx_sum * 0.85).clamp(-1.0, 1.0);
                l[i] = (l[i] + s).clamp(-1.0, 1.0);
                r[i] = (r[i] + s).clamp(-1.0, 1.0);
            }
            self.voices.retain(|v| (v.pos as usize) < v.samples.len());
        }

        out_bytes.reserve(frames * 8);
        for i in 0..frames {
            out_bytes.extend_from_slice(&l[i].to_le_bytes());
            out_bytes.extend_from_slice(&r[i].to_le_bytes());
        }
    }
}

/// Decode compressed audio (MP3/OGG) into interleaved stereo `f32` PCM at `sample_rate` via `ffmpeg`.
fn decode_compressed_via_ffmpeg(bytes: &[u8], sample_rate: u32) -> Option<Vec<f32>> {
    let sr = sample_rate.to_string();
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", "pipe:0", "-f", "f32le", "-ac", "2", "-ar", &sr, "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Write stdin on a helper thread so large MP3s never deadlock against stdout pipe buffer.
    let mut stdin = child.stdin.take()?;
    let owned = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });
    let output = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !output.status.success() || output.stdout.len() < 8 {
        return None;
    }
    let samples: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if samples.len() >= 2 {
        Some(samples)
    } else {
        None
    }
}

/// Minimal zero-dependency PCM WAV reader (8-bit unsigned or 16-bit signed, mono/stereo -> mono f32).
pub(crate) fn parse_wav_mono(bytes: &[u8]) -> Option<(u32, Vec<f32>)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut channels = 1u16;
    let mut sample_rate = 11_025u32;
    let mut bits_per_sample = 8u16;
    let mut audio_fmt = 1u16;
    let mut data_slice: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_len =
            u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
                as usize;
        pos += 8;
        let end = pos.saturating_add(chunk_len).min(bytes.len());
        if chunk_id == b"fmt " && end - pos >= 16 {
            audio_fmt = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
            channels = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]).max(1);
            sample_rate = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ])
            .max(1000);
            bits_per_sample = u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]);
        } else if chunk_id == b"data" {
            data_slice = Some(&bytes[pos..end]);
            break;
        }
        pos = end + (chunk_len & 1);
    }

    if audio_fmt != 1 {
        return None;
    }
    let data = data_slice?;
    let ch = channels as usize;
    let mut out = Vec::new();
    match bits_per_sample {
        8 => {
            for frame in data.chunks_exact(ch) {
                let mut sum = 0.0f32;
                for &b in frame {
                    sum += (b as f32 - 128.0) / 128.0;
                }
                out.push(sum / ch as f32);
            }
        }
        16 => {
            for frame in data.chunks_exact(ch * 2) {
                let mut sum = 0.0f32;
                for c in 0..ch {
                    let s = i16::from_le_bytes([frame[c * 2], frame[c * 2 + 1]]);
                    sum += s as f32 / 32768.0;
                }
                out.push(sum / ch as f32);
            }
        }
        _ => return None,
    }
    if out.is_empty() {
        None
    } else {
        Some((sample_rate, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_audio_mixer_renders_exact_tick_pcm_and_mixes_sfx() {
        // Construct a tiny 8-bit mono WAV containing a non-zero square wave.
        let payload = vec![200u8; 64];
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + payload.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&22050u32.to_le_bytes());
        wav.extend_from_slice(&22050u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        wav.extend_from_slice(&payload);

        let mut mixer = OfflineAudioMixer::new(true);
        mixer.apply_events(&[AudioEvent::PlaySfx(1)], true, |k| {
            if k == "audio/sfx/sfx_001.wav" {
                Some(wav.clone())
            } else {
                None
            }
        });
        let mut pcm = Vec::new();
        mixer.render_tick_f32le(&mut pcm);
        assert_eq!(pcm.len(), FRAMES_PER_TICK * 8);
        let peak = pcm
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 0.3, "expected audible SFX peak, got {peak}");
    }
}
