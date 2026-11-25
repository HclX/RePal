//! Native desktop stereo audio engine: mixes `pal_core::synth::DualSynth` BGM with
//! decoded local PCM WAV sound effects (`assets/audio/sfx/sfx_XXX.wav`).
//!
//! Output playback uses `cpal` on macOS/Windows by default and on Linux with `--features audio`.

use pal_core::synth::DualSynth;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
struct SfxVoice {
    samples: Arc<Vec<f32>>,
    pos: f64,
    step: f64,
}

#[allow(dead_code)]
struct HdBgmStream {
    left: Arc<Vec<f32>>,
    right: Arc<Vec<f32>>,
    pos: f64,
    step: f64,
    looped: bool,
}

pub struct AudioMixer {
    pub synth: DualSynth,
    pub sample_rate: u32,
    pub music_enabled: bool,
    pub sfx_enabled: bool,
    hd_bgm: Option<HdBgmStream>,
    voices: Vec<SfxVoice>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl AudioMixer {
    pub fn new(sample_rate: u32, hd_mode: bool) -> Self {
        let mut synth = DualSynth::new(sample_rate);
        synth.hd_mode = hd_mode;
        synth.master_volume = 0.72;
        Self {
            synth,
            sample_rate,
            music_enabled: true,
            sfx_enabled: true,
            hd_bgm: None,
            voices: Vec::new(),
            scratch_l: vec![0.0f32; 2048],
            scratch_r: vec![0.0f32; 2048],
        }
    }

    #[allow(dead_code)]
    pub fn render_interleaved_stereo(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        if self.scratch_l.len() < frames {
            self.scratch_l.resize(frames, 0.0);
            self.scratch_r.resize(frames, 0.0);
        }
        let l = &mut self.scratch_l[..frames];
        let r = &mut self.scratch_r[..frames];
        l.fill(0.0);
        r.fill(0.0);
        if self.music_enabled {
            if self.synth.hd_mode {
                if let Some(bgm) = self.hd_bgm.as_mut() {
                    let len = bgm.left.len().min(bgm.right.len());
                    if len > 0 {
                        let len_f = len as f64;
                        let vol = 0.85f32;
                        for i in 0..frames {
                            if bgm.pos >= len_f {
                                if bgm.looped {
                                    bgm.pos %= len_f;
                                } else {
                                    break;
                                }
                            }
                            let idx = bgm.pos as usize;
                            let next = if idx + 1 < len {
                                idx + 1
                            } else if bgm.looped {
                                0
                            } else {
                                idx
                            };
                            let frac = (bgm.pos - idx as f64) as f32;
                            l[i] = (bgm.left[idx] + (bgm.left[next] - bgm.left[idx]) * frac) * vol;
                            r[i] = (bgm.right[idx] + (bgm.right[next] - bgm.right[idx]) * frac) * vol;
                            bgm.pos += bgm.step;
                        }
                    }
                } else {
                    self.synth.render_stereo(l, r);
                }
            } else {
                self.synth.render_stereo(l, r);
            }
        }
        if self.sfx_enabled && !self.voices.is_empty() {
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
        for i in 0..frames {
            out[i * 2] = l[i];
            out[i * 2 + 1] = r[i];
        }
    }
}

pub struct NativeAudio {
    assets_root: PathBuf,
    mixer: Arc<Mutex<AudioMixer>>,
    midi_cache: HashMap<i32, Vec<u8>>,
    hd_bgm_cache: HashMap<i32, (u32, Arc<Vec<f32>>, Arc<Vec<f32>>)>,
    hd_bgm_order: Vec<i32>,
    hd_bgm_missing: HashSet<i32>,
    sfx_cache: HashMap<i32, (u32, Arc<Vec<f32>>)>,
    current_track: i32,
    current_loop: bool,
    #[cfg(any(feature = "audio", target_os = "macos", target_os = "windows"))]
    _stream: Option<cpal::Stream>,
}

impl NativeAudio {
    pub fn new(assets_root: PathBuf, hd_mode: bool) -> Self {
        #[cfg(any(feature = "audio", target_os = "macos", target_os = "windows"))]
        let (sample_rate, mixer, stream) = start_cpal_stream(hd_mode);
        #[cfg(not(any(feature = "audio", target_os = "macos", target_os = "windows")))]
        let (sample_rate, mixer) = (
            44100u32,
            Arc::new(Mutex::new(AudioMixer::new(44100, hd_mode))),
        );
        let _ = sample_rate;
        Self {
            assets_root,
            mixer,
            midi_cache: HashMap::new(),
            hd_bgm_cache: HashMap::new(),
            hd_bgm_order: Vec::new(),
            hd_bgm_missing: HashSet::new(),
            sfx_cache: HashMap::new(),
            current_track: 0,
            current_loop: true,
            #[cfg(any(feature = "audio", target_os = "macos", target_os = "windows"))]
            _stream: stream,
        }
    }

    pub fn set_hd_mode(&mut self, hd: bool) {
        if let Ok(mut m) = self.mixer.lock() {
            m.synth.hd_mode = hd;
            if !hd {
                // Switching to 1995 Retro mode: ensure OPL2 MIDI is active for current track.
                if self.current_track > 0 {
                    if let Some(bytes) = self.midi_cache.get(&self.current_track) {
                        let _ = m.synth.load_midi(bytes, self.current_loop);
                    }
                }
            } else if self.current_track > 0 {
                if let Some((sr, left, right)) = self.hd_bgm_cache.get(&self.current_track) {
                    let step = (*sr as f64 / m.sample_rate.max(1) as f64).max(0.05);
                    m.hd_bgm = Some(HdBgmStream {
                        left: left.clone(),
                        right: right.clone(),
                        pos: 0.0,
                        step,
                        looped: self.current_loop,
                    });
                    m.synth.stop();
                }
            }
        }
    }

    pub fn toggle_music(&mut self) -> bool {
        if let Ok(mut m) = self.mixer.lock() {
            m.music_enabled = !m.music_enabled;
            return m.music_enabled;
        }
        false
    }

    pub fn toggle_sfx(&mut self) -> bool {
        if let Ok(mut m) = self.mixer.lock() {
            m.sfx_enabled = !m.sfx_enabled;
            return m.sfx_enabled;
        }
        false
    }

    pub fn play_music(&mut self, track_id: i32, looped: bool) {
        self.play_music_vfs(track_id, looped, None);
    }

    fn load_hd_bgm(
        &mut self,
        track_id: i32,
        vfs: Option<&mut pal_runtime::pack_vfs::OverlayVfs>,
    ) -> Option<(u32, Arc<Vec<f32>>, Arc<Vec<f32>>)> {
        if self.hd_bgm_missing.contains(&track_id) {
            return None;
        }
        if let Some(entry) = self.hd_bgm_cache.get(&track_id) {
            return Some(entry.clone());
        }
        let mut vfs_opt = vfs;
        for ext in ["mp3", "ogg"] {
            let key = format!("audio/bgm_hd/mus_{:03}.{ext}", track_id);
            let raw = if let Some(v) = vfs_opt.as_deref_mut() {
                v.read_bytes(&key).ok()
            } else {
                fs::read(self.assets_root.join(&key)).ok()
            };
            let Some(bytes) = raw else { continue };
            if let Some((sr, l, r)) = decode_compressed_stereo(&bytes, ext) {
                let al = Arc::new(l);
                let ar = Arc::new(r);
                if self.hd_bgm_cache.len() >= 6 {
                    if let Some(old_id) = self.hd_bgm_order.first().copied() {
                        self.hd_bgm_order.remove(0);
                        self.hd_bgm_cache.remove(&old_id);
                    }
                }
                self.hd_bgm_order.push(track_id);
                self.hd_bgm_cache
                    .insert(track_id, (sr, al.clone(), ar.clone()));
                return Some((sr, al, ar));
            }
        }
        self.hd_bgm_missing.insert(track_id);
        None
    }

    pub fn play_music_vfs(
        &mut self,
        track_id: i32,
        looped: bool,
        mut vfs: Option<&mut pal_runtime::pack_vfs::OverlayVfs>,
    ) {
        if track_id <= 0 {
            self.stop_music();
            return;
        }
        if self.current_track == track_id && self.current_loop == looped {
            return;
        }
        let hd_mode = self
            .mixer
            .lock()
            .map(|m| m.synth.hd_mode)
            .unwrap_or(true);

        // Pre-cache MIDI bytes so instant [F2] HD <-> 1995 OPL2 toggling always works.
        let midi_bytes = if let Some(b) = self.midi_cache.get(&track_id) {
            Some(b.clone())
        } else {
            let key = format!("audio/midi/mus_{:03}.mid", track_id);
            let loaded = if let Some(v) = vfs.as_deref_mut() {
                v.read_bytes(&key).ok()
            } else {
                fs::read(self.assets_root.join(&key)).ok()
            };
            if let Some(b) = loaded {
                self.midi_cache.insert(track_id, b.clone());
                Some(b)
            } else {
                None
            }
        };

        // Hybrid HD BGM (#3): prefer pre-rendered `audio/bgm_hd/mus_XXX.{mp3,ogg}` when in HD mode.
        let hd_entry = if hd_mode {
            self.load_hd_bgm(track_id, vfs)
        } else {
            None
        };

        if let Ok(mut m) = self.mixer.lock() {
            if let Some((sr, left, right)) = hd_entry {
                let step = (sr as f64 / m.sample_rate.max(1) as f64).max(0.05);
                m.hd_bgm = Some(HdBgmStream {
                    left,
                    right,
                    pos: 0.0,
                    step,
                    looped,
                });
                m.synth.stop();
                self.current_track = track_id;
                self.current_loop = looped;
                return;
            }
            m.hd_bgm = None;
            if let Some(bytes) = midi_bytes {
                if m.synth.load_midi(&bytes, looped) {
                    self.current_track = track_id;
                    self.current_loop = looped;
                }
            }
        }
    }

    pub fn stop_music(&mut self) {
        self.current_track = 0;
        if let Ok(mut m) = self.mixer.lock() {
            m.hd_bgm = None;
            m.synth.stop();
        }
    }

    #[allow(dead_code)]
    pub fn play_sfx(&mut self, sfx_id: i32) {
        self.play_sfx_vfs(sfx_id, None);
    }

    pub fn play_sfx_vfs(
        &mut self,
        sfx_id: i32,
        vfs: Option<&mut pal_runtime::pack_vfs::OverlayVfs>,
    ) {
        if sfx_id <= 0 {
            return;
        }
        let cached = if let Some(entry) = self.sfx_cache.get(&sfx_id) {
            Some(entry.clone())
        } else {
            let key = format!("audio/sfx/sfx_{:03}.wav", sfx_id);
            let parsed = if let Some(v) = vfs {
                v.read_bytes(&key).ok().and_then(|b| parse_wav_mono(&b))
            } else {
                load_wav_mono(&self.assets_root.join(&key))
            };
            parsed.map(|(sr, pcm)| {
                let arc = Arc::new(pcm);
                self.sfx_cache.insert(sfx_id, (sr, arc.clone()));
                (sr, arc)
            })
        };
        if let Some((src_rate, samples)) = cached {
            if let Ok(mut m) = self.mixer.lock() {
                if !m.sfx_enabled {
                    return;
                }
                let step = (src_rate as f64 / m.sample_rate.max(1) as f64).max(0.05);
                if m.voices.len() >= 16 {
                    m.voices.remove(0);
                }
                m.voices.push(SfxVoice {
                    samples,
                    pos: 0.0,
                    step,
                });
            }
        }
    }
}

/// Decode a pre-rendered compressed HD BGM file (`.mp3` or `.ogg` Vorbis) into stereo `f32` PCM
/// via pure-Rust `symphonia` (zero native C dependencies).
pub fn decode_compressed_stereo(bytes: &[u8], ext: &str) -> Option<(u32, Vec<f32>, Vec<f32>)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;
    let track = format.default_track()?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100).max(1000);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1);
        let frames = decoded.frames();
        if frames == 0 {
            continue;
        }
        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
        });
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        left.reserve(frames);
        right.reserve(frames);
        if ch == 1 {
            for &s in samples {
                left.push(s);
                right.push(s);
            }
        } else {
            for frame in samples.chunks_exact(ch) {
                left.push(frame[0]);
                right.push(frame[1]);
            }
        }
    }
    if left.is_empty() {
        None
    } else {
        Some((sample_rate, left, right))
    }
}

/// Minimal zero-dependency PCM WAV reader (8-bit unsigned or 16-bit signed, mono/stereo -> mono f32).
pub fn parse_wav_mono(bytes: &[u8]) -> Option<(u32, Vec<f32>)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut channels = 1u16;
    let mut sample_rate = 22050u32;
    let mut bits_per_sample = 8u16;
    let mut data_slice: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        let end = pos.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end - pos >= 16 {
            let fmt_tag = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
            if fmt_tag != 1 {
                return None;
            }
            channels = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]).max(1);
            sample_rate = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ])
            .max(1000);
            bits_per_sample = u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]);
        } else if id == b"data" {
            data_slice = Some(&bytes[pos..end]);
            break;
        }
        pos = end.saturating_add(size & 1);
    }

    let raw = data_slice?;
    let ch = channels as usize;
    let mut out = Vec::new();
    match bits_per_sample {
        8 => {
            for frame in raw.chunks_exact(ch) {
                let mut sum = 0.0f32;
                for &b in frame {
                    sum += (b as f32 - 128.0) / 128.0;
                }
                out.push(sum / ch as f32);
            }
        }
        16 => {
            for frame in raw.chunks_exact(ch * 2) {
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
    Some((sample_rate, out))
}

fn load_wav_mono(path: &Path) -> Option<(u32, Vec<f32>)> {
    let bytes = fs::read(path).ok()?;
    parse_wav_mono(&bytes)
}

#[cfg(any(feature = "audio", target_os = "macos", target_os = "windows"))]
fn start_cpal_stream(hd_mode: bool) -> (u32, Arc<Mutex<AudioMixer>>, Option<cpal::Stream>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let mixer = Arc::new(Mutex::new(AudioMixer::new(44100, hd_mode)));
        return (44100, mixer, None);
    };
    let Ok(default_cfg) = device.default_output_config() else {
        let mixer = Arc::new(Mutex::new(AudioMixer::new(44100, hd_mode)));
        return (44100, mixer, None);
    };
    let sample_rate = default_cfg.sample_rate().0;
    let channels = default_cfg.channels() as usize;
    let sample_format = default_cfg.sample_format();
    let stream_cfg: cpal::StreamConfig = default_cfg.into();
    let mixer = Arc::new(Mutex::new(AudioMixer::new(sample_rate, hd_mode)));
    let mixer_cb = mixer.clone();

    // Non-blocking try_lock on the real-time audio thread (#267): never stall the audio callback
    // waiting for main-thread MIDI/SFX mutations; emit silence on rare lock contention instead.
    let render_into_stereo = |mixer_cb: &Arc<Mutex<AudioMixer>>, stereo: &mut [f32]| {
        if let Ok(mut m) = mixer_cb.try_lock() {
            m.render_interleaved_stereo(stereo);
        } else {
            stereo.fill(0.0);
        }
    };

    let err_cb = |err| eprintln!("[pal-desktop] audio stream error: {err}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut stereo = vec![0.0f32; 4096];
            device
                .build_output_stream(
                    &stream_cfg,
                    move |data: &mut [f32], _| {
                        if channels == 2 {
                            render_into_stereo(&mixer_cb, data);
                        } else {
                            let frames = data.len() / channels.max(1);
                            if stereo.len() < frames * 2 {
                                stereo.resize(frames * 2, 0.0);
                            }
                            let st = &mut stereo[..frames * 2];
                            render_into_stereo(&mixer_cb, st);
                            for f in 0..frames {
                                let mono = 0.5 * (st[f * 2] + st[f * 2 + 1]);
                                for c in 0..channels {
                                    data[f * channels + c] = if channels == 1 {
                                        mono
                                    } else if c < 2 {
                                        st[f * 2 + c]
                                    } else {
                                        0.0
                                    };
                                }
                            }
                        }
                    },
                    err_cb,
                    None,
                )
                .ok()
        }
        cpal::SampleFormat::I16 => {
            let mut stereo = vec![0.0f32; 4096];
            device
                .build_output_stream(
                    &stream_cfg,
                    move |data: &mut [i16], _| {
                        let frames = data.len() / channels.max(1);
                        if stereo.len() < frames * 2 {
                            stereo.resize(frames * 2, 0.0);
                        }
                        let st = &mut stereo[..frames * 2];
                        render_into_stereo(&mixer_cb, st);
                        for f in 0..frames {
                            let mono = 0.5 * (st[f * 2] + st[f * 2 + 1]);
                            for c in 0..channels {
                                let s = if channels == 1 {
                                    mono
                                } else if c < 2 {
                                    st[f * 2 + c]
                                } else {
                                    0.0
                                };
                                data[f * channels + c] =
                                    (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                            }
                        }
                    },
                    err_cb,
                    None,
                )
                .ok()
        }
        cpal::SampleFormat::U16 => {
            let mut stereo = vec![0.0f32; 4096];
            device
                .build_output_stream(
                    &stream_cfg,
                    move |data: &mut [u16], _| {
                        let frames = data.len() / channels.max(1);
                        if stereo.len() < frames * 2 {
                            stereo.resize(frames * 2, 0.0);
                        }
                        let st = &mut stereo[..frames * 2];
                        render_into_stereo(&mixer_cb, st);
                        for f in 0..frames {
                            let mono = 0.5 * (st[f * 2] + st[f * 2 + 1]);
                            for c in 0..channels {
                                let s = if channels == 1 {
                                    mono
                                } else if c < 2 {
                                    st[f * 2 + c]
                                } else {
                                    0.0
                                };
                                data[f * channels + c] =
                                    ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32).round()
                                        as u16;
                            }
                        }
                    },
                    err_cb,
                    None,
                )
                .ok()
        }
        other => {
            eprintln!("[pal-desktop] unsupported audio sample format: {other:?}");
            None
        }
    };
    if let Some(s) = &stream {
        let _ = s.play();
    }
    (sample_rate, mixer, stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_8bit_and_mixer_render() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + 4).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&22050u32.to_le_bytes());
        wav.extend_from_slice(&22050u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[128, 255, 0, 128]);
        let (sr, pcm) = parse_wav_mono(&wav).expect("valid 8-bit wav");
        assert_eq!(sr, 22050);
        assert_eq!(pcm.len(), 4);
        assert!(pcm[1] > 0.9);
        assert!(pcm[2] < -0.9);

        let mut mixer = AudioMixer::new(44100, true);
        let mut out = vec![0.0f32; 256];
        mixer.render_interleaved_stereo(&mut out);
        assert_eq!(out.len(), 256);
    }

    #[test]
    fn hd_bgm_stream_loops_and_respects_hd_toggle() {
        let mut mixer = AudioMixer::new(44100, true);
        mixer.hd_bgm = Some(HdBgmStream {
            left: Arc::new(vec![0.5, 0.5, 0.5, 0.5]),
            right: Arc::new(vec![-0.5, -0.5, -0.5, -0.5]),
            pos: 0.0,
            step: 1.0,
            looped: true,
        });
        let mut out = vec![0.0f32; 16]; // 8 stereo frames (>4, tests wrap-around loop)
        mixer.render_interleaved_stereo(&mut out);
        assert!(out[0] > 0.4 && out[1] < -0.4);
        assert!(out[14] > 0.4 && out[15] < -0.4);

        // Switching to 1995 Retro mode bypasses hd_bgm (silencing it when no MIDI is loaded)
        mixer.synth.hd_mode = false;
        out.fill(0.0);
        mixer.render_interleaved_stereo(&mut out);
        assert_eq!(out[0], 0.0);
    }
}
