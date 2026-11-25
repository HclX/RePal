//! Multi-frame burst capture & deterministic `.repal` → MP4/GIF/PNG-sequence video export.
//!
//! Zero extra Rust codec dependencies:
//! - Video/GIF targets (`.mp4`, `.mkv`, `.webm`, `.mov`, `.gif`) stream raw RGBA framebuffers
//!   directly into `ffmpeg -f rawvideo` via `stdin` (zero temporary PNG disk I/O).
//! - Directory targets write numbered PNG frames (`frame_00000.png`, ...), working even without
//!   `ffmpeg` installed.

use crate::HeadlessApp;
use pal_render::framebuffer::Framebuffer;
use pal_runtime::repal_archive::read_repal_file;
use pal_runtime::replay_player::{ReplayPlayer, ReplayStepOutcome};
use std::fs;
use std::io::Write;
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
}

enum CaptureSink {
    PngDir {
        dir: PathBuf,
    },
    Ffmpeg {
        child: Child,
        stdin: Option<ChildStdin>,
        format: &'static str,
    },
}

/// Stateful per-tick framebuffer recorder shared by Text Mode `capture` and `--replay-export-video`.
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

impl FrameRecorder {
    /// Open a recorder targeting either a video/GIF file (via `ffmpeg`) or a PNG sequence directory.
    pub fn open(
        out_path: impl Into<PathBuf>,
        width: u32,
        height: u32,
        stride: u32,
        with_thought: bool,
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
            cmd.arg(&out_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());

            let mut child = cmd.spawn().map_err(|e| {
                format!(
                    "failed to spawn ffmpeg for {} ({e}); install ffmpeg or pass a directory path for PNG frames",
                    out_path.display()
                )
            })?;
            let stdin = child.stdin.take();
            CaptureSink::Ffmpeg {
                child,
                stdin,
                format: fmt,
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

    /// Record one simulation tick (writing a frame when `ticks_seen % stride == 0`).
    pub fn push_tick(&mut self, fb: &Framebuffer) -> Result<(), String> {
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

    /// Flush & finalize the recording.
    pub fn finish(self) -> Result<CaptureSummary, String> {
        let format = self.format();
        let out_path = self.out_path;
        let frames = self.frames_written;
        let ticks = self.ticks_seen;
        let width = self.width;
        let height = self.height;
        let fps = self.fps;
        let stride = self.stride;
        match self.sink {
            CaptureSink::PngDir { .. } => {}
            CaptureSink::Ffmpeg { child, mut stdin, .. } => {
                // Drop stdin first so ffmpeg receives EOF and flushes the container.
                drop(stdin.take());
                let out = child
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
        })
    }
}

/// Options for deterministic `.repal` → MP4/GIF/PNG-sequence video export (`--replay-export-video`).
#[derive(Clone, Debug)]
pub struct VideoExportOpts {
    pub hd_mode: bool,
    pub stride: u32,
    pub with_thought: bool,
    pub start_tick: u64,
    pub end_tick: Option<u64>,
}

impl Default for VideoExportOpts {
    fn default() -> Self {
        Self {
            hd_mode: true,
            stride: 2, // 25 fps default (fast & crisp)
            with_thought: false,
            start_tick: 0,
            end_tick: None,
        }
    }
}

/// Deterministically play a `.repal` archive through `HeadlessApp` and export frames to video/PNG dir.
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
    if !player.load_segment(0, &mut app.rt) {
        return Err("failed to load initial segment of .repal archive".to_string());
    }
    app.pump_host_cmds();
    app.render_frame();

    let (w, h) = (app.rt.compositor.fb.width, app.rt.compositor.fb.height);
    let mut rec = FrameRecorder::open(out_path, w, h, opts.stride.max(1), opts.with_thought)?;

    let mut global_tick: u64 = 0;
    let max_ticks = opts.end_tick.unwrap_or(u64::MAX);

    loop {
        if global_tick >= opts.start_tick && global_tick <= max_ticks {
            app.render_frame();
            if opts.with_thought && app.rt.has_visible_ai_thought_overlay() {
                let mut fb_copy = app.rt.compositor.fb.clone();
                app.rt.draw_ai_thought_overlay_onto(&mut fb_copy);
                rec.push_tick(&fb_copy)?;
            } else {
                rec.push_tick(&app.rt.compositor.fb)?;
            }
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
        assert_eq!(summary.ticks, 5);
        assert_eq!(summary.frames, 3); // ticks 0, 2, 4
        assert!(dir.join("frame_00000.png").is_file());
        assert!(dir.join("frame_00002.png").is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
