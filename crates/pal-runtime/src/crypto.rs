//! Zero-dependency pure-Rust cryptographic & integrity primitives shared across
//! native (`pal-desktop`, `pal-headless`) and browser WASM (`pal-wasm`) for `#237` RPK1 packs:
//!
//! - [`Sha256`]: Streaming FIPS 180-4 SHA-256 (also used by `pal-wasm` for chunked pack hashing).
//! - [`crc32_ieee`]: Standard IEEE 802.3 CRC-32 for per-entry plain RPK integrity.
//! - [`aes256_gcm_encrypt`] / [`aes256_gcm_decrypt`]: Standard FIPS 197 AES-256 + NIST SP 800-38D
//!   GCM (96-bit nonce, 128-bit tag, constant-time tag verification, 100% WebCrypto & Python
//!   `cryptography` interoperable).
//! - [`wrap_pack_key`] / [`unwrap_pack_key`]: Master-key AES-256-GCM wrapping of per-pack content keys.

/// Constant-time slice equality check (avoids early-exit timing leaks on auth tags).
#[inline]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ============================================================================
// IEEE 802.3 CRC-32
// ============================================================================

pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ============================================================================
// Streaming FIPS 180-4 SHA-256
// ============================================================================

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let block: [u8; 64] = data[..64].try_into().unwrap();
            self.compress(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for b in &mut self.buf[self.buf_len..64] {
                *b = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        for b in &mut self.buf[self.buf_len..56] {
            *b = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, &w) in self.h.iter().enumerate() {
            out[i * 4..(i + 1) * 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let clean = s.trim();
    if clean.len() % 2 != 0 {
        return Err("hex string must have even length".to_string());
    }
    let bytes = clean.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let nib = |c: u8| -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("invalid hex character '{}'", c as char)),
        }
    };
    for pair in bytes.chunks_exact(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Ok(out)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

// ============================================================================
// FIPS 197 AES-256 + NIST SP 800-38D GCM (96-bit nonce, 128-bit tag)
// ============================================================================

const AES_SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

#[inline]
fn xtime(a: u8) -> u8 {
    (a << 1) ^ (((a >> 7) & 1).wrapping_neg() & 0x1b)
}

#[derive(Clone)]
struct Aes256 {
    rk: [[u8; 16]; 15],
}

impl Aes256 {
    fn new(key: &[u8; 32]) -> Self {
        let mut w = [0u32; 60];
        for i in 0..8 {
            w[i] = u32::from_be_bytes(key[i * 4..(i + 1) * 4].try_into().unwrap());
        }
        let mut rcon: u8 = 0x01;
        for i in 8..60 {
            let mut tmp = w[i - 1];
            if i % 8 == 0 {
                tmp = tmp.rotate_left(8);
                let b = tmp.to_be_bytes();
                tmp = u32::from_be_bytes([
                    AES_SBOX[b[0] as usize] ^ rcon,
                    AES_SBOX[b[1] as usize],
                    AES_SBOX[b[2] as usize],
                    AES_SBOX[b[3] as usize],
                ]);
                rcon = xtime(rcon);
            } else if i % 8 == 4 {
                let b = tmp.to_be_bytes();
                tmp = u32::from_be_bytes([
                    AES_SBOX[b[0] as usize],
                    AES_SBOX[b[1] as usize],
                    AES_SBOX[b[2] as usize],
                    AES_SBOX[b[3] as usize],
                ]);
            }
            w[i] = w[i - 8] ^ tmp;
        }
        let mut rk = [[0u8; 16]; 15];
        for r in 0..15 {
            for c in 0..4 {
                rk[r][c * 4..(c + 1) * 4].copy_from_slice(&w[r * 4 + c].to_be_bytes());
            }
        }
        Self { rk }
    }

    fn encrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut s = *input;
        for i in 0..16 {
            s[i] ^= self.rk[0][i];
        }
        for round in 1..15 {
            // SubBytes
            for b in &mut s {
                *b = AES_SBOX[*b as usize];
            }
            // ShiftRows (column-major state)
            let t = s;
            s[1] = t[5];
            s[5] = t[9];
            s[9] = t[13];
            s[13] = t[1];
            s[2] = t[10];
            s[6] = t[14];
            s[10] = t[2];
            s[14] = t[6];
            s[3] = t[15];
            s[7] = t[3];
            s[11] = t[7];
            s[15] = t[11];
            // MixColumns (rounds 1..=13)
            if round < 14 {
                for c in 0..4 {
                    let i = c * 4;
                    let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
                    let xor = a0 ^ a1 ^ a2 ^ a3;
                    s[i] = a0 ^ xor ^ xtime(a0 ^ a1);
                    s[i + 1] = a1 ^ xor ^ xtime(a1 ^ a2);
                    s[i + 2] = a2 ^ xor ^ xtime(a2 ^ a3);
                    s[i + 3] = a3 ^ xor ^ xtime(a3 ^ a0);
                }
            }
            // AddRoundKey
            for i in 0..16 {
                s[i] ^= self.rk[round][i];
            }
        }
        s
    }
}

/// Multiplication in GF(2^128) with NIST SP 800-38D polynomial `x^128 + x^7 + x^2 + x + 1` (R = 0xE1 << 120).
fn ghash_mul(x: u128, y: u128) -> u128 {
    let mut z: u128 = 0;
    let mut v: u128 = y;
    for i in 0..128 {
        let bit = (x >> (127 - i)) & 1;
        if bit != 0 {
            z ^= v;
        }
        let lsb = v & 1;
        v >>= 1;
        if lsb != 0 {
            v ^= 0xE100_0000_0000_0000_0000_0000_0000_0000u128;
        }
    }
    z
}

fn ghash(h: u128, aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut y: u128 = 0;
    let mut feed = |data: &[u8]| {
        let mut chunks = data.chunks_exact(16);
        for blk in chunks.by_ref() {
            let v = u128::from_be_bytes(blk.try_into().unwrap());
            y = ghash_mul(y ^ v, h);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut pad = [0u8; 16];
            pad[..rem.len()].copy_from_slice(rem);
            y = ghash_mul(y ^ u128::from_be_bytes(pad), h);
        }
    };
    feed(aad);
    feed(ct);
    let aad_bits = (aad.len() as u64).wrapping_mul(8);
    let ct_bits = (ct.len() as u64).wrapping_mul(8);
    let mut len_blk = [0u8; 16];
    len_blk[..8].copy_from_slice(&aad_bits.to_be_bytes());
    len_blk[8..16].copy_from_slice(&ct_bits.to_be_bytes());
    y = ghash_mul(y ^ u128::from_be_bytes(len_blk), h);
    y.to_be_bytes()
}

fn gcm_ctr_xor(aes: &Aes256, j0: &[u8; 16], data: &mut [u8]) {
    let mut ctr = *j0;
    let mut n = u32::from_be_bytes(ctr[12..16].try_into().unwrap());
    for chunk in data.chunks_mut(16) {
        n = n.wrapping_add(1);
        ctr[12..16].copy_from_slice(&n.to_be_bytes());
        let ks = aes.encrypt_block(&ctr);
        for (b, &k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= k;
        }
    }
}

/// Encrypt `plaintext` with AES-256-GCM (96-bit nonce, 128-bit tag), returning `ciphertext || tag`.
pub fn aes256_gcm_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let aes = Aes256::new(key);
    let h = u128::from_be_bytes(aes.encrypt_block(&[0u8; 16]));
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;

    let mut out = Vec::with_capacity(plaintext.len() + 16);
    out.extend_from_slice(plaintext);
    gcm_ctr_xor(&aes, &j0, &mut out);

    let s = ghash(h, aad, &out);
    let ej0 = aes.encrypt_block(&j0);
    let mut tag = [0u8; 16];
    for i in 0..16 {
        tag[i] = s[i] ^ ej0[i];
    }
    out.extend_from_slice(&tag);
    out
}

/// Decrypt `ciphertext_and_tag` (`ciphertext || 16B tag`) with AES-256-GCM.
/// Verifies the authentication tag in constant time BEFORE returning plaintext.
pub fn aes256_gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext_and_tag: &[u8],
) -> Result<Vec<u8>, String> {
    if ciphertext_and_tag.len() < 16 {
        return Err("AES-256-GCM ciphertext shorter than 16-byte tag".to_string());
    }
    let (ct, tag) = ciphertext_and_tag.split_at(ciphertext_and_tag.len() - 16);
    let aes = Aes256::new(key);
    let h = u128::from_be_bytes(aes.encrypt_block(&[0u8; 16]));
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;

    let s = ghash(h, aad, ct);
    let ej0 = aes.encrypt_block(&j0);
    let mut expected_tag = [0u8; 16];
    for i in 0..16 {
        expected_tag[i] = s[i] ^ ej0[i];
    }
    if !ct_eq(tag, &expected_tag) {
        return Err("AES-256-GCM authentication failed (wrong key or tampered data)".to_string());
    }

    let mut pt = ct.to_vec();
    gcm_ctr_xor(&aes, &j0, &mut pt);
    Ok(pt)
}

/// Derive a deterministic 96-bit RPK1 per-entry nonce from `pack_id` and `entry_index`.
/// Each pack build uses a fresh random 256-bit content key, so `(content_key, nonce)` never repeats.
pub fn derive_rpk_nonce(pack_id: &str, entry_index: u32) -> [u8; 12] {
    let h = sha256(pack_id.as_bytes());
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&h[..8]);
    nonce[8..12].copy_from_slice(&entry_index.to_le_bytes());
    nonce
}

/// Wrap a 32-byte per-pack content key under `master_key` using AES-256-GCM (`v1:<hex(nonce||ct||tag)>`).
/// Bound to `pack_id` via AAD so wrapped keys cannot be transplanted between packs.
pub fn wrap_pack_key(
    master_key: &[u8; 32],
    pack_id: &str,
    content_key: &[u8; 32],
    wrap_nonce: &[u8; 12],
) -> String {
    let aad = format!("repal-pack-key/v1\0{pack_id}");
    let ct_tag = aes256_gcm_encrypt(master_key, wrap_nonce, aad.as_bytes(), content_key);
    let mut blob = Vec::with_capacity(12 + ct_tag.len());
    blob.extend_from_slice(wrap_nonce);
    blob.extend_from_slice(&ct_tag);
    format!("v1:{}", hex_encode(&blob))
}

/// Unwrap a `v1:<hex(nonce||ct||tag)>` content key wrapped by [`wrap_pack_key`].
pub fn unwrap_pack_key(
    master_key: &[u8; 32],
    pack_id: &str,
    wrapped: &str,
) -> Result<[u8; 32], String> {
    let hex_part = wrapped
        .trim()
        .strip_prefix("v1:")
        .ok_or_else(|| "unsupported wrapped_key version (expected 'v1:...')".to_string())?;
    let blob = hex_decode(hex_part)?;
    if blob.len() != 12 + 32 + 16 {
        return Err(format!("invalid wrapped_key length {} (expected 60 bytes)", blob.len()));
    }
    let nonce: [u8; 12] = blob[..12].try_into().unwrap();
    let aad = format!("repal-pack-key/v1\0{pack_id}");
    let pt = aes256_gcm_decrypt(master_key, &nonce, aad.as_bytes(), &blob[12..])?;
    let key: [u8; 32] = pt
        .try_into()
        .map_err(|_| "unwrapped pack key is not 32 bytes".to_string())?;
    Ok(key)
}

/// Parse a 32-byte hex master key (64 hex chars, ignoring surrounding whitespace).
pub fn parse_key_hex(raw: &str) -> Result<[u8; 32], String> {
    let bytes = hex_decode(raw)?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected 32-byte (64 hex char) key, got {} bytes", v.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_and_crc32_match_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Streaming update matches single-shot
        let mut h = Sha256::new();
        h.update(b"a");
        h.update(b"bc");
        assert_eq!(hex_encode(&h.finalize()), sha256_hex(b"abc"));

        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn aes256_gcm_matches_nist_sp800_38d_test_cases_13_to_16() {
        // NIST SP 800-38D Test Case 13: K=0, IV=0, P=empty, A=empty
        let k0 = [0u8; 32];
        let iv0 = [0u8; 12];
        let ct13 = aes256_gcm_encrypt(&k0, &iv0, b"", b"");
        assert_eq!(hex_encode(&ct13), "530f8afbc74536b9a963b4f1c4cb738b");
        assert_eq!(aes256_gcm_decrypt(&k0, &iv0, b"", &ct13).unwrap(), b"");

        // NIST SP 800-38D Test Case 14: K=0, IV=0, P=16 zero bytes, A=empty
        let ct14 = aes256_gcm_encrypt(&k0, &iv0, b"", &[0u8; 16]);
        assert_eq!(
            hex_encode(&ct14),
            "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"
        );
        assert_eq!(aes256_gcm_decrypt(&k0, &iv0, b"", &ct14).unwrap(), [0u8; 16]);

        // NIST SP 800-38D Test Case 16: non-zero key, IV, plaintext, AND AAD
        let k16: [u8; 32] = hex_decode("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308")
            .unwrap()
            .try_into()
            .unwrap();
        let iv16: [u8; 12] = hex_decode("cafebabefacedbaddecaf888")
            .unwrap()
            .try_into()
            .unwrap();
        let pt16 = hex_decode(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        )
        .unwrap();
        let aad16 = hex_decode("feedfacedeadbeeffeedfacedeadbeefabaddad2").unwrap();
        let ct16 = aes256_gcm_encrypt(&k16, &iv16, &aad16, &pt16);
        let expected_ct = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662";
        let expected_tag = "76fc6ece0f4e1768cddf8853bb2d551b";
        assert_eq!(hex_encode(&ct16), format!("{expected_ct}{expected_tag}"));
        assert_eq!(aes256_gcm_decrypt(&k16, &iv16, &aad16, &ct16).unwrap(), pt16);

        // Tamper check: flipping 1 bit in ciphertext, tag, or AAD fails
        let mut bad_ct = ct16.clone();
        bad_ct[5] ^= 1;
        assert!(aes256_gcm_decrypt(&k16, &iv16, &aad16, &bad_ct).is_err());
        let mut bad_tag = ct16.clone();
        *bad_tag.last_mut().unwrap() ^= 0x80;
        assert!(aes256_gcm_decrypt(&k16, &iv16, &aad16, &bad_tag).is_err());
        assert!(aes256_gcm_decrypt(&k16, &iv16, b"wrong-aad", &ct16).is_err());
    }

    #[test]
    fn key_wrap_unwrap_binds_pack_id() {
        let master = [0x42u8; 32];
        let content = [0x99u8; 32];
        let nonce = [0x11u8; 12];
        let wrapped = wrap_pack_key(&master, "ch01a-yuhang", &content, &nonce);
        assert_eq!(unwrap_pack_key(&master, "ch01a-yuhang", &wrapped).unwrap(), content);
        assert!(unwrap_pack_key(&master, "ch02-suzhou", &wrapped).is_err());
        assert!(unwrap_pack_key(&[0x43u8; 32], "ch01a-yuhang", &wrapped).is_err());
    }
}
