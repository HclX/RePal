//! Dual-mode MIDI Sequencer & Synthesizer:
//! - Mode 0 (`hd_mode = true`): Ancient Chinese / Orchestral acoustic physical-harmonic synth + Stereo Hall Reverb.
//! - Mode 1 (`hd_mode = false`): Classic 1995 OPL2 (YM3812) 2-Operator FM chip synthesizer.
//! Supports instant mid-song switching without losing beat or note state!

use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug)]
struct MidiEvent {
    time_sec: f32,
    kind: EventKind,
}

#[derive(Clone, Copy, Debug)]
enum EventKind {
    NoteOn { ch: u8, note: u8, vel: u8 },
    NoteOff { ch: u8, note: u8 },
    Program { ch: u8, prog: u8 },
    ControlVolume { ch: u8, vol: u8 },
    ControlPan { ch: u8, pan: u8 },
    PitchBend { ch: u8, bend: i16 },
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    released: bool,
    ch: u8,
    note: u8,
    vel: f32,
    prog: u8,
    phase: f32,
    mod_phase: f32,
    base_freq: f32,
    age: f32,
    rel_age: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            released: false,
            ch: 0,
            note: 60,
            vel: 0.0,
            prog: 0,
            phase: 0.0,
            mod_phase: 0.0,
            base_freq: 261.6256,
            age: 0.0,
            rel_age: 0.0,
        }
    }
}

struct CombFilter {
    buf: Vec<f32>,
    idx: usize,
    feedback: f32,
    damp: f32,
    store: f32,
}

impl CombFilter {
    fn new(size: usize, feedback: f32, damp: f32) -> Self {
        Self {
            buf: vec![0.0; size.max(16)],
            idx: 0,
            feedback,
            damp,
            store: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * (1.0 - self.damp) + self.store * self.damp;
        self.buf[self.idx] = input + self.store * self.feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct AllPassFilter {
    buf: Vec<f32>,
    idx: usize,
}

impl AllPassFilter {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0.0; size.max(16)],
            idx: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let bufout = self.buf[self.idx];
        let out = -input + bufout;
        self.buf[self.idx] = input + bufout * 0.5;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

pub struct DualSynth {
    sample_rate: f32,
    pub hd_mode: bool,
    pub loop_enabled: bool,
    pub master_volume: f32,
    events: Vec<MidiEvent>,
    duration_sec: f64,
    cursor_idx: usize,
    play_time: f64,
    playing: bool,
    ch_prog: [u8; 16],
    ch_vol: [f32; 16],
    ch_pan: [f32; 16],
    ch_bend: [f32; 16],
    voices: [Voice; 32],
    noise_seed: u32,
    combs_l: [CombFilter; 4],
    combs_r: [CombFilter; 4],
    allpass_l: [AllPassFilter; 2],
    allpass_r: [AllPassFilter; 2],
}

impl DualSynth {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.clamp(8000, 192_000) as f32;
        let scale = sr / 44100.0;
        let mk_c = |n: usize, spread: usize| {
            CombFilter::new(((n + spread) as f32 * scale) as usize, 0.79, 0.28)
        };
        let mk_a =
            |n: usize, spread: usize| AllPassFilter::new(((n + spread) as f32 * scale) as usize);

        Self {
            sample_rate: sr,
            hd_mode: true,
            loop_enabled: true,
            master_volume: 0.65,
            events: Vec::new(),
            duration_sec: 0.0,
            cursor_idx: 0,
            play_time: 0.0,
            playing: false,
            ch_prog: [0; 16],
            ch_vol: [0.85; 16],
            ch_pan: [0.5; 16],
            ch_bend: [0.0; 16],
            voices: [Voice::default(); 32],
            noise_seed: 0x1234_5678,
            combs_l: [mk_c(1116, 0), mk_c(1188, 0), mk_c(1277, 0), mk_c(1356, 0)],
            combs_r: [
                mk_c(1116, 23),
                mk_c(1188, 23),
                mk_c(1277, 23),
                mk_c(1356, 23),
            ],
            allpass_l: [mk_a(556, 0), mk_a(441, 0)],
            allpass_r: [mk_a(556, 23), mk_a(441, 23)],
        }
    }

    fn clear_reverb(&mut self) {
        for c in self.combs_l.iter_mut().chain(self.combs_r.iter_mut()) {
            c.buf.fill(0.0);
            c.idx = 0;
            c.store = 0.0;
        }
        for a in self.allpass_l.iter_mut().chain(self.allpass_r.iter_mut()) {
            a.buf.fill(0.0);
            a.idx = 0;
        }
    }

    pub fn stop(&mut self) {
        self.playing = false;
        for v in &mut self.voices {
            v.active = false;
        }
        self.clear_reverb();
    }

    /// Load a Standard MIDI File (`MThd`) and begin playback.
    pub fn load_midi(&mut self, midi: &[u8], loop_play: bool) -> bool {
        let Some((events, duration)) = parse_smf(midi) else {
            return false;
        };
        self.events = events;
        self.duration_sec = (duration as f64).max(0.5);
        self.cursor_idx = 0;
        self.play_time = 0.0;
        self.loop_enabled = loop_play;
        self.playing = true;
        self.ch_prog = [0; 16];
        self.ch_vol = [0.85; 16];
        self.ch_pan = [0.5; 16];
        self.ch_bend = [0.0; 16];
        for v in &mut self.voices {
            v.active = false;
        }
        self.clear_reverb();
        true
    }

    #[inline]
    fn next_noise(&mut self) -> f32 {
        self.noise_seed = self
            .noise_seed
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        ((self.noise_seed >> 8) as f32 / 8_388_608.0) - 1.0
    }

    fn dispatch_event(&mut self, ev: EventKind) {
        match ev {
            EventKind::NoteOn { ch, note, vel } => {
                let c = (ch & 0x0F) as usize;
                if vel == 0 {
                    self.note_off(ch, note);
                    return;
                }
                // Reuse matching note on channel or pick quietest/free voice
                let mut slot = None;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.active && !v.released && v.ch == ch && v.note == note {
                        slot = Some(i);
                        break;
                    }
                }
                if slot.is_none() {
                    for (i, v) in self.voices.iter().enumerate() {
                        if !v.active {
                            slot = Some(i);
                            break;
                        }
                    }
                }
                let idx = slot.unwrap_or_else(|| {
                    self.voices
                        .iter()
                        .enumerate()
                        .max_by(|a, b| {
                            a.1.age
                                .partial_cmp(&b.1.age)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                });
                let base_freq = 440.0 * 2.0f32.powf((note as f32 + self.ch_bend[c] - 69.0) / 12.0);
                self.voices[idx] = Voice {
                    active: true,
                    released: false,
                    ch,
                    note,
                    vel: (vel as f32 / 127.0).sqrt(),
                    prog: self.ch_prog[c],
                    phase: 0.0,
                    mod_phase: 0.0,
                    base_freq,
                    age: 0.0,
                    rel_age: 0.0,
                };
            }
            EventKind::NoteOff { ch, note } => self.note_off(ch, note),
            EventKind::Program { ch, prog } => {
                self.ch_prog[(ch & 0x0F) as usize] = prog;
            }
            EventKind::ControlVolume { ch, vol } => {
                self.ch_vol[(ch & 0x0F) as usize] = (vol as f32 / 127.0).clamp(0.0, 1.0);
            }
            EventKind::ControlPan { ch, pan } => {
                self.ch_pan[(ch & 0x0F) as usize] = (pan as f32 / 127.0).clamp(0.0, 1.0);
            }
            EventKind::PitchBend { ch, bend } => {
                // +/- 2 semitones standard bend range
                let ci = (ch & 0x0F) as usize;
                let semitones = (bend as f32 / 8192.0) * 2.0;
                self.ch_bend[ci] = semitones;
                for v in &mut self.voices {
                    if v.active && v.ch == ch {
                        v.base_freq = 440.0 * 2.0f32.powf((v.note as f32 + semitones - 69.0) / 12.0);
                    }
                }
            }
        }
    }

    fn note_off(&mut self, ch: u8, note: u8) {
        for v in &mut self.voices {
            if v.active && !v.released && v.ch == ch && v.note == note {
                v.released = true;
                v.rel_age = 0.0;
            }
        }
    }

    /// Render `frames` stereo samples into `out_l` and `out_r`.
    pub fn render_stereo(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        if !self.playing || self.events.is_empty() {
            out_l[..frames].fill(0.0);
            out_r[..frames].fill(0.0);
            return;
        }

        let dt = 1.0 / self.sample_rate;
        let dt_f64 = 1.0 / self.sample_rate as f64;
        let hd = self.hd_mode;
        let vol_master = self.master_volume;

        for i in 0..frames {
            while self.cursor_idx < self.events.len()
                && (self.events[self.cursor_idx].time_sec as f64) <= self.play_time
            {
                let ev = self.events[self.cursor_idx].kind;
                self.cursor_idx += 1;
                self.dispatch_event(ev);
            }

            let mut dry_l = 0.0f32;
            let mut dry_r = 0.0f32;
            let noise = self.next_noise();

            for vi in 0..self.voices.len() {
                if !self.voices[vi].active {
                    continue;
                }
                let mut v = self.voices[vi];
                v.age += dt;
                if v.released {
                    v.rel_age += dt;
                }

                let ch_idx = (v.ch & 0x0F) as usize;
                let ch_gain = self.ch_vol[ch_idx] * v.vel;
                let pan = if hd { self.ch_pan[ch_idx] } else { 0.5 };

                let sample = if (v.ch & 0x0F) == 9 {
                    // GM Channel 10 Percussion
                    if v.note == 35 || v.note == 36 || v.note == 41 || v.note == 45 {
                        // Chinese Bass Drum / Taiko Tom
                        let env = (-v.age * 16.0).exp();
                        if env < 0.002 {
                            v.active = false;
                        }
                        let f = 62.0 + 90.0 * (-v.age * 45.0).exp();
                        v.phase = (v.phase + f * dt).fract();
                        (v.phase * TAU).sin() * env * 0.9
                    } else if v.note == 42 || v.note == 44 || v.note == 46 || v.note == 49 {
                        // Hi-hat / Cymbal
                        let rate = if v.note == 46 || v.note == 49 { 6.0 } else { 28.0 };
                        let env = (-v.age * rate).exp();
                        if env < 0.002 {
                            v.active = false;
                        }
                        noise * env * 0.35
                    } else {
                        // Woodblock / Snare / Hand drum
                        let env = (-v.age * 24.0).exp();
                        if env < 0.002 {
                            v.active = false;
                        }
                        let f = 220.0 + (v.note as f32) * 4.0;
                        v.phase = (v.phase + f * dt).fract();
                        ((v.phase * TAU).sin() * 0.6 + noise * 0.4) * env * 0.55
                    }
                } else {
                    let base_freq = v.base_freq;

                    if hd {
                        // HD Mode: Ancient Chinese / Orchestral Acoustic Modeling
                        let prog = v.prog;
                        if matches!(prog, 0..=7 | 14..=15 | 24..=31 | 46 | 104..=107) {
                            // Plucked Guzheng / Pipa / Dulcimer / Harp
                            let rel_env = if v.released {
                                (-v.rel_age * 14.0).exp()
                            } else {
                                1.0
                            };
                            let pluck_env = (1.0 - (-v.age * 450.0).exp())
                                * ((-v.age * 2.4).exp() * 0.85 + 0.15)
                                * rel_env;
                            if pluck_env < 0.0015 || v.age > 6.0 {
                                v.active = false;
                            }
                            let bright = (-v.age * 7.5).exp();
                            v.phase = (v.phase + base_freq * dt).fract();
                            let p1 = (v.phase * TAU).sin();
                            let p2 = (v.phase * 2.0 * TAU).sin() * (0.42 * bright + 0.12);
                            let p3 = (v.phase * 3.0 * TAU).sin() * (0.25 * bright);
                            let p4 = (v.phase * 4.0 * TAU).sin() * (0.12 * bright);
                            (p1 + p2 + p3 + p4) * pluck_env * 0.28
                        } else if matches!(prog, 64..=79 | 109 | 111) {
                            // Bamboo Flute (Dizi) / Xiao / Ocarina
                            let atk = (v.age * 18.0).min(1.0);
                            let rel = if v.released {
                                (-v.rel_age * 12.0).exp()
                            } else {
                                1.0
                            };
                            let env = atk * rel;
                            if v.released && env < 0.0015 {
                                v.active = false;
                            }
                            let vib_depth = (v.age - 0.18).max(0.0).min(0.35) * 0.012;
                            let vib = (v.age * 5.6 * TAU).sin() * vib_depth;
                            v.phase = (v.phase + base_freq * (1.0 + vib) * dt).fract();
                            let p1 = (v.phase * TAU).sin();
                            let p2 = (v.phase * 2.0 * TAU).sin() * 0.28;
                            let p3 = (v.phase * 3.0 * TAU).sin() * 0.14;
                            let breath = noise * 0.03 * (-v.age * 8.0).exp();
                            (p1 + p2 + p3 + breath) * env * 0.26
                        } else {
                            // Bowed Erhu / Warm Orchestral Strings / Choir Pad
                            let atk = (v.age * 9.0).min(1.0);
                            let rel = if v.released {
                                (-v.rel_age * 8.0).exp()
                            } else {
                                1.0
                            };
                            let env = atk * rel;
                            if v.released && env < 0.0015 {
                                v.active = false;
                            }
                            let vib = (v.age * 5.2 * TAU).sin()
                                * ((v.age - 0.15).max(0.0).min(0.4) * 0.009);
                            v.phase = (v.phase + base_freq * (1.0 + vib) * dt).fract();
                            let p1 = (v.phase * TAU).sin();
                            let p2 = (v.phase * 2.0 * TAU).sin() * 0.36;
                            let p3 = (v.phase * 3.0 * TAU).sin() * 0.18;
                            let p5 = (v.phase * 5.0 * TAU).sin() * 0.07;
                            (p1 + p2 + p3 + p5) * env * 0.23
                        }
                    } else {
                        // 1995 Classic Mode: Authentic OPL2 2-Operator FM Synthesis!
                        let (ratio_m, mod_idx_base, decay_rate) = match v.prog {
                            0..=31 | 46 | 104..=107 => (2.0, 2.2, 3.2),
                            64..=79 => (1.0, 0.9, 0.6),
                            _ => (1.0, 1.6, 1.1),
                        };
                        let sustain = (-v.age * decay_rate * 0.4).exp() * 0.65 + 0.35;
                        let rel = if v.released {
                            sustain * (-v.rel_age * 16.0).exp()
                        } else {
                            sustain
                        };
                        let atk = (v.age * 60.0).min(1.0);
                        let env = atk * rel;
                        if v.released && (env < 0.002 || v.rel_age > 0.4) {
                            v.active = false;
                        }
                        let mod_env = (-v.age * decay_rate * 2.0).exp() * 0.7 + 0.3;
                        v.mod_phase = (v.mod_phase + base_freq * ratio_m * dt).fract();
                        v.phase = (v.phase + base_freq * dt).fract();
                        let modulator = (v.mod_phase * TAU).sin() * mod_idx_base * mod_env;
                        // Subtle 1995 12-bit DAC quantization crunch
                        let raw_fm = (v.phase * TAU + modulator).sin() * env * 0.24;
                        (raw_fm * 512.0).round() / 512.0
                    }
                };

                self.voices[vi] = v;
                let s = sample * ch_gain;
                dry_l += s * (1.0 - pan * 0.65);
                dry_r += s * (0.35 + pan * 0.65);
            }

            if hd {
                // Lush Stereo Hall Reverb for HD Mode
                let rev_in = (dry_l + dry_r) * 0.18;
                let mut wet_l = 0.0f32;
                let mut wet_r = 0.0f32;
                for c in &mut self.combs_l {
                    wet_l += c.process(rev_in);
                }
                for c in &mut self.combs_r {
                    wet_r += c.process(rev_in);
                }
                for a in &mut self.allpass_l {
                    wet_l = a.process(wet_l);
                }
                for a in &mut self.allpass_r {
                    wet_r = a.process(wet_r);
                }
                out_l[i] = ((dry_l + wet_l * 0.28) * vol_master).clamp(-0.98, 0.98);
                out_r[i] = ((dry_r + wet_r * 0.28) * vol_master).clamp(-0.98, 0.98);
            } else {
                // Dry Classic OPL2 SoundBlaster Output
                out_l[i] = (dry_l * vol_master).clamp(-0.98, 0.98);
                out_r[i] = (dry_r * vol_master).clamp(-0.98, 0.98);
            }

            self.play_time += dt_f64;
            if self.play_time >= self.duration_sec {
                if self.loop_enabled {
                    self.play_time = 0.0;
                    self.cursor_idx = 0;
                    self.ch_prog = [0; 16];
                    self.ch_vol = [0.85; 16];
                    self.ch_pan = [0.5; 16];
                    self.ch_bend = [0.0; 16];
                    for v in &mut self.voices {
                        if v.active && !v.released {
                            v.released = true;
                            v.rel_age = 0.0;
                        }
                    }
                } else {
                    self.playing = false;
                }
            }
        }
    }
}

fn read_vlq(data: &[u8], pos: &mut usize) -> Option<u32> {
    let mut val = 0u32;
    for _ in 0..4 {
        let b = *data.get(*pos)?;
        *pos += 1;
        val = (val << 7) | ((b & 0x7F) as u32);
        if (b & 0x80) == 0 {
            return Some(val);
        }
    }
    Some(val)
}

#[derive(Clone, Copy)]
enum RawTrackEvent {
    Tempo(u32),
    Midi(EventKind),
}

fn parse_smf(data: &[u8]) -> Option<(Vec<MidiEvent>, f32)> {
    if data.len() < 14 || &data[0..4] != b"MThd" {
        return None;
    }
    let hdr_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let num_tracks = u16::from_be_bytes([data[10], data[11]]) as usize;
    let division = u16::from_be_bytes([data[12], data[13]]);
    if (division & 0x8000) != 0 || division == 0 {
        return None;
    }
    let tpqn = division as f64;

    let mut pos = match 8usize.checked_add(hdr_len) {
        Some(p) if p <= data.len() => p,
        _ => return None,
    };
    let mut raw_events: Vec<(u64, RawTrackEvent)> = Vec::new();
    let mut max_tick = 0u64;

    for _ in 0..num_tracks {
        let Some(chunk_start) = pos.checked_add(8) else {
            break;
        };
        if chunk_start > data.len() {
            break;
        }
        let chunk_id = &data[pos..pos + 4];
        let chunk_len =
            u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                as usize;
        pos = chunk_start;
        let end = pos.saturating_add(chunk_len).min(data.len());
        if chunk_id != b"MTrk" {
            pos = end;
            continue;
        }

        let mut tpos = pos;
        let mut abs_tick = 0u64;
        let mut running_status = 0u8;

        while tpos < end {
            let Some(delta) = read_vlq(data, &mut tpos) else {
                break;
            };
            abs_tick += delta as u64;
            max_tick = max_tick.max(abs_tick);

            let Some(&first) = data.get(tpos) else {
                break;
            };
            let status = if (first & 0x80) != 0 {
                tpos += 1;
                if first < 0xF0 {
                    running_status = first;
                }
                first
            } else {
                running_status
            };

            if status == 0xFF {
                let Some(&meta_type) = data.get(tpos) else {
                    break;
                };
                tpos += 1;
                let Some(mlen) = read_vlq(data, &mut tpos) else {
                    break;
                };
                let mlen = mlen as usize;
                if meta_type == 0x51 && mlen == 3 && tpos.saturating_add(3) <= end {
                    let us_per_qn = ((data[tpos] as u32) << 16)
                        | ((data[tpos + 1] as u32) << 8)
                        | (data[tpos + 2] as u32);
                    raw_events.push((abs_tick, RawTrackEvent::Tempo(us_per_qn.max(10_000))));
                }
                tpos = tpos.saturating_add(mlen).min(end);
            } else if status == 0xF0 || status == 0xF7 {
                let Some(slen) = read_vlq(data, &mut tpos) else {
                    break;
                };
                tpos = tpos.saturating_add(slen as usize).min(end);
            } else {
                let cmd = status & 0xF0;
                let ch = status & 0x0F;
                match cmd {
                    0x80 => {
                        if tpos + 2 > end {
                            break;
                        }
                        let note = data[tpos] & 0x7F;
                        tpos += 2;
                        raw_events.push((abs_tick, RawTrackEvent::Midi(EventKind::NoteOff { ch, note })));
                    }
                    0x90 => {
                        if tpos + 2 > end {
                            break;
                        }
                        let note = data[tpos] & 0x7F;
                        let vel = data[tpos + 1] & 0x7F;
                        tpos += 2;
                        raw_events.push((
                            abs_tick,
                            RawTrackEvent::Midi(EventKind::NoteOn { ch, note, vel }),
                        ));
                    }
                    0xB0 => {
                        if tpos + 2 > end {
                            break;
                        }
                        let cc = data[tpos] & 0x7F;
                        let val = data[tpos + 1] & 0x7F;
                        tpos += 2;
                        if cc == 7 {
                            raw_events.push((
                                abs_tick,
                                RawTrackEvent::Midi(EventKind::ControlVolume { ch, vol: val }),
                            ));
                        } else if cc == 10 {
                            raw_events.push((
                                abs_tick,
                                RawTrackEvent::Midi(EventKind::ControlPan { ch, pan: val }),
                            ));
                        }
                    }
                    0xC0 => {
                        if tpos + 1 > end {
                            break;
                        }
                        let prog = data[tpos] & 0x7F;
                        tpos += 1;
                        raw_events.push((
                            abs_tick,
                            RawTrackEvent::Midi(EventKind::Program { ch, prog }),
                        ));
                    }
                    0xE0 => {
                        if tpos + 2 > end {
                            break;
                        }
                        let lsb = data[tpos] & 0x7F;
                        let msb = data[tpos + 1] & 0x7F;
                        tpos += 2;
                        let bend = (((msb as i16) << 7) | (lsb as i16)) - 8192;
                        raw_events.push((
                            abs_tick,
                            RawTrackEvent::Midi(EventKind::PitchBend { ch, bend }),
                        ));
                    }
                    0xA0 | 0xD0 => {
                        let skip = if cmd == 0xD0 { 1 } else { 2 };
                        tpos = (tpos + skip).min(end);
                    }
                    _ => break,
                }
            }
        }
        pos = end;
    }

    raw_events.sort_by_key(|(tick, _)| *tick);

    let mut sec_accum = 0.0f64;
    let mut last_tick = 0u64;
    let mut us_per_qn = 500_000.0f64; // Default 120 BPM
    let mut out = Vec::with_capacity(raw_events.len());

    for (tick, rev) in raw_events {
        let dt_ticks = (tick - last_tick) as f64;
        sec_accum += (dt_ticks / tpqn) * (us_per_qn / 1_000_000.0);
        last_tick = tick;
        match rev {
            RawTrackEvent::Tempo(t) => {
                us_per_qn = t as f64;
            }
            RawTrackEvent::Midi(kind) => {
                out.push(MidiEvent {
                    time_sec: sec_accum as f32,
                    kind,
                });
            }
        }
    }

    let total_dur = sec_accum
        + ((max_tick.saturating_sub(last_tick)) as f64 / tpqn) * (us_per_qn / 1_000_000.0)
        + 0.4;
    Some((out, total_dur as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retro_opl2_melodic_notes_do_not_die_on_sample_one() {
        // Minimal 1-track SMF with a single C4 NoteOn at tick 0 on channel 0.
        let mut midi = Vec::new();
        midi.extend_from_slice(b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x60");
        let track: &[u8] = &[
            0x00, 0x90, 60, 100, // tick 0: NoteOn ch0 C4 vel 100
            0x60, 0x80, 60, 0,   // tick 96: NoteOff
            0x00, 0xFF, 0x2F, 0x00, // End of track
        ];
        midi.extend_from_slice(b"MTrk");
        midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi.extend_from_slice(track);

        let mut synth = DualSynth::new(48_000);
        synth.hd_mode = false; // 1995 Retro OPL2 FM mode
        assert!(synth.load_midi(&midi, false));

        let mut l = vec![0.0f32; 2048];
        let mut r = vec![0.0f32; 2048];
        synth.render_stereo(&mut l, &mut r);

        let peak = l
            .iter()
            .chain(r.iter())
            .fold(0.0f32, |acc, &x| acc.max(x.abs()));
        assert!(
            peak > 0.01,
            "1995 Retro OPL2 melodic voice died early (peak={peak})"
        );
    }
}
