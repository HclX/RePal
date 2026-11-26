//! Background asset-package downloads with progress (#18): a blocking chapter download behind a
//! loading overlay on scene entry, and the optional "download all assets" job.
//!
//! Downloads never run on the winit event-loop thread, so the window keeps rendering. While a
//! chapter download is pending the host simply withholds `HostReply::SceneLoaded`; the core stays
//! blocked on the host (`tick_count` does not advance), which keeps live play and `.repal`
//! playback deterministic.

use crate::pack_fetcher::PackFetcher;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

enum DownloadMsg {
    Progress(u64),
    Finished(Result<(), String>),
}

/// One background job downloading an ordered list of packs.
pub(crate) struct PackDownloadJob {
    rx: Receiver<DownloadMsg>,
    pub(crate) total_bytes: u64,
    pub(crate) done_bytes: u64,
    pub(crate) started: Instant,
    result: Option<Result<(), String>>,
}

impl PackDownloadJob {
    pub(crate) fn spawn(fetcher: &PackFetcher, pack_ids: Vec<String>, name: &str) -> Self {
        let total_bytes = pack_ids.iter().map(|p| fetcher.pack_size(p)).sum();
        let (tx, rx) = channel();
        let fetcher = fetcher.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("repal-dl-{name}"))
            .spawn({
                let tx = tx.clone();
                move || {
                    let mut base = 0u64;
                    for pid in pack_ids {
                        let size = fetcher.pack_size(&pid);
                        let res = fetcher.ensure_pack_with_progress(&pid, &mut |b| {
                            let cur = if size > 0 { b.min(size) } else { b };
                            let _ = tx.send(DownloadMsg::Progress(base + cur));
                        });
                        if let Err(e) = res {
                            let _ = tx.send(DownloadMsg::Finished(Err(e)));
                            return;
                        }
                        base += size;
                        let _ = tx.send(DownloadMsg::Progress(base));
                    }
                    let _ = tx.send(DownloadMsg::Finished(Ok(())));
                }
            });
        if let Err(e) = spawned {
            let _ = tx.send(DownloadMsg::Finished(Err(format!(
                "spawn download thread: {e}"
            ))));
        }
        Self {
            rx,
            total_bytes,
            done_bytes: 0,
            started: Instant::now(),
            result: None,
        }
    }

    /// Drain progress messages; returns `Some(result)` once (when the job finishes).
    pub(crate) fn poll(&mut self) -> Option<Result<(), String>> {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                DownloadMsg::Progress(b) => self.done_bytes = b,
                DownloadMsg::Finished(r) => self.result = Some(r),
            }
        }
        self.result.take()
    }

    pub(crate) fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// A scene whose chapter package is being downloaded; `SceneLoaded` is withheld until done.
pub(crate) struct PendingSceneLoad {
    pub(crate) scene_id: u16,
    pub(crate) map_id: u16,
    pub(crate) job: PackDownloadJob,
    pub(crate) error: Option<String>,
    pub(crate) retry_at: Option<Instant>,
}

/// Delay before showing the loading overlay so cached/instant transitions never flash.
pub(crate) const LOADING_OVERLAY_DELAY: Duration = Duration::from_millis(300);
/// Automatic retry delay after a failed chapter download.
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(3);

pub(crate) fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Text content of the chapter-download loading overlay.
pub(crate) struct LoadingView {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// `Some(0.0..=1.0)` draws a progress bar; `None` while showing an error.
    pub(crate) fraction: Option<f32>,
}

/// Loading-overlay content for a pending chapter download.
pub(crate) fn loading_view_for(p: &PendingSceneLoad, lang: pal_core::Language) -> LoadingView {
    loading_view(
        p.job.done_bytes,
        p.job.total_bytes,
        p.error.as_deref(),
        lang,
    )
}

/// Loading-overlay content from raw progress (`error` takes precedence while auto-retrying).
pub(crate) fn loading_view(
    done_bytes: u64,
    total_bytes: u64,
    error: Option<&str>,
    lang: pal_core::Language,
) -> LoadingView {
    use pal_core::Language;
    if let Some(err) = error {
        let title = match lang {
            Language::En => "Download failed — retrying…",
            Language::ZhHant => "下載失敗，正在重試…",
            Language::ZhHans => "下载失败，正在重试…",
        };
        return LoadingView {
            title: title.to_string(),
            detail: err.lines().next().unwrap_or("").trim().to_string(),
            fraction: None,
        };
    }
    let f = if total_bytes == 0 {
        0.0
    } else {
        (done_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0) as f32
    };
    let title = match lang {
        Language::En => "Downloading chapter assets",
        Language::ZhHant => "正在下載章節資源",
        Language::ZhHans => "正在下载章节资源",
    };
    LoadingView {
        title: title.to_string(),
        detail: format!(
            "{:.1} / {:.1} MB · {}%",
            mb(done_bytes),
            mb(total_bytes),
            (f * 100.0).round() as u32
        ),
        fraction: Some(f),
    }
}

/// Draw the loading overlay (full-screen dim, centered panel with title, progress bar and a
/// one-line detail truncated to fit) onto `rt.compositor.fb`.
pub(crate) fn render_loading_overlay(rt: &mut pal_runtime::PalRuntime, view: &LoadingView) {
    use pal_render::framebuffer::Color;
    let s = if rt.ui.hd_mode { 4 } else { 1 };
    let fonts = &mut rt.compositor.fonts;
    let fb = &mut rt.compositor.fb;
    let (w, h) = (fb.width as i32, fb.height as i32);
    let (title_px, detail_px) = ((11 * s) as u16, (9 * s) as u16);
    fb.fill_rect(0, 0, w, h, Color::rgba(6, 10, 16, 205));

    let max_bw = 280 * s;
    let pad = 14 * s;
    let content_w = fonts
        .measure_text(&view.title, title_px)
        .max(fonts.measure_text(&view.detail, detail_px))
        .max(150 * s);
    let bw = (content_w + pad * 2).min(max_bw);
    let bh = 58 * s;
    let (bx, by) = ((w - bw) / 2, (h - bh) / 2);
    fb.fill_rect(bx, by, bw, bh, Color::rgba(14, 22, 32, 235));
    fb.stroke_rect(bx, by, bw, bh, s.max(1), Color::rgba(216, 178, 92, 230));

    fonts.draw_text_centered(
        fb,
        w / 2,
        by + 9 * s,
        &view.title,
        title_px,
        Color::GOLD_HI,
        Some(Color::BLACK),
    );

    // Progress bar (or a muted divider while showing an error).
    let (px, py, pw, ph) = (bx + pad, by + 27 * s, bw - pad * 2, 5 * s);
    fb.fill_rect(px, py, pw, ph, Color::rgba(40, 48, 60, 255));
    if let Some(f) = view.fraction {
        let fill = ((pw as f32) * f).round() as i32;
        if fill > 0 {
            fb.fill_rect(px, py, fill, ph, Color::rgba(216, 178, 92, 255));
        }
    }

    // Detail line, truncated with an ellipsis to the panel width.
    let mut detail = view.detail.clone();
    let max_dw = bw - pad * 2;
    if fonts.measure_text(&detail, detail_px) > max_dw {
        let mut chars: Vec<char> = detail.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let cand: String = chars.iter().collect::<String>() + "…";
            if fonts.measure_text(&cand, detail_px) <= max_dw {
                detail = cand;
                break;
            }
        }
    }
    let detail_color = if view.fraction.is_some() {
        Color::PAPER_TEXT
    } else {
        Color::rgba(232, 140, 120, 255)
    };
    fonts.draw_text_centered(
        fb,
        w / 2,
        by + 38 * s,
        &detail,
        detail_px,
        detail_color,
        Some(Color::BLACK),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use pal_runtime::crypto::sha256_hex;
    use std::fs;

    #[test]
    fn chapter_job_downloads_missing_packs_with_progress_and_skips_rehash() {
        let root = std::env::temp_dir().join(format!("repal_dl_job_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (src, cache) = (root.join("src"), root.join("cache"));
        fs::create_dir_all(&src).unwrap();
        let base = vec![0xB5u8; 40_000];
        let ch = vec![0xC3u8; 25_000];
        fs::write(src.join("base.rpk"), &base).unwrap();
        fs::write(src.join("ch01.rpk"), &ch).unwrap();
        let manifest = serde_json::json!({
            "format": "repal-packs/1",
            "schema_version": 1,
            "version": "dl-test",
            "boot": ["base"],
            "shared": [],
            "scenes": {"1": ["ch01"]},
            "packs": {
                "base": {"file": "base.rpk", "size": base.len(), "sha256": sha256_hex(&base), "urls": ["base.rpk"]},
                "ch01": {"file": "ch01.rpk", "size": ch.len(), "sha256": sha256_hex(&ch), "urls": ["ch01.rpk"]}
            }
        });
        let man = src.join("pack-manifest.json");
        fs::write(&man, manifest.to_string()).unwrap();
        let fetcher =
            PackFetcher::fetch_manifest(cache.clone(), format!("file://{}", man.display()), None)
                .unwrap();

        // Scene 1 needs base + ch01; nothing is cached yet.
        let missing = fetcher.missing_scene_packs(1, None);
        assert_eq!(missing, vec!["base".to_string(), "ch01".to_string()]);
        let mut job = PackDownloadJob::spawn(&fetcher, missing, "test");
        assert_eq!(job.total_bytes, (base.len() + ch.len()) as u64);
        let deadline = Instant::now() + Duration::from_secs(10);
        let res = loop {
            if let Some(r) = job.poll() {
                break r;
            }
            assert!(Instant::now() < deadline, "download job timed out");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(job.done_bytes, job.total_bytes);
        assert!((job.fraction() - 1.0).abs() < f32::EPSILON);
        assert!(fetcher.missing_scene_packs(1, None).is_empty());
        assert!(fetcher.missing_packs().is_empty());

        // Verified once this session: corrupting the cached bytes (same size) is not re-hashed on
        // the fast path, proving scene changes no longer re-hash every cached pack.
        fs::write(cache.join("ch01.rpk"), vec![0u8; ch.len()]).unwrap();
        assert!(fetcher.ensure_pack("ch01").is_ok());
        // A fresh session (new fetcher) does verify, detects the corruption and re-downloads.
        let fresh =
            PackFetcher::fetch_manifest(cache.clone(), format!("file://{}", man.display()), None)
                .unwrap();
        let p = fresh.ensure_pack("ch01").unwrap();
        assert_eq!(fs::read(p).unwrap(), ch);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn labels_cover_all_states() {
        use pal_core::Language;
        assert!(pal_ui::download_all_text(Language::ZhHans, 0, None).contains('✓'));
        assert!(
            pal_ui::download_all_text(Language::En, 300 * 1024 * 1024, None).contains("300 MB")
        );
        let v = loading_view(5 * 1024 * 1024, 10 * 1024 * 1024, None, Language::ZhHans);
        assert_eq!(v.fraction, Some(0.5));
        assert!(v.detail.contains("50%"));
        assert!(loading_view(0, 1, Some("boom"), Language::En)
            .fraction
            .is_none());
    }
}
