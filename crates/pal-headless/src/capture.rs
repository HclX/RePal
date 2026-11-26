//! Multi-frame burst capture & deterministic `.repal` → MP4/GIF/PNG-sequence video export
//! with tick-exact stereo audio muxing (HD BGM / OPL3 MIDI + SFX WAV, #11).
//!
//! Zero extra Rust codec dependencies:
//! - Video/GIF targets (`.mp4`, `.mkv`, `.webm`, `.mov`, `.gif`) stream raw RGBA framebuffers
//!   directly into `ffmpeg -f rawvideo` via `stdin` (zero temporary PNG disk I/O).
//! - Audio-capable video containers (`.mp4`, `.mkv`, `.webm`, `.mov`) render exact 20 ms stereo
//!   PCM chunks (`882` frames @ `44,100 Hz`) per simulation tick via [`OfflineAudioMixer`] and
//!   fast-mux with `-c:v copy` (no video re-encode) on `finish()`.
//! - Directory targets write numbered PNG frames (`frame_00000.png`, ...), working even without
//!   `ffmpeg` installed.

use crate::offline_audio::{OfflineAudioMixer, OFFLINE_SAMPLE_RATE};
use crate::{AudioEvent, HeadlessApp};
use pal_render::framebuffer::Framebuffer;
use pal_runtime::repal_archive::read_repal_file;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as ProcCommand, Stdio};

#[derive(Clone, Debug)]
pub struct CaptureSummary {
    pub path: PathBuf,
    pub format: &'static str,
    pub frames: u64,
    pub ticks: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub stride: u32,
    pub has_audio: bool,
}

struct AudioTrackState {
    tmp_video_path: PathBuf,
    tmp_audio_path: PathBuf,
    pcm_writer: Option<BufWriter<File>>,
    mixer: OfflineAudioMixer,
    pcm_scratch: Vec<u8>,
}

enum CaptureSink {
    PngDir {
        dir: PathBuf,
    },
    Ffmpeg {
        child: Option<Child>,
        stdin: Option<ChildStdin>,
        format: &'static str,
        audio: Option<AudioTrackState>,
    },
}

/// Stateful per-tick framebuffer + stereo audio recorder shared by Text Mode `capture` and `--replay-export-video`.
pub struct FrameRecorder {
    out_path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    stride: u32,
    with_thought: bool,
    ticks_seen: u64,
    frames_written: u64,
    sink: CaptureSink,
}

fn video_format_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" => Some("mp4"),
        "mkv" => Some("mkv"),
        "webm" => Some("webm"),
        "mov" => Some("mov"),
        "gif" => Some("gif"),
        _ => None,
    }
}

fn container_supports_audio(fmt: &str) -> bool {
    matches!(fmt, "mp4" | "mkv" | "webm" | "mov")
}

fn make_temp_sibling(out_path: &Path, tag: &str, ext: &str) -> PathBuf {
    let dir = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let stem = out_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("repal_capture");
    dir.join(format!(".{stem}.repal_{tag}_{}.{ext}", std::process::id()))
}

impl FrameRecorder {
    /// Open a video-only recorder (backwards-compatible shorthand).
    pub fn open(
        out_path: impl Into<PathBuf>,
        width: u32,
        height: u32,
        stride: u32,
        with_thought: bool,
    ) -> Result<Self, String> {
        Self::open_with_audio(out_path, width, height, stride, with_thought, false, true)
    }

    /// Open a recorder targeting either an audio-muxed video/GIF file (via `ffmpeg`) or a PNG
    /// sequence directory.
    pub fn open_with_audio(
        out_path: impl Into<PathBuf>,
        width: u32,
        height: u32,
        stride: u32,
        with_thought: bool,
        with_audio: bool,
        hd_mode: bool,
    ) -> Result<Self, String> {
        let out_path = out_path.into();
        let stride = stride.max(1);
        let fps = (50 / stride).max(1);

        let sink = if let Some(fmt) = video_format_for_path(&out_path) {
            if let Some(parent) = out_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create output dir {}: {e}", parent.display()))?;
                }
            }
            let enable_audio = with_audio && container_supports_audio(fmt);
            let (video_target, audio_state) = if enable_audio {
                let tmp_vid = make_temp_sibling(&out_path, "vid", fmt);
                let tmp_aud = make_temp_sibling(&out_path, "aud", "f32le");
                let pcm_file = File::create(&tmp_aud).map_err(|e| {
                    format!("create temp audio file {}: {e}", tmp_aud.display())
                })?;
                (
                    tmp_vid.clone(),
                    Some(AudioTrackState {
                        tmp_video_path: tmp_vid,
                        tmp_audio_path: tmp_aud,
                        pcm_writer: Some(BufWriter::with_capacity(64 * 1024, pcm_file)),
                        mixer: OfflineAudioMixer::new(hd_mode),
                        pcm_scratch: Vec::with_capacity(8192),
                    }),
                )
            } else {
                (out_path.clone(), None)
            };

            let size = format!("{width}x{height}");
            let fps_s = fps.to_string();
            let mut cmd = ProcCommand::new("ffmpeg");
            cmd.args([
                "-y",
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                &size,
                "-r",
                &fps_s,
                "-i",
                "-",
            ]);
            match fmt {
                "gif" => {
                    cmd.args([
                        "-vf",
                        "split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse",
                    ]);
                }
                "webm" => {
                    cmd.args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p"]);
                }
                _ => {
                    cmd.args([
                        "-c:v",
                        "libx264",
                        "-preset",
                        "veryfast",
                        "-crf",
                        "20",
                        "-pix_fmt",
                        "yuv420p",
                    ]);
                }
            }
            cmd.arg(&video_target)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());

            let mut child = cmd.spawn().map_err(|e| {
                if let Some(aud) = &audio_state {
                    let _ = fs::remove_file(&aud.tmp_audio_path);
                    let _ = fs::remove_file(&aud.tmp_video_path);
                }
                format!(
                    "failed to spawn ffmpeg for {} ({e}); install ffmpeg or pass a directory path for PNG frames",
                    out_path.display()
                )
            })?;
            let stdin = child.stdin.take();
            CaptureSink::Ffmpeg {
                child: Some(child),
                stdin,
                format: fmt,
                audio: audio_state,
            }
        } else {
            fs::create_dir_all(&out_path)
                .map_err(|e| format!("create capture dir {}: {e}", out_path.display()))?;
            CaptureSink::PngDir {
                dir: out_path.clone(),
            }
        };

        Ok(Self {
            out_path,
            width,
            height,
            fps,
            stride,
            with_thought,
            ticks_seen: 0,
            frames_written: 0,
            sink,
        })
    }

    pub fn with_thought(&self) -> bool {
        self.with_thought
    }

    pub fn has_audio(&self) -> bool {
        matches!(
            &self.sink,
            CaptureSink::Ffmpeg {
                audio: Some(_),
                ..
            }
        )
    }

    pub fn out_path(&self) -> &Path {
        &self.out_path
    }

    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    pub fn ticks_seen(&self) -> u64 {
        self.ticks_seen
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn format(&self) -> &'static str {
        match &self.sink {
            CaptureSink::PngDir { .. } => "png_dir",
            CaptureSink::Ffmpeg { format, .. } => format,
        }
    }

    /// Feed audio events during pre-roll ticks (`0..start_tick`) without writing PCM or video frames,
    /// so active BGM is already playing when recording starts.
    pub fn pre_roll_audio<F>(&mut self, events: &[AudioEvent], hd_mode: bool, read_asset: F)
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if let CaptureSink::Ffmpeg {
            audio: Some(aud), ..
        } = &mut self.sink
        {
            if !events.is_empty() {
                aud.mixer.apply_events(events, hd_mode, read_asset);
            }
        }
    }

    /// Record one simulation tick without new audio events (continuing any active BGM/SFX).
    pub fn push_tick(&mut self, fb: &Framebuffer) -> Result<(), String> {
        self.push_tick_with_audio(fb, &[], true, |_| None)
    }

    /// Record one simulation tick: renders 20 ms of stereo audio on EVERY tick (regardless of
    /// `stride`) and writes a video frame when `ticks_seen % stride == 0`.
    pub fn push_tick_with_audio<F>(
        &mut self,
        fb: &Framebuffer,
        audio_events: &[AudioEvent],
        hd_mode: bool,
        read_asset: F,
    ) -> Result<(), String>
    where
        F: FnMut(&str) -> Option<Vec<u8>>,
    {
        if let CaptureSink::Ffmpeg {
            audio: Some(aud), ..
        } = &mut self.sink
        {
            aud.mixer.apply_events(audio_events, hd_mode, read_asset);
            aud.pcm_scratch.clear();
            aud.mixer.render_tick_f32le(&mut aud.pcm_scratch);
            if let Some(w) = aud.pcm_writer.as_mut() {
                w.write_all(&aud.pcm_scratch)
                    .map_err(|e| format!("write temp audio PCM: {e}"))?;
            }
        }

        let keep = self.ticks_seen % (self.stride as u64) == 0;
        self.ticks_seen += 1;
        if !keep {
            return Ok(());
        }
        if fb.width != self.width || fb.height != self.height {
            return Err(format!(
                "capture resolution changed mid-recording ({}x{} -> {}x{}); stop capture before toggling HD/Retro",
                self.width, self.height, fb.width, fb.height
            ));
        }
        match &mut self.sink {
            CaptureSink::PngDir { dir } => {
                let path = dir.join(format!("frame_{:05}.png", self.frames_written));
                image::save_buffer(
                    &path,
                    &fb.pixels,
                    fb.width,
                    fb.height,
                    image::ColorType::Rgba8,
                )
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
            CaptureSink::Ffmpeg { stdin, .. } => {
                let pipe = stdin
                    .as_mut()
                    .ok_or_else(|| "ffmpeg stdin already closed".to_string())?;
                pipe.write_all(&fb.pixels)
                    .map_err(|e| format!("ffmpeg write frame {}: {e}", self.frames_written))?;
            }
        }
        self.frames_written += 1;
        Ok(())
    }

    /// Flush & finalize the recording (including fast `-c:v copy` audio mux when enabled).
    pub fn finish(mut self) -> Result<CaptureSummary, String> {
        let format = self.format();
        let has_audio = self.has_audio();
        let out_path = self.out_path.clone();
        let frames = self.frames_written;
        let ticks = self.ticks_seen;
        let width = self.width;
        let height = self.height;
        let fps = self.fps;
        let stride = self.stride;

        if let CaptureSink::Ffmpeg {
            child,
            stdin,
            audio,
            ..
        } = &mut self.sink
        {
            // Drop stdin first so ffmpeg receives EOF and flushes the container.
            drop(stdin.take());
            if let Some(c) = child.take() {
                let out = c
                    .wait_with_output()
                    .map_err(|e| format!("wait for ffmpeg: {e}"))?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(format!(
                        "ffmpeg exited with {}: {}",
                        out.status,
                        stderr.trim()
                    ));
                }
            }
            if let Some(mut aud) = audio.take() {
                if let Some(mut w) = aud.pcm_writer.take() {
                    w.flush()
                        .map_err(|e| format!("flush temp audio PCM: {e}"))?;
                }
                let mux_res = mux_video_and_pcm(
                    &aud.tmp_video_path,
                    &aud.tmp_audio_path,
                    &out_path,
                    format,
                );
                let _ = fs::remove_file(&aud.tmp_video_path);
                let _ = fs::remove_file(&aud.tmp_audio_path);
                mux_res?;
            }
        }
        Ok(CaptureSummary {
            path: out_path,
            format,
            frames,
            ticks,
            width,
            height,
            fps,
            stride,
            has_audio,
        })
    }
}

impl Drop for FrameRecorder {
    fn drop(&mut self) {
        if let CaptureSink::Ffmpeg {
            child,
            stdin,
            audio,
            ..
        } = &mut self.sink
        {
            drop(stdin.take());
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            if let Some(mut aud) = audio.take() {
                drop(aud.pcm_writer.take());
                let _ = fs::remove_file(&aud.tmp_video_path);
                let _ = fs::remove_file(&aud.tmp_audio_path);
            }
        }
    }
}

fn mux_video_and_pcm(
    tmp_video: &Path,
    tmp_audio: &Path,
    out_path: &Path,
    format: &str,
) -> Result<(), String> {
    let sr = OFFLINE_SAMPLE_RATE.to_string();
    let mut cmd = ProcCommand::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-i"])
        .arg(tmp_video)
        .args(["-f", "f32le", "-ar", &sr, "-ac", "2", "-i"])
        .arg(tmp_audio)
        .args(["-c:v", "copy"]);
    match format {
        "webm" => {
            cmd.args(["-c:a", "libopus", "-b:a", "160k"]);
        }
        "mp4" | "mov" => {
            cmd.args(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]);
        }
        _ => {
            cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        }
    }
    cmd.args(["-shortest"])
        .arg(out_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let out = cmd
        .output()
        .map_err(|e| format!("failed to spawn ffmpeg audio mux: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "ffmpeg audio mux exited with {}: {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Options for deterministic `.repal` → MP4/GIF/PNG-sequence video export (`--replay-export-video`).
#[derive(Clone, Debug)]
pub struct VideoExportOpts {
    pub hd_mode: bool,
    pub stride: u32,
    pub with_thought: bool,
    pub with_audio: bool,
    pub start_tick: u64,
    pub end_tick: Option<u64>,
}

impl Default for VideoExportOpts {
    fn default() -> Self {
        Self {
            hd_mode: true,
            stride: 2, // 25 fps default (fast & crisp)
            with_thought: false,
            with_audio: true,
            start_tick: 0,
            end_tick: None,
        }
    }
}

/// Deterministically play a `.repal` archive through `HeadlessApp` and export frames + stereo audio
/// to video/PNG dir.
pub fn export_repal_video(
    app: &mut HeadlessApp,
    repal_path: &Path,
    out_path: &Path,
    opts: &VideoExportOpts,
) -> Result<CaptureSummary, String> {
    let bundle = read_repal_file(repal_path)?;
    let mut player = ReplayPlayer::new(bundle)?;
    app.ensure_chapters_loaded()?;
    app.set_hd_mode(opts.hd_mode);
    app.rt.set_in_title(false);
    let _ = app.drain_audio_events();
    if !player.load_segment(0, &mut app.rt) {
        return Err("failed to load initial segment of .repal archive".to_string());
    }
    app.pump_host_cmds();
    app.render_frame();

    let (w, h) = (app.rt.compositor.fb.width, app.rt.compositor.fb.height);
    let mut rec = FrameRecorder::open_with_audio(
        out_path,
        w,
        h,
        opts.stride.max(1),
        opts.with_thought,
        opts.with_audio,
        opts.hd_mode,
    )?;

    let mut global_tick: u64 = 0;
    let max_ticks = opts.end_tick.unwrap_or(u64::MAX);

    loop {
        let audio_events = app.drain_audio_events();
        let cur_hd = app.rt.ui.hd_mode;
        if global_tick >= opts.start_tick && global_tick <= max_ticks {
            app.render_frame();
            if opts.with_thought && app.rt.has_visible_ai_thought_overlay() {
                let mut fb_copy = app.rt.compositor.fb.clone();
                app.rt.draw_ai_thought_overlay_onto(&mut fb_copy);
                let vfs = &mut app.vfs;
                rec.push_tick_with_audio(&fb_copy, &audio_events, cur_hd, |k| {
                    vfs.read_bytes(k).ok()
                })?;
            } else {
                let fb = &app.rt.compositor.fb;
                let vfs = &mut app.vfs;
                rec.push_tick_with_audio(fb, &audio_events, cur_hd, |k| {
                    vfs.read_bytes(k).ok()
                })?;
            }
        } else if global_tick < opts.start_tick && !audio_events.is_empty() {
            let vfs = &mut app.vfs;
            rec.pre_roll_audio(&audio_events, cur_hd, |k| vfs.read_bytes(k).ok());
        }
        if global_tick >= max_ticks {
            break;
        }
        match player.step_one_tick(&mut app.rt, |_, _| {}, |_, _| {}) {
            ReplayStepOutcome::Stepped => {
                app.pump_host_cmds();
                global_tick += 1;
            }
            ReplayStepOutcome::SegmentCompleted { next_idx } => {
                if !player.load_segment(next_idx, &mut app.rt) {
                    break;
                }
                app.pump_host_cmds();
                global_tick += 1;
            }
            ReplayStepOutcome::Finished => break,
        }
    }

    rec.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pal_render::framebuffer::Color;

    #[test]
    fn frame_recorder_png_dir_respects_stride_and_writes_frames() {
        let dir = std::env::temp_dir().join(format!(
            "repal_capture_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let mut rec = FrameRecorder::open(&dir, 16, 16, 2, false).expect("open png dir");
        let mut fb = Framebuffer::new(16, 16);
        fb.clear(Color::rgba(10, 20, 30, 255));
        for _ in 0..5 {
            rec.push_tick(&fb).unwrap();
        }
        let summary = rec.finish().unwrap();
        assert_eq!(summary.format, "png_dir");
        assert!(!summary.has_audio);
        assert_eq!(summary.ticks, 5);
        assert_eq!(summary.frames, 3); // ticks 0, 2, 4
        assert!(dir.join("frame_00000.png").is_file());
        assert!(dir.join("frame_00002.png").is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
