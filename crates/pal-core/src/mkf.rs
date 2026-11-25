//! MKF (32-bit offset table) and SMKF (16-bit word-offset table) container parsers.

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> Option<u16> {
    let bytes = data.get(off..off + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let bytes = data.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Number of chunks stored in a standard `.MKF` archive.
pub fn mkf_chunk_count(data: &[u8]) -> usize {
    let Some(first_off) = read_u32_le(data, 0) else {
        return 0;
    };
    let entries = (first_off as usize) / 4;
    entries.saturating_sub(1)
}

/// Borrow raw bytes of `idx`-th chunk in a `.MKF` archive (may be empty `&[]`).
pub fn mkf_chunk(data: &[u8], idx: usize) -> Option<&[u8]> {
    let count = mkf_chunk_count(data);
    if idx >= count {
        return None;
    }
    let o1 = read_u32_le(data, idx * 4)? as usize;
    let o2 = read_u32_le(data, (idx + 1) * 4)? as usize;
    if o2 < o1 || o2 > data.len() {
        return Some(&[]);
    }
    Some(&data[o1..o2])
}

/// Number of sprite frames inside a `SMKF` sub-archive (`GOP`, decompressed `MGO`/`ABC`/`F`/`FIRE`).
pub fn smkf_frame_count(data: &[u8]) -> usize {
    let Some(w0) = read_u16_le(data, 0) else {
        return 0;
    };
    if w0 == 0 {
        return 0;
    }
    let table_bytes = (w0 as usize) * 2;
    if table_bytes > data.len() {
        return 0;
    }
    // Check if the trailing entry is a sentinel (either end offset or 0);
    // in DOS PAL SMKF, if the last entry is 0 or points to end of buffer, frame count is w0 - 1,
    // EXCEPT in ABC/F/FIRE where w0 itself can be frame count if w0*2 == first frame offset!
    // Notice: first frame ALWAYS starts at byte offset `w0 * 2`!
    // Wait: if `data[0..2]` is `w0`, then `tbl[0]` IS `w0`! So `tbl[0] * 2 == w0 * 2` is the byte offset of frame 0!
    // Let's check whether the last entry `tbl[w0 - 1]` is an end-sentinel (0 or >= len/2) or a real frame start:
    let last_w = read_u16_le(data, (w0 as usize - 1) * 2).unwrap_or(0) as usize;
    if w0 >= 2 && (last_w == 0 || last_w * 2 >= data.len()) {
        (w0 as usize) - 1
    } else {
        w0 as usize
    }
}

/// Borrow raw RLE bytes of `idx`-th frame in a `SMKF` sub-archive.
pub fn smkf_frame(data: &[u8], idx: usize) -> Option<&[u8]> {
    let w0 = read_u16_le(data, 0)? as usize;
    let count = smkf_frame_count(data);
    if idx >= count || w0 == 0 {
        return None;
    }
    let start = (read_u16_le(data, idx * 2)? as usize) * 2;
    if start == 0 || start >= data.len() {
        return Some(&[]);
    }
    let end = if idx + 1 < w0 {
        let next = (read_u16_le(data, (idx + 1) * 2)? as usize) * 2;
        if next == 0 || next > data.len() {
            data.len()
        } else {
            next
        }
    } else {
        data.len()
    };
    if end < start {
        return Some(&[]);
    }
    Some(&data[start..end])
}
