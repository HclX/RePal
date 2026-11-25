//! Extensible `RPK1` (RePal Pack v1) zero-extraction random-access asset container (#237).
//!
//! ## Forward & Backward Compatibility Design
//! - **32-byte little-endian header** carrying `format_major`, `format_minor`, `header_len`, and
//!   `entry_stride`:
//!   - Older v1 readers accept any `format_major == 1` with any `format_minor >= 0`, skipping
//!     additive header extensions (`toc_offset >= header_len >= 32`), additive fixed TOC fields
//!     (`entry_stride >= 32`), and per-entry TLV extension bytes (`ext_len`).
//!   - Feature flags split into **must-understand** bits (`0..16`: unknown set bit -> clean error)
//!     and **advisory/optional** bits (`16..32`: unknown set bit -> safely ignored).
//! - **O(entry) random access**: Both plain and AES-256-GCM encrypted packs decrypt/inflate only
//!   the single requested entry (never the whole pack).
//! - **Defense-in-depth guards (#210)**: Strict caps on entry count, TOC size, decompressed entry
//!   size, normalized relative keys (no `..`, no leading `/`, no `\`), and overflow-checked offsets.

use crate::crypto::{
    aes256_gcm_decrypt, aes256_gcm_encrypt, crc32_ieee, derive_rpk_nonce, sha256_hex,
};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

pub const RPK_MAGIC: &[u8; 4] = b"RPK1";
pub const RPK_FORMAT_MAJOR: u16 = 1;
pub const RPK_FORMAT_MINOR: u16 = 0;
pub const RPK_MIN_HEADER_LEN: u16 = 32;
pub const RPK_MIN_ENTRY_STRIDE: u16 = 32;

/// Header flag: TOC payload is raw-deflate compressed (RFC 1951).
pub const RPK_FLAG_TOC_DEFLATED: u32 = 1 << 0;
/// Header flag: entry payloads are per-entry AES-256-GCM encrypted.
pub const RPK_FLAG_ENCRYPTED: u32 = 1 << 1;
/// Header flag: TOC payload itself is AES-256-GCM encrypted (nonce index `u32::MAX`).
pub const RPK_FLAG_TOC_ENCRYPTED: u32 = 1 << 2;

/// Must-understand flag mask (bits 0..16). Bits 16..32 are advisory and ignored by older readers.
const RPK_MUST_UNDERSTAND_MASK: u32 = 0x0000_FFFF;
const RPK_KNOWN_MUST_UNDERSTAND_FLAGS: u32 =
    RPK_FLAG_TOC_DEFLATED | RPK_FLAG_ENCRYPTED | RPK_FLAG_TOC_ENCRYPTED;

/// Compression enum stored per TOC entry.
pub const RPK_COMP_RAW: u8 = 0;
pub const RPK_COMP_DEFLATE: u8 = 1;

/// Safety bounds matching `#210` decompression-bomb guards.
pub const MAX_RPK_ENTRIES: u32 = 65_536;
pub const MAX_RPK_TOC_STORED: u32 = 16 * 1024 * 1024;
pub const MAX_RPK_TOC_RAW: usize = 32 * 1024 * 1024;
pub const MAX_RPK_ENTRY_RAW: u32 = 64 * 1024 * 1024;
pub const MAX_RPK_KEY_LEN: usize = 512;

/// Validate and normalize an RPK asset key (forward slashes, relative, no `..` / `.` / `\`).
pub fn normalize_rpk_key(raw: &str) -> Result<String, String> {
    if raw.is_empty() || raw.len() > MAX_RPK_KEY_LEN {
        return Err(format!("invalid RPK key length ({})", raw.len()));
    }
    if raw.contains('\\') || raw.contains('\0') || raw.starts_with('/') || raw.ends_with('/') {
        return Err(format!("invalid RPK key '{raw}'"));
    }
    for seg in raw.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(format!("invalid RPK key segment in '{raw}'"));
        }
    }
    Ok(raw.to_string())
}

/// Heuristic: compress text/table/binary-grid assets with raw-deflate; store already-compressed
/// media (`.webp`, `.png`, `.jpg`, `.ogg`, `.mp3`, `.wav`, `.ttf`) raw (`RPK_COMP_RAW`).
pub fn should_compress_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".json")
        || lower.ends_with(".bin")
        || lower.ends_with(".txt")
        || lower.ends_with(".mid")
        || lower.ends_with(".asc")
        || lower.ends_with(".rpg")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpkHeader {
    pub format_major: u16,
    pub format_minor: u16,
    pub header_len: u16,
    pub entry_stride: u16,
    pub flags: u32,
    pub toc_offset: u64,
    pub toc_stored_len: u32,
    pub entry_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpkEntryMeta {
    pub index: u32,
    pub key: String,
    pub offset: u64,
    pub stored_len: u32,
    pub raw_len: u32,
    pub crc32: u32,
    pub compression: u8,
    pub entry_flags: u8,
}

#[derive(Clone, Debug)]
pub struct RpkWriteOptions {
    pub pack_id: String,
    pub compress_toc: bool,
    /// Optional 32-byte per-pack content key; when `Some`, entries are AES-256-GCM encrypted.
    pub content_key: Option<[u8; 32]>,
    /// When `true` (and `content_key.is_some()`), encrypt the TOC payload as well.
    pub encrypt_toc: bool,
    /// Optional advisory flags (bits 16..31) stored in the header.
    pub advisory_flags: u32,
}

impl Default for RpkWriteOptions {
    fn default() -> Self {
        Self {
            pack_id: "pack".to_string(),
            compress_toc: true,
            content_key: None,
            encrypt_toc: false,
            advisory_flags: 0,
        }
    }
}

/// In-memory builder that writes a deterministic, 8-byte-aligned `RPK1` pack.
pub struct RpkBuilder {
    opts: RpkWriteOptions,
    entries: Vec<(String, Vec<u8>, bool)>,
}

impl RpkBuilder {
    pub fn new(opts: RpkWriteOptions) -> Self {
        Self {
            opts,
            entries: Vec::new(),
        }
    }

    /// Add an entry (compression auto-selected by [`should_compress_key`]).
    pub fn add(&mut self, key: &str, data: Vec<u8>) -> Result<(), String> {
        let compress = should_compress_key(key);
        self.add_with_compression(key, data, compress)
    }

    pub fn add_with_compression(
        &mut self,
        key: &str,
        data: Vec<u8>,
        compress: bool,
    ) -> Result<(), String> {
        let norm = normalize_rpk_key(key)?;
        if data.len() > MAX_RPK_ENTRY_RAW as usize {
            return Err(format!("entry '{norm}' exceeds MAX_RPK_ENTRY_RAW"));
        }
        self.entries.push((norm, data, compress));
        Ok(())
    }

    /// Serialize the pack into bytes (deterministic order sorted by key).
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        if self.entries.len() > MAX_RPK_ENTRIES as usize {
            return Err(format!("too many entries ({})", self.entries.len()));
        }
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        for w in self.entries.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(format!("duplicate RPK key '{}'", w[0].0));
            }
        }

        let encrypt_entries = self.opts.content_key.is_some();
        let encrypt_toc = encrypt_entries && self.opts.encrypt_toc;
        let mut flags = (self.opts.advisory_flags & !RPK_MUST_UNDERSTAND_MASK)
            | if self.opts.compress_toc { RPK_FLAG_TOC_DEFLATED } else { 0 }
            | if encrypt_entries { RPK_FLAG_ENCRYPTED } else { 0 }
            | if encrypt_toc { RPK_FLAG_TOC_ENCRYPTED } else { 0 };

        // Step 1: Compress (before encrypt!) and optionally encrypt each entry payload.
        struct Prepared {
            key: String,
            stored: Vec<u8>,
            raw_len: u32,
            crc32: u32,
            compression: u8,
        }
        let mut prepared = Vec::with_capacity(self.entries.len());
        for (idx, (key, raw, want_comp)) in self.entries.into_iter().enumerate() {
            let crc = crc32_ieee(&raw);
            let raw_len = raw.len() as u32;
            let (mut body, comp) = if want_comp && raw.len() >= 32 {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
                enc.write_all(&raw).map_err(|e| format!("deflate '{key}': {e}"))?;
                let deflated = enc.finish().map_err(|e| format!("deflate finish '{key}': {e}"))?;
                if deflated.len() < raw.len() {
                    (deflated, RPK_COMP_DEFLATE)
                } else {
                    (raw, RPK_COMP_RAW)
                }
            } else {
                (raw, RPK_COMP_RAW)
            };
            if let Some(ckey) = &self.opts.content_key {
                let nonce = derive_rpk_nonce(&self.opts.pack_id, idx as u32);
                let aad = format!("{}\0{key}", self.opts.pack_id);
                body = aes256_gcm_encrypt(ckey, &nonce, aad.as_bytes(), &body);
            }
            prepared.push(Prepared {
                key,
                stored: body,
                raw_len,
                crc32: crc,
                compression: comp,
            });
        }

        // Step 2: Compute TOC size first (two-pass so offsets are exact with 8-byte alignment).
        let encode_toc = |base_data_offset: u64| -> Vec<u8> {
            let mut toc = Vec::new();
            let mut cur = base_data_offset;
            for p in &prepared {
                // 8-byte alignment for clean slice/mmap access
                let rem = cur % 8;
                if rem != 0 {
                    cur += 8 - rem;
                }
                toc.extend_from_slice(&cur.to_le_bytes());
                toc.extend_from_slice(&(p.stored.len() as u32).to_le_bytes());
                toc.extend_from_slice(&p.raw_len.to_le_bytes());
                toc.extend_from_slice(&p.crc32.to_le_bytes());
                toc.push(p.compression);
                toc.push(0u8); // entry_flags
                toc.extend_from_slice(&(p.key.len() as u16).to_le_bytes());
                toc.extend_from_slice(&0u16.to_le_bytes()); // ext_len = 0
                toc.extend_from_slice(&[0u8; 6]); // reserved
                toc.extend_from_slice(p.key.as_bytes());
                cur += p.stored.len() as u64;
            }
            toc
        };

        let finalize_toc = |raw_toc: Vec<u8>, flags_ref: &mut u32| -> Result<Vec<u8>, String> {
            let mut buf = if self.opts.compress_toc {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
                enc.write_all(&raw_toc).map_err(|e| format!("deflate TOC: {e}"))?;
                let deflated = enc.finish().map_err(|e| format!("deflate TOC finish: {e}"))?;
                if deflated.len() < raw_toc.len() {
                    *flags_ref |= RPK_FLAG_TOC_DEFLATED;
                    deflated
                } else {
                    *flags_ref &= !RPK_FLAG_TOC_DEFLATED;
                    raw_toc
                }
            } else {
                raw_toc
            };
            if encrypt_toc {
                let ckey = self.opts.content_key.as_ref().unwrap();
                let nonce = derive_rpk_nonce(&self.opts.pack_id, u32::MAX);
                let aad = format!("{}\0__TOC__", self.opts.pack_id);
                buf = aes256_gcm_encrypt(ckey, &nonce, aad.as_bytes(), &buf);
            }
            Ok(buf)
        };

        // Pass 1: estimate compressed TOC length (deflate size can vary by a few bytes if offsets
        // change, so iterate up to 3 times until data_offset stabilizes, then pad if shorter).
        let header_len = RPK_MIN_HEADER_LEN as u64;
        let mut data_offset = header_len + encode_toc(header_len).len() as u64 + 64;
        let mut toc_stored;
        loop {
            let mut trial_flags = flags;
            let raw_toc = encode_toc(data_offset);
            toc_stored = finalize_toc(raw_toc, &mut trial_flags)?;
            let needed = header_len + toc_stored.len() as u64;
            let aligned = (needed + 7) & !7;
            if aligned <= data_offset {
                flags = trial_flags;
                break;
            }
            data_offset = aligned + 32;
        }
        // Align data_offset to 8 bytes
        if data_offset % 8 != 0 {
            data_offset = (data_offset + 7) & !7;
        }
        // Final TOC pass with exact frozen `data_offset`
        let mut final_flags = flags;
        toc_stored = finalize_toc(encode_toc(data_offset), &mut final_flags)?;
        if header_len + toc_stored.len() as u64 > data_offset {
            return Err("internal RPK TOC layout overflow".to_string());
        }
        flags = final_flags;

        // Step 3: Assemble header + TOC + padding + 8B-aligned entry payloads.
        let mut out = Vec::new();
        out.extend_from_slice(RPK_MAGIC);
        out.extend_from_slice(&RPK_FORMAT_MAJOR.to_le_bytes());
        out.extend_from_slice(&RPK_FORMAT_MINOR.to_le_bytes());
        out.extend_from_slice(&RPK_MIN_HEADER_LEN.to_le_bytes());
        out.extend_from_slice(&RPK_MIN_ENTRY_STRIDE.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&header_len.to_le_bytes()); // toc_offset
        out.extend_from_slice(&(toc_stored.len() as u32).to_le_bytes());
        out.extend_from_slice(&(prepared.len() as u32).to_le_bytes());
        debug_assert_eq!(out.len(), RPK_MIN_HEADER_LEN as usize);
        out.extend_from_slice(&toc_stored);
        while (out.len() as u64) < data_offset {
            out.push(0);
        }
        for p in prepared {
            while out.len() % 8 != 0 {
                out.push(0);
            }
            out.extend_from_slice(&p.stored);
        }
        Ok(out)
    }
}

/// Zero-extraction random-access `RPK1` reader over any `Read + Seek` (e.g. `File` or `Cursor`).
pub struct RpkReader<R: Read + Seek> {
    inner: R,
    file_len: u64,
    pack_id: String,
    header: RpkHeader,
    content_key: Option<[u8; 32]>,
    entries: HashMap<String, RpkEntryMeta>,
}

impl<R: Read + Seek> RpkReader<R> {
    pub fn open(
        mut inner: R,
        pack_id: impl Into<String>,
        content_key: Option<[u8; 32]>,
    ) -> Result<Self, String> {
        let pack_id = pack_id.into();
        let file_len = inner
            .seek(SeekFrom::End(0))
            .map_err(|e| format!("seek end: {e}"))?;
        if file_len < RPK_MIN_HEADER_LEN as u64 {
            return Err("RPK file too short for header".to_string());
        }
        inner
            .seek(SeekFrom::Start(0))
            .map_err(|e| format!("seek start: {e}"))?;
        let mut hdr = [0u8; 32];
        inner
            .read_exact(&mut hdr)
            .map_err(|e| format!("read header: {e}"))?;
        if &hdr[0..4] != RPK_MAGIC {
            return Err("invalid RPK magic (expected RPK1)".to_string());
        }
        let format_major = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
        let format_minor = u16::from_le_bytes(hdr[6..8].try_into().unwrap());
        let header_len = u16::from_le_bytes(hdr[8..10].try_into().unwrap());
        let entry_stride = u16::from_le_bytes(hdr[10..12].try_into().unwrap());
        let flags = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
        let toc_offset = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
        let toc_stored_len = u32::from_le_bytes(hdr[24..28].try_into().unwrap());
        let entry_count = u32::from_le_bytes(hdr[28..32].try_into().unwrap());

        if format_major != RPK_FORMAT_MAJOR {
            return Err(format!(
                "unsupported RPK major version {format_major} (supported: {RPK_FORMAT_MAJOR})"
            ));
        }
        if header_len < RPK_MIN_HEADER_LEN {
            return Err(format!("invalid RPK header_len {header_len}"));
        }
        if entry_stride < RPK_MIN_ENTRY_STRIDE {
            return Err(format!("invalid RPK entry_stride {entry_stride}"));
        }
        let unknown_must = (flags & RPK_MUST_UNDERSTAND_MASK) & !RPK_KNOWN_MUST_UNDERSTAND_FLAGS;
        if unknown_must != 0 {
            return Err(format!(
                "unsupported required RPK flags 0x{unknown_must:08x} (upgrade client)"
            ));
        }
        if entry_count > MAX_RPK_ENTRIES {
            return Err(format!(
                "RPK entry_count {entry_count} exceeds cap {MAX_RPK_ENTRIES}"
            ));
        }
        if toc_stored_len > MAX_RPK_TOC_STORED {
            return Err(format!(
                "RPK toc_stored_len {toc_stored_len} exceeds cap {MAX_RPK_TOC_STORED}"
            ));
        }
        let toc_end = toc_offset
            .checked_add(toc_stored_len as u64)
            .ok_or_else(|| "RPK toc_offset overflow".to_string())?;
        if toc_offset < header_len as u64 || toc_end > file_len {
            return Err("RPK TOC range out of file bounds".to_string());
        }
        if (flags & (RPK_FLAG_ENCRYPTED | RPK_FLAG_TOC_ENCRYPTED)) != 0 && content_key.is_none() {
            return Err("RPK pack is encrypted but no decryption key was provided".to_string());
        }

        // Read & decode TOC
        inner
            .seek(SeekFrom::Start(toc_offset))
            .map_err(|e| format!("seek TOC: {e}"))?;
        let mut toc_buf = vec![0u8; toc_stored_len as usize];
        inner
            .read_exact(&mut toc_buf)
            .map_err(|e| format!("read TOC: {e}"))?;

        if (flags & RPK_FLAG_TOC_ENCRYPTED) != 0 {
            let ckey = content_key.as_ref().unwrap();
            let nonce = derive_rpk_nonce(&pack_id, u32::MAX);
            let aad = format!("{pack_id}\0__TOC__");
            toc_buf = aes256_gcm_decrypt(ckey, &nonce, aad.as_bytes(), &toc_buf)?;
        }
        if (flags & RPK_FLAG_TOC_DEFLATED) != 0 {
            toc_buf = inflate_bounded(&toc_buf, MAX_RPK_TOC_RAW)?;
        }

        let entries = parse_binary_toc(&toc_buf, entry_count, entry_stride, file_len)?;
        Ok(Self {
            inner,
            file_len,
            pack_id,
            header: RpkHeader {
                format_major,
                format_minor,
                header_len,
                entry_stride,
                flags,
                toc_offset,
                toc_stored_len,
                entry_count,
            },
            content_key,
            entries,
        })
    }

    pub fn header(&self) -> &RpkHeader {
        &self.header
    }

    pub fn pack_id(&self) -> &str {
        &self.pack_id
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn meta(&self, key: &str) -> Option<&RpkEntryMeta> {
        self.entries.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|s| s.as_str())
    }

    /// Read, decrypt (if encrypted), inflate (if compressed), and verify a single entry in O(entry).
    pub fn read_entry(&mut self, key: &str) -> Result<Vec<u8>, String> {
        let meta = self
            .entries
            .get(key)
            .ok_or_else(|| format!("RPK entry '{key}' not found in '{}'", self.pack_id))?
            .clone();
        let end = meta
            .offset
            .checked_add(meta.stored_len as u64)
            .ok_or_else(|| "RPK entry offset overflow".to_string())?;
        if end > self.file_len {
            return Err(format!("RPK entry '{key}' exceeds file bounds"));
        }
        self.inner
            .seek(SeekFrom::Start(meta.offset))
            .map_err(|e| format!("seek entry '{key}': {e}"))?;
        let mut buf = vec![0u8; meta.stored_len as usize];
        self.inner
            .read_exact(&mut buf)
            .map_err(|e| format!("read entry '{key}': {e}"))?;

        if (self.header.flags & RPK_FLAG_ENCRYPTED) != 0 {
            let ckey = self
                .content_key
                .as_ref()
                .ok_or_else(|| "missing content key".to_string())?;
            let nonce = derive_rpk_nonce(&self.pack_id, meta.index);
            let aad = format!("{}\0{}", self.pack_id, meta.key);
            buf = aes256_gcm_decrypt(ckey, &nonce, aad.as_bytes(), &buf)?;
        }

        let plain = match meta.compression {
            RPK_COMP_RAW => buf,
            RPK_COMP_DEFLATE => inflate_bounded(&buf, meta.raw_len as usize)?,
            other => return Err(format!("unsupported compression id {other} on '{key}'")),
        };

        if plain.len() != meta.raw_len as usize {
            return Err(format!(
                "RPK entry '{key}' raw_len mismatch (got {}, expected {})",
                plain.len(),
                meta.raw_len
            ));
        }
        let actual_crc = crc32_ieee(&plain);
        if actual_crc != meta.crc32 {
            return Err(format!(
                "RPK entry '{key}' CRC32 mismatch (got 0x{actual_crc:08x}, expected 0x{:08x})",
                meta.crc32
            ));
        }
        Ok(plain)
    }
}

pub type RpkMemoryReader<'a> = RpkReader<Cursor<&'a [u8]>>;

fn inflate_bounded(data: &[u8], max_out: usize) -> Result<Vec<u8>, String> {
    let mut dec = DeflateDecoder::new(data);
    let mut out = Vec::new();
    let mut limited = (&mut dec).take((max_out as u64).saturating_add(1));
    limited
        .read_to_end(&mut out)
        .map_err(|e| format!("inflate failed: {e}"))?;
    if out.len() > max_out {
        return Err(format!("inflated payload exceeds limit {max_out} bytes"));
    }
    Ok(out)
}

fn parse_binary_toc(
    toc: &[u8],
    entry_count: u32,
    entry_stride: u16,
    file_len: u64,
) -> Result<HashMap<String, RpkEntryMeta>, String> {
    let stride = entry_stride as usize;
    let mut pos = 0usize;
    let mut map = HashMap::with_capacity(entry_count as usize);
    for idx in 0..entry_count {
        if pos + stride > toc.len() {
            return Err(format!("truncated RPK TOC at entry {idx}"));
        }
        let rec = &toc[pos..pos + stride];
        let offset = u64::from_le_bytes(rec[0..8].try_into().unwrap());
        let stored_len = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        let raw_len = u32::from_le_bytes(rec[12..16].try_into().unwrap());
        let crc32 = u32::from_le_bytes(rec[16..20].try_into().unwrap());
        let compression = rec[20];
        let entry_flags = rec[21];
        let key_len = u16::from_le_bytes(rec[22..24].try_into().unwrap()) as usize;
        let ext_len = u16::from_le_bytes(rec[24..26].try_into().unwrap()) as usize;
        pos += stride;

        if key_len == 0 || key_len > MAX_RPK_KEY_LEN {
            return Err(format!("invalid RPK key_len {key_len} at entry {idx}"));
        }
        if raw_len > MAX_RPK_ENTRY_RAW {
            return Err(format!(
                "RPK entry {idx} raw_len {raw_len} exceeds cap {MAX_RPK_ENTRY_RAW}"
            ));
        }
        if pos + key_len + ext_len > toc.len() {
            return Err(format!("truncated RPK TOC key/ext at entry {idx}"));
        }
        let key_raw = std::str::from_utf8(&toc[pos..pos + key_len])
            .map_err(|_| format!("non-UTF8 RPK key at entry {idx}"))?;
        let key = normalize_rpk_key(key_raw)?;
        pos += key_len + ext_len; // skip optional forward-compatible extension bytes!

        let end = offset
            .checked_add(stored_len as u64)
            .ok_or_else(|| format!("RPK entry '{key}' offset overflow"))?;
        if end > file_len {
            return Err(format!("RPK entry '{key}' out of file bounds"));
        }
        if map.contains_key(&key) {
            return Err(format!("duplicate RPK key '{key}' in TOC"));
        }
        map.insert(
            key.clone(),
            RpkEntryMeta {
                index: idx,
                key,
                offset,
                stored_len,
                raw_len,
                crc32,
                compression,
                entry_flags,
            },
        );
    }
    Ok(map)
}

/// Compute SHA-256 hex digest of an entire `.rpk` byte slice (matches manifest `sha256`).
pub fn rpk_sha256_hex(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{unwrap_pack_key, wrap_pack_key};

    #[test]
    fn rpk_plain_and_encrypted_roundtrip_and_tamper_checks() {
        let yaml_data = b"scenes:\n  - id: 1\n    name: Yuhang\n".repeat(40);
        let webp_data = vec![0x52, 0x49, 0x46, 0x46, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

        // 1. Plain pack round-trip
        let mut b_plain = RpkBuilder::new(RpkWriteOptions {
            pack_id: "ch01a-yuhang".into(),
            compress_toc: true,
            content_key: None,
            encrypt_toc: false,
            advisory_flags: 1 << 16, // advisory bit must be safely ignored by reader
        });
        b_plain.add("data/scenes.yaml", yaml_data.clone()).unwrap();
        b_plain
            .add("graphics/mgo/mgo_001_4x.webp", webp_data.clone())
            .unwrap();
        let plain_bytes = b_plain.finish().unwrap();

        let mut r_plain = RpkReader::open(Cursor::new(plain_bytes.as_slice()), "ch01a-yuhang", None)
            .expect("open plain RPK");
        assert_eq!(r_plain.len(), 2);
        assert_eq!(r_plain.meta("data/scenes.yaml").unwrap().compression, RPK_COMP_DEFLATE);
        assert_eq!(r_plain.meta("graphics/mgo/mgo_001_4x.webp").unwrap().compression, RPK_COMP_RAW);
        assert_eq!(r_plain.read_entry("data/scenes.yaml").unwrap(), yaml_data);
        assert_eq!(r_plain.read_entry("graphics/mgo/mgo_001_4x.webp").unwrap(), webp_data);

        // Plain tamper: flipping 1 byte inside an entry fails CRC32!
        let off = r_plain.meta("graphics/mgo/mgo_001_4x.webp").unwrap().offset as usize;
        let mut tampered_plain = plain_bytes.clone();
        tampered_plain[off + 3] ^= 0x01;
        let mut r_bad_plain =
            RpkReader::open(Cursor::new(tampered_plain.as_slice()), "ch01a-yuhang", None).unwrap();
        assert!(r_bad_plain.read_entry("graphics/mgo/mgo_001_4x.webp").is_err());

        // 2. Encrypted pack (entries + TOC encrypted, key wrapped under master key)
        let master_key = [0xABu8; 32];
        let content_key = [0xCDu8; 32];
        let wrapped = wrap_pack_key(&master_key, "ch01a-yuhang", &content_key, &[0x55u8; 12]);
        let unwrapped = unwrap_pack_key(&master_key, "ch01a-yuhang", &wrapped).unwrap();

        let mut b_enc = RpkBuilder::new(RpkWriteOptions {
            pack_id: "ch01a-yuhang".into(),
            compress_toc: true,
            content_key: Some(content_key),
            encrypt_toc: true,
            advisory_flags: 0,
        });
        b_enc.add("data/scenes.yaml", yaml_data.clone()).unwrap();
        b_enc
            .add("graphics/mgo/mgo_001_4x.webp", webp_data.clone())
            .unwrap();
        let enc_bytes = b_enc.finish().unwrap();

        // Opening encrypted pack without key or with wrong key or wrong pack_id (AAD) fails!
        assert!(RpkReader::open(Cursor::new(enc_bytes.as_slice()), "ch01a-yuhang", None).is_err());
        assert!(
            RpkReader::open(Cursor::new(enc_bytes.as_slice()), "ch01a-yuhang", Some([0x11u8; 32]))
                .is_err()
        );
        assert!(
            RpkReader::open(Cursor::new(enc_bytes.as_slice()), "other-pack", Some(unwrapped))
                .is_err()
        );

        let mut r_enc =
            RpkReader::open(Cursor::new(enc_bytes.as_slice()), "ch01a-yuhang", Some(unwrapped))
                .expect("open encrypted RPK");
        assert_eq!(r_enc.read_entry("data/scenes.yaml").unwrap(), yaml_data);
        assert_eq!(r_enc.read_entry("graphics/mgo/mgo_001_4x.webp").unwrap(), webp_data);

        // Tampering 1 byte in encrypted entry fails GCM tag verification!
        let enc_off = r_enc.meta("data/scenes.yaml").unwrap().offset as usize;
        let mut tampered_enc = enc_bytes.clone();
        tampered_enc[enc_off] ^= 0x01;
        let mut r_bad_enc = RpkReader::open(
            Cursor::new(tampered_enc.as_slice()),
            "ch01a-yuhang",
            Some(unwrapped),
        )
        .unwrap();
        assert!(r_bad_enc.read_entry("data/scenes.yaml").is_err());
        // Other untouched entry still decrypts cleanly (proves per-entry random access!)
        assert_eq!(
            r_bad_enc.read_entry("graphics/mgo/mgo_001_4x.webp").unwrap(),
            webp_data
        );
    }

    #[test]
    fn rpk_forward_compatibility_skips_future_minor_extensions() {
        // Synthesize a future v1.2 pack with wider header_len (40), wider entry_stride (40),
        // advisory flag bit 20, and per-entry ext_len (6 bytes).
        let payload = b"hello future v1.2";
        let key = b"data/test.txt";
        let crc = crc32_ieee(payload);
        let header_len: u16 = 40;
        let entry_stride: u16 = 40;
        let toc_offset: u64 = 40;
        let ext_bytes = b"EXTTLV";
        let toc_len = entry_stride as usize + key.len() + ext_bytes.len();
        let data_offset: u64 = ((toc_offset as usize + toc_len + 7) & !7) as u64;

        let mut buf = Vec::new();
        buf.extend_from_slice(RPK_MAGIC);
        buf.extend_from_slice(&1u16.to_le_bytes()); // major = 1
        buf.extend_from_slice(&2u16.to_le_bytes()); // minor = 2 (future!)
        buf.extend_from_slice(&header_len.to_le_bytes());
        buf.extend_from_slice(&entry_stride.to_le_bytes());
        buf.extend_from_slice(&(1u32 << 20).to_le_bytes()); // advisory flag
        buf.extend_from_slice(&toc_offset.to_le_bytes());
        buf.extend_from_slice(&(toc_len as u32).to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // entry_count
        buf.extend_from_slice(&[0xEEu8; 8]); // 8 bytes of future header fields

        // Entry 0 (40-byte stride + key + 6-byte ext)
        buf.extend_from_slice(&data_offset.to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.push(RPK_COMP_RAW);
        buf.push(0);
        buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(ext_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&[0u8; 6]);
        buf.extend_from_slice(&[0xDDu8; 8]); // 8 bytes of future fixed entry metadata
        buf.extend_from_slice(key);
        buf.extend_from_slice(ext_bytes);
        while (buf.len() as u64) < data_offset {
            buf.push(0);
        }
        buf.extend_from_slice(payload);

        let mut reader = RpkReader::open(Cursor::new(buf.as_slice()), "future", None)
            .expect("v1.0 reader must accept future v1.2 minor extensions");
        assert_eq!(reader.header().format_minor, 2);
        assert_eq!(reader.read_entry("data/test.txt").unwrap(), payload);

        // Unknown MUST-UNDERSTAND flag (bit 5) MUST be rejected!
        buf[12..16].copy_from_slice(&(1u32 << 5).to_le_bytes());
        assert!(RpkReader::open(Cursor::new(buf.as_slice()), "future", None).is_err());

        // Unsupported major version (v2) MUST be rejected!
        buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        buf[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(RpkReader::open(Cursor::new(buf.as_slice()), "future", None).is_err());
    }

    #[test]
    fn rpk_rejects_path_traversal_and_malformed_fuzz_inputs() {
        assert!(normalize_rpk_key("../secret").is_err());
        assert!(normalize_rpk_key("a/../b").is_err());
        assert!(normalize_rpk_key("/abs/path").is_err());
        assert!(normalize_rpk_key("a\\b").is_err());
        assert!(normalize_rpk_key("").is_err());

        // Malformed / truncated headers never panic
        for bad in [
            vec![],
            b"RPK1".to_vec(),
            vec![0u8; 64],
            {
                let mut v = vec![0u8; 32];
                v[0..4].copy_from_slice(RPK_MAGIC);
                v[4..6].copy_from_slice(&1u16.to_le_bytes());
                v[8..10].copy_from_slice(&32u16.to_le_bytes());
                v[10..12].copy_from_slice(&32u16.to_le_bytes());
                v[28..32].copy_from_slice(&u32::MAX.to_le_bytes()); // absurd entry_count
                v
            },
        ] {
            assert!(RpkReader::open(Cursor::new(bad.as_slice()), "fuzz", None).is_err());
        }
    }
}
