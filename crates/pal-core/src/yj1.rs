//! 1995 DOS `YJ_1` Huffman + LZSS bitstream decompressor.

const MAX_UNCOMP_LEN: usize = 16 * 1024 * 1024;
const MAX_PREALLOC: usize = 65_536;

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> Result<u16, &'static str> {
    let end = off.checked_add(2).ok_or("YJ_1 offset overflow (u16)")?;
    let b = data.get(off..end).ok_or("YJ_1 unexpected EOF (u16)")?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> Result<u32, &'static str> {
    let end = off.checked_add(4).ok_or("YJ_1 offset overflow (u32)")?;
    let b = data.get(off..end).ok_or("YJ_1 unexpected EOF (u32)")?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[inline]
fn get_bits(buf: &[u8], bit_ptr: &mut usize, count: u8) -> Result<u32, &'static str> {
    if count == 0 {
        return Ok(0);
    }
    if count > 32 {
        return Err("YJ_1 invalid bit count");
    }
    let mut val = 0u32;
    let mut bp = *bit_ptr;
    for _ in 0..count {
        let word_idx = (bp >> 4)
            .checked_mul(2)
            .ok_or("YJ_1 bit pointer overflow")?;
        let bit_off = 15 - (bp & 15);
        let pair = buf
            .get(word_idx..word_idx + 2)
            .ok_or("YJ_1 unexpected bit EOF")?;
        let w = (pair[0] as u16) | ((pair[1] as u16) << 8);
        val = (val << 1) | (((w >> bit_off) & 1) as u32);
        bp = bp.checked_add(1).ok_or("YJ_1 bit pointer overflow")?;
    }
    *bit_ptr = bp;
    Ok(val)
}

/// Decompresses a `YJ_1`-compressed buffer.
pub fn yj1_decompress(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() < 16 || &data[0..4] != b"YJ_1" {
        return Err("Missing YJ_1 magic header");
    }

    let uncomp_len = read_u32_le(data, 4)? as usize;
    if uncomp_len > MAX_UNCOMP_LEN {
        return Err("YJ_1 uncompressed length exceeds limit");
    }
    let _comp_len = read_u32_le(data, 8)? as usize;
    let block_count = read_u16_le(data, 12)? as usize;
    let tree_len = data[15] as usize;
    if tree_len == 0 {
        return Err("YJ_1 empty Huffman tree");
    }

    let num_nodes = tree_len * 2;
    let flag_bytes_off = 16usize
        .checked_add(num_nodes)
        .ok_or("YJ_1 Huffman table overflow")?;
    let flag_byte_len = ((num_nodes + 15) >> 4) * 2;
    let data_start = flag_bytes_off
        .checked_add(flag_byte_len)
        .ok_or("YJ_1 Huffman table overflow")?;
    if data_start > data.len() {
        return Err("YJ_1 Huffman table truncated");
    }
    let tree_vals = &data[16..flag_bytes_off];

    let mut is_leaf = vec![false; num_nodes + 1];
    let mut value = vec![0u8; num_nodes + 1];
    let mut left = vec![0usize; num_nodes + 1];
    let mut right = vec![0usize; num_nodes + 1];

    let mut flag_bit_ptr = 0usize;
    let flag_slice = &data[flag_bytes_off..data_start];
    for i in 1..=num_nodes {
        is_leaf[i] = get_bits(flag_slice, &mut flag_bit_ptr, 1)? == 0;
        value[i] = tree_vals[i - 1];
    }

    let mut nxt = 1usize;
    for i in 0..=num_nodes {
        if !is_leaf[i] {
            if nxt + 1 > num_nodes || nxt <= i {
                return Err("YJ_1 malformed Huffman tree");
            }
            left[i] = nxt;
            right[i] = nxt + 1;
            nxt += 2;
        }
    }

    let mut pos = data_start;
    let mut out = Vec::with_capacity(uncomp_len.min(MAX_PREALLOC));

    for _ in 0..block_count {
        if pos >= data.len() || out.len() >= uncomp_len {
            break;
        }
        let u_len = read_u16_le(data, pos)? as usize;
        let c_len_off = pos.checked_add(2).ok_or("YJ_1 block offset overflow")?;
        let c_len = read_u16_le(data, c_len_off)? as usize;
        if c_len == 0 {
            let payload_start = pos.checked_add(4).ok_or("YJ_1 raw block offset overflow")?;
            let raw_end = payload_start
                .checked_add(u_len)
                .ok_or("YJ_1 raw block offset overflow")?;
            if raw_end > data.len() {
                return Err("YJ_1 raw block truncated");
            }
            if u_len > uncomp_len - out.len() {
                return Err("YJ_1 raw block exceeds uncompressed length");
            }
            out.extend_from_slice(&data[payload_start..raw_end]);
            pos = raw_end;
            continue;
        }
        if c_len < 24 {
            return Err("YJ_1 compressed block header too small");
        }
        let hdr_end = pos
            .checked_add(24)
            .ok_or("YJ_1 compressed block offset overflow")?;
        let blk_end_pos = pos
            .checked_add(c_len)
            .ok_or("YJ_1 compressed block offset overflow")?;
        if hdr_end > data.len() || blk_end_pos > data.len() {
            return Err("YJ_1 compressed block truncated");
        }

        let lz_rep = [
            read_u16_le(data, pos + 4)? as usize,
            read_u16_le(data, pos + 6)? as usize,
            read_u16_le(data, pos + 8)? as usize,
            read_u16_le(data, pos + 10)? as usize,
        ];
        let lz_off_clen = [
            data[pos + 12],
            data[pos + 13],
            data[pos + 14],
            data[pos + 15],
        ];
        let lz_rep_clen = [data[pos + 16], data[pos + 17], data[pos + 18]];
        let cc_clen = [data[pos + 19], data[pos + 20], data[pos + 21]];
        let cc_tbl = [data[pos + 22] as usize, data[pos + 23] as usize];

        let bdata = &data[hdr_end..blk_end_pos];
        pos = blk_end_pos;
        let mut st = 0usize;

        let get_loop = |st: &mut usize| -> Result<usize, &'static str> {
            if get_bits(bdata, st, 1)? != 0 {
                return Ok(cc_tbl[0]);
            }
            let tmp = get_bits(bdata, st, 2)? as usize;
            if tmp != 0 {
                Ok(get_bits(bdata, st, cc_clen[tmp - 1])? as usize)
            } else {
                Ok(cc_tbl[1])
            }
        };

        let get_count = |st: &mut usize| -> Result<usize, &'static str> {
            let tmp = get_bits(bdata, st, 2)? as usize;
            if tmp == 0 {
                return Ok(lz_rep[0]);
            }
            if get_bits(bdata, st, 1)? != 0 {
                Ok(get_bits(bdata, st, lz_rep_clen[tmp - 1])? as usize)
            } else {
                Ok(lz_rep[tmp])
            }
        };

        let blk_end = out.len().saturating_add(u_len).min(uncomp_len);
        while out.len() < blk_end {
            let lp = get_loop(&mut st)?;
            if lp == 0 {
                break;
            }
            for _ in 0..lp {
                let mut node = 0usize;
                let mut steps = 0usize;
                while !is_leaf[node] {
                    if steps > num_nodes {
                        return Err("YJ_1 Huffman tree cycle");
                    }
                    if get_bits(bdata, &mut st, 1)? != 0 {
                        node = right[node];
                    } else {
                        node = left[node];
                    }
                    steps += 1;
                }
                out.push(value[node]);
                if out.len() >= blk_end {
                    break;
                }
            }
            if out.len() >= blk_end {
                break;
            }
            let lp = get_loop(&mut st)?;
            if lp == 0 {
                break;
            }
            for _ in 0..lp {
                let cnt = get_count(&mut st)?;
                let tmp = get_bits(bdata, &mut st, 2)? as usize;
                let off = get_bits(bdata, &mut st, lz_off_clen[tmp])? as usize;
                if off == 0 || off > out.len() {
                    return Err("YJ_1 invalid LZSS back-reference offset");
                }
                for _ in 0..cnt {
                    let b = out[out.len() - off];
                    out.push(b);
                    if out.len() >= blk_end {
                        break;
                    }
                }
                if out.len() >= blk_end {
                    break;
                }
            }
        }
    }

    if out.len() != uncomp_len {
        return Err("YJ_1 truncated output");
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yj1_decompress_valid_compressed_and_raw() {
        assert_eq!(yj1_decompress(&[]).unwrap(), Vec::<u8>::new());

        // Authentic RNG.MKF[0][10] 50-byte compressed chunk -> [4, 255, 124, 0]
        let real_rng_chunk: [u8; 50] = [
            0x59, 0x4a, 0x5f, 0x31, 0x04, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x0d, 0x03, 0x01, 0x02, 0xff, 0x7c, 0x04, 0x00, 0x00, 0xc0, 0x04, 0x00, 0x1a, 0x00,
            0xff, 0x3f, 0xff, 0x3f, 0xff, 0x3f, 0xff, 0x3f, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x04, 0xff, 0x90, 0xc3,
        ];
        assert_eq!(
            yj1_decompress(&real_rng_chunk).unwrap(),
            vec![4, 255, 124, 0]
        );

        // Synthetic valid raw block (c_len == 0)
        let mut raw = Vec::new();
        raw.extend_from_slice(b"YJ_1");
        raw.extend_from_slice(&4u32.to_le_bytes()); // uncomp_len = 4
        raw.extend_from_slice(&28u32.to_le_bytes()); // comp_len
        raw.extend_from_slice(&1u16.to_le_bytes()); // block_count = 1
        raw.push(0);
        raw.push(1); // tree_len = 1 (num_nodes = 2)
        raw.extend_from_slice(&[0x41, 0x42]); // tree_vals
        raw.extend_from_slice(&[0x00, 0x00]); // flags
        raw.extend_from_slice(&4u16.to_le_bytes()); // u_len = 4
        raw.extend_from_slice(&0u16.to_le_bytes()); // c_len = 0 (raw)
        raw.extend_from_slice(b"TEST");
        assert_eq!(yj1_decompress(&raw).unwrap(), b"TEST");
    }

    #[test]
    fn test_yj1_rejects_c_len_lt_24_without_panic() {
        let mut data = Vec::new();
        data.extend_from_slice(b"YJ_1");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&44u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.push(0);
        data.push(1);
        data.extend_from_slice(&[0x41, 0x42]);
        data.extend_from_slice(&[0x00, 0x00]);
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // c_len = 1 < 24
        data.extend_from_slice(&[0u8; 20]);
        assert!(yj1_decompress(&data).is_err());
    }

    #[test]
    fn test_yj1_rejects_zero_tree_len_without_loop() {
        let mut data = Vec::new();
        data.extend_from_slice(b"YJ_1");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.push(0);
        data.push(0); // tree_len = 0
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&[0u8; 18]);
        data.extend_from_slice(&[1, 1]);
        assert!(yj1_decompress(&data).is_err());
    }

    #[test]
    fn test_yj1_rejects_cyclic_huffman_tree_without_loop() {
        let mut data = Vec::new();
        data.extend_from_slice(b"YJ_1");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&44u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.push(0);
        data.push(1);
        data.extend_from_slice(&[0x41, 0x42]);
        data.extend_from_slice(&[0x00, 0xFF]); // all internal -> cycle
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&[0u8; 18]);
        data.extend_from_slice(&[1, 1]);
        assert!(yj1_decompress(&data).is_err());
    }

    #[test]
    fn test_yj1_rejects_excessive_uncomp_len_without_oom() {
        let mut data = Vec::new();
        data.extend_from_slice(b"YJ_1");
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        data.extend_from_slice(&20u32.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.push(0);
        data.push(1);
        data.extend_from_slice(&[0x41, 0x42]);
        data.extend_from_slice(&[0x00, 0x00]);
        assert!(yj1_decompress(&data).is_err());
    }

    #[test]
    fn test_yj1_rejects_truncated_flags_and_bit_eof() {
        // Case 1: truncated flag slice
        let mut trunc_flags = Vec::new();
        trunc_flags.extend_from_slice(b"YJ_1");
        trunc_flags.extend_from_slice(&1u32.to_le_bytes());
        trunc_flags.extend_from_slice(&18u32.to_le_bytes());
        trunc_flags.extend_from_slice(&1u16.to_le_bytes());
        trunc_flags.push(0);
        trunc_flags.push(1);
        trunc_flags.extend_from_slice(&[0x41, 0x42]);
        assert!(yj1_decompress(&trunc_flags).is_err());

        // Case 2: bit EOF in empty bdata (c_len = 24)
        let mut bit_eof = Vec::new();
        bit_eof.extend_from_slice(b"YJ_1");
        bit_eof.extend_from_slice(&1u32.to_le_bytes());
        bit_eof.extend_from_slice(&44u32.to_le_bytes());
        bit_eof.extend_from_slice(&1u16.to_le_bytes());
        bit_eof.push(0);
        bit_eof.push(1);
        bit_eof.extend_from_slice(&[0x41, 0x42]);
        bit_eof.extend_from_slice(&[0x00, 0x00]);
        bit_eof.extend_from_slice(&1u16.to_le_bytes());
        bit_eof.extend_from_slice(&24u16.to_le_bytes());
        bit_eof.extend_from_slice(&[0u8; 18]);
        bit_eof.extend_from_slice(&[1, 1]);
        assert!(yj1_decompress(&bit_eof).is_err());
    }

    #[test]
    fn test_yj1_rejects_raw_block_overflow_and_truncated_stream() {
        // Case 1: raw block payload larger than uncomp_len
        let mut over = Vec::new();
        over.extend_from_slice(b"YJ_1");
        over.extend_from_slice(&1u32.to_le_bytes());
        over.extend_from_slice(&28u32.to_le_bytes());
        over.extend_from_slice(&1u16.to_le_bytes());
        over.push(0);
        over.push(1);
        over.extend_from_slice(&[0x41, 0x42]);
        over.extend_from_slice(&[0x00, 0x00]);
        over.extend_from_slice(&4u16.to_le_bytes());
        over.extend_from_slice(&0u16.to_le_bytes());
        over.extend_from_slice(&[1, 2, 3, 4]);
        assert!(yj1_decompress(&over).is_err());

        // Case 2: uncomp_len = 4, block_count = 0 (truncated output)
        let mut trunc = Vec::new();
        trunc.extend_from_slice(b"YJ_1");
        trunc.extend_from_slice(&4u32.to_le_bytes());
        trunc.extend_from_slice(&20u32.to_le_bytes());
        trunc.extend_from_slice(&0u16.to_le_bytes());
        trunc.push(0);
        trunc.push(1);
        trunc.extend_from_slice(&[0x41, 0x42]);
        trunc.extend_from_slice(&[0x00, 0x00]);
        assert!(yj1_decompress(&trunc).is_err());
    }
}
