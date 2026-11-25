//! Zero-dependency native splash screen rendered directly into `softbuffer`'s `0x00RRGGBB`
//! framebuffer while RePal downloads, verifies, and extracts the pinned asset pack on first run.

pub struct SplashState {
    pub progress: f32,
    pub stage: String,
    pub detail: String,
    pub error: Option<String>,
    pub tick: u32,
}

impl Default for SplashState {
    fn default() -> Self {
        Self {
            progress: 0.0,
            stage: "PREPARING ASSETS...".to_string(),
            detail: "CHECKING LOCAL CACHE".to_string(),
            error: None,
            tick: 0,
        }
    }
}

/// Render the RePal asset-download splash screen directly into a `0x00RRGGBB` pixel buffer.
pub fn render_splash(buf: &mut [u32], width: usize, height: usize, state: &SplashState) {
    if width == 0 || height == 0 || buf.len() < width.saturating_mul(height) {
        return;
    }

    // 1. Vertical dark indigo-slate gradient background.
    for y in 0..height {
        let t = y as f32 / height.max(1) as f32;
        let r = (12.0 + t * 14.0) as u32;
        let g = (20.0 + t * 22.0) as u32;
        let b = (34.0 + t * 28.0) as u32;
        let px = (r << 16) | (g << 8) | b;
        let row = y * width;
        buf[row..row + width].fill(px);
    }

    // 2. Centered decorative gold frame.
    let margin_x = (width / 10).max(24);
    let margin_y = (height / 10).max(24);
    if width > margin_x * 2 + 8 && height > margin_y * 2 + 8 {
        draw_rect_outline(
            buf,
            width,
            height,
            margin_x,
            margin_y,
            width - margin_x * 2,
            height - margin_y * 2,
            2,
            0x00C8A458,
        );
        draw_rect_outline(
            buf,
            width,
            height,
            margin_x + 6,
            margin_y + 6,
            width - (margin_x + 6) * 2,
            height - (margin_y + 6) * 2,
            1,
            0x004E677D,
        );
    }

    let cx = width / 2;
    let cy = height / 2;

    // 3. Title "REPAL" + Subtitle.
    let title_scale = (width / 150).clamp(4, 10);
    let title_y = cy.saturating_sub(title_scale * 16);
    draw_text_centered(
        buf,
        width,
        height,
        "REPAL",
        cx + title_scale / 2,
        title_y + title_scale / 2,
        title_scale,
        0x00080D14,
    );
    draw_text_centered(
        buf,
        width,
        height,
        "REPAL",
        cx,
        title_y,
        title_scale,
        0x00F2D184,
    );

    let sub_scale = (width / 420).clamp(2, 4);
    let sub_y = title_y + title_scale * 9;
    draw_text_centered(
        buf,
        width,
        height,
        "LEGEND OF SWORD AND FAIRY - HD",
        cx,
        sub_y,
        sub_scale,
        0x009BB7CC,
    );

    if let Some(err) = &state.error {
        // Error card
        let err_scale = (width / 520).clamp(2, 3);
        let box_w = (width * 3 / 4).min(960);
        let box_x = cx.saturating_sub(box_w / 2);
        let box_y = cy.saturating_sub(20);
        let box_h = (height / 3).max(120);
        fill_rect(buf, width, height, box_x, box_y, box_w, box_h, 0x002B1418);
        draw_rect_outline(buf, width, height, box_x, box_y, box_w, box_h, 2, 0x00E06464);

        draw_text_centered(
            buf,
            width,
            height,
            "ASSET DOWNLOAD FAILED",
            cx,
            box_y + 18,
            err_scale,
            0x00FF9B9B,
        );

        let max_chars = ((box_w.saturating_sub(40)) / (err_scale * 6)).max(24);
        let mut line_y = box_y + 18 + err_scale * 12;
        for raw_line in err.lines() {
            for wrapped in wrap_text(raw_line, max_chars) {
                if line_y + err_scale * 8 >= box_y + box_h.saturating_sub(32) {
                    break;
                }
                draw_text_centered(
                    buf,
                    width,
                    height,
                    &wrapped,
                    cx,
                    line_y,
                    err_scale,
                    0x00F2E6E6,
                );
                line_y += err_scale * 9;
            }
        }
        draw_text_centered(
            buf,
            width,
            height,
            "PRESS ESC TO EXIT",
            cx,
            box_y + box_h.saturating_sub(err_scale * 10),
            err_scale,
            0x00D8B25C,
        );
        return;
    }

    // 4. Progress bar.
    let bar_w = (width * 3 / 5).clamp(240, 760);
    let bar_h = (height / 28).clamp(18, 30);
    let bar_x = cx.saturating_sub(bar_w / 2);
    let bar_y = cy + (height / 14);

    let stage_scale = (width / 460).clamp(2, 3);
    draw_text_centered(
        buf,
        width,
        height,
        &state.stage,
        cx,
        bar_y.saturating_sub(stage_scale * 11),
        stage_scale,
        0x00E8EEF4,
    );

    // Track background & border
    fill_rect(buf, width, height, bar_x, bar_y, bar_w, bar_h, 0x000A111A);
    draw_rect_outline(buf, width, height, bar_x, bar_y, bar_w, bar_h, 2, 0x00C8A458);

    let pct = state.progress.clamp(0.0, 1.0);
    let inner_w = bar_w.saturating_sub(6);
    let inner_h = bar_h.saturating_sub(6);
    let fill_w = ((inner_w as f32) * pct).round() as usize;
    if fill_w > 0 && inner_h > 0 {
        let shimmer_x = ((state.tick as usize * 6) % (inner_w + 80)).saturating_sub(40);
        for dy in 0..inner_h {
            let py = bar_y + 3 + dy;
            if py >= height {
                break;
            }
            let v_shade = if dy < inner_h / 2 { 18i32 } else { -12i32 };
            let row = py * width;
            for dx in 0..fill_w {
                let px_x = bar_x + 3 + dx;
                if px_x >= width {
                    break;
                }
                let dist = dx.abs_diff(shimmer_x);
                let glow = if dist < 24 { ((24 - dist) * 2) as i32 } else { 0 };
                let r = (212 + v_shade + glow).clamp(0, 255) as u32;
                let g = (168 + v_shade + glow).clamp(0, 255) as u32;
                let b = (76 + v_shade / 2 + glow / 2).clamp(0, 255) as u32;
                buf[row + px_x] = (r << 16) | (g << 8) | b;
            }
        }
    }

    // Percentage + detail below bar
    let pct_str = format!("{:3.0}%", pct * 100.0);
    draw_text_centered(
        buf,
        width,
        height,
        &pct_str,
        cx,
        bar_y + bar_h + stage_scale * 6,
        stage_scale,
        0x00F2D184,
    );

    let detail_scale = (width / 560).clamp(1, 3);
    draw_text_centered(
        buf,
        width,
        height,
        &state.detail,
        cx,
        bar_y + bar_h + stage_scale * 16,
        detail_scale,
        0x008FA8BD,
    );

    draw_text_centered(
        buf,
        width,
        height,
        "FIRST-TIME SETUP: ASSETS ARE CACHED LOCALLY FOR FUTURE LAUNCHES",
        cx,
        height.saturating_sub(margin_y + detail_scale * 14),
        detail_scale,
        0x00637A8F,
    );
}

fn wrap_text(s: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + word.len() > max_chars {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

pub(crate) fn fill_rect(
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    rw: usize,
    rh: usize,
    color: u32,
) {
    let y1 = (y + rh).min(h);
    let x1 = (x + rw).min(w);
    for py in y..y1 {
        let row = py * w;
        buf[row + x..row + x1].fill(color);
    }
}

pub(crate) fn fill_rect_alpha(
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    rw: usize,
    rh: usize,
    color: u32,
    alpha: u32,
) {
    let y1 = (y + rh).min(h);
    let x1 = (x + rw).min(w);
    let sr = (color >> 16) & 0xFF;
    let sg = (color >> 8) & 0xFF;
    let sb = color & 0xFF;
    let inv = 255 - alpha;
    for py in y..y1 {
        let row = py * w;
        for px in x..x1 {
            let dst = buf[row + px];
            let dr = (dst >> 16) & 0xFF;
            let dg = (dst >> 8) & 0xFF;
            let db = dst & 0xFF;
            let r = (sr * alpha + dr * inv) / 255;
            let g = (sg * alpha + dg * inv) / 255;
            let b = (sb * alpha + db * inv) / 255;
            buf[row + px] = (r << 16) | (g << 8) | b;
        }
    }
}

pub(crate) fn draw_rect_outline(
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    rw: usize,
    rh: usize,
    t: usize,
    color: u32,
) {
    fill_rect(buf, w, h, x, y, rw, t, color);
    fill_rect(buf, w, h, x, y + rh.saturating_sub(t), rw, t, color);
    fill_rect(buf, w, h, x, y, t, rh, color);
    fill_rect(buf, w, h, x + rw.saturating_sub(t), y, t, rh, color);
}

pub(crate) fn draw_text_at(
    buf: &mut [u32],
    w: usize,
    h: usize,
    text: &str,
    x: usize,
    y: usize,
    scale: usize,
    color: u32,
) {
    for (i, ch) in text.chars().enumerate() {
        let gx = x + i * 6 * scale;
        draw_glyph(buf, w, h, ch.to_ascii_uppercase(), gx, y, scale, color);
    }
}

fn draw_text_centered(
    buf: &mut [u32],
    w: usize,
    h: usize,
    text: &str,
    cx: usize,
    y: usize,
    scale: usize,
    color: u32,
) {
    let count = text.chars().count();
    if count == 0 {
        return;
    }
    let total_w = count * 6 * scale - scale;
    let start_x = cx.saturating_sub(total_w / 2);
    for (i, ch) in text.chars().enumerate() {
        let gx = start_x + i * 6 * scale;
        draw_glyph(buf, w, h, ch.to_ascii_uppercase(), gx, y, scale, color);
    }
}

fn draw_glyph(
    buf: &mut [u32],
    w: usize,
    h: usize,
    ch: char,
    x: usize,
    y: usize,
    scale: usize,
    color: u32,
) {
    let rows = glyph_5x7(ch);
    for (ry, &bits) in rows.iter().enumerate() {
        for rx in 0..5 {
            if (bits >> (4 - rx)) & 1 != 0 {
                fill_rect(
                    buf,
                    w,
                    h,
                    x + rx * scale,
                    y + ry * scale,
                    scale,
                    scale,
                    color,
                );
            }
        }
    }
}

fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x0E, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x01, 0x0E],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '%' => [0x19, 0x19, 0x02, 0x04, 0x08, 0x13, 0x13],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0E, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '#' => [0x0A, 0x1F, 0x0A, 0x0A, 0x1F, 0x0A, 0x00],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '"' => [0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}
