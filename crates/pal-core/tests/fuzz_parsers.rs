//! #273 D11: Zero-dependency deterministic, structure-aware parser fuzzing for untrusted
//! `pal-core` inputs (`load_save_json`, `load_dos_rpg`, `ReplayBundle::from_json`,
//! `sanitize_ai_thought`, `yj1_decompress`, `mkf`/`smkf`, `decode_rle_sprite`).
//!
//! - Seeds corpora from real game snapshots, real `.rpg` saves, and adversarial edge cases.
//! - Uses structure-aware JSON mutations (extreme numbers `-1`, `i64::MIN`, `u64::MAX`, `1e308`,
//!   key drop/dup, type swaps, deep nesting) + byte mutations (bit flip, truncation, splice,
//!   magic-preserving corruption).
//! - Runs every target under a per-input watchdog (heartbeat) so infinite loops (like #265) fail
//!   fast and dump the hanging input, without false timeouts at high iteration counts (#24).
//! - Writes any crashing/panicking input to `.tmp/fuzz_parser_<target>_<hash>.bin`.
//!
//! Override iteration budget via `PAL_PARSER_FUZZ_ITERS` (default `250` per target in PR).

mod common;

use common::{assets_dir, NativeHost};
use pal_core::game::replay_rec::{sanitize_ai_thought, ReplayBundle, MAX_AI_THOUGHT_CHARS};
use pal_core::graphics::{decode_rle_sprite, Palette};
use pal_core::mkf::{mkf_chunk, mkf_chunk_count, smkf_frame, smkf_frame_count};
use pal_core::yj1::yj1_decompress;
use serde_json::{json, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct MutRng(u64);

impl MutRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() as usize) % n }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn dump_parser_failure(target: &str, bytes: &[u8]) -> std::path::PathBuf {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp");
    let _ = std::fs::create_dir_all(&out_dir);
    let hash = fnv1a64(bytes);
    let path = out_dir.join(format!("fuzz_parser_{target}_{hash:016x}.bin"));
    let _ = std::fs::write(&path, bytes);
    path
}

fn mutate_bytes(seed: &[u8], rng: &mut MutRng, preserve_magic: usize) -> Vec<u8> {
    let mut buf = seed.to_vec();
    if buf.is_empty() {
        let len = 1 + rng.range(64);
        return (0..len).map(|_| rng.next_u64() as u8).collect();
    }
    let ops = 1 + rng.range(4);
    for _ in 0..ops {
        if buf.is_empty() {
            break;
        }
        let min_off = preserve_magic.min(buf.len().saturating_sub(1));
        match rng.range(6) {
            0 => {
                // Bit flip
                let idx = min_off + rng.range(buf.len() - min_off);
                buf[idx] ^= 1u8 << (rng.next_u64() & 7);
            }
            1 => {
                // Interesting integer overwrite (0, 0xFF, 0x7FFF, 0xFFFF, 0x7FFFFFFF)
                let idx = min_off + rng.range(buf.len() - min_off);
                let vals: [&[u8]; 5] = [
                    &[0, 0],
                    &[0xFF, 0xFF],
                    &[0xFF, 0x7F],
                    &[0xFF, 0xFF, 0xFF, 0x7F],
                    &[0xFF, 0xFF, 0xFF, 0xFF],
                ];
                let v = vals[rng.range(vals.len())];
                for (k, &b) in v.iter().enumerate() {
                    if idx + k < buf.len() {
                        buf[idx + k] = b;
                    }
                }
            }
            2 => {
                // Truncate
                let new_len = rng.range(buf.len());
                buf.truncate(new_len);
            }
            3 => {
                // Append random tail (bounded to 16 KiB)
                if buf.len() < 16_384 {
                    let add = 1 + rng.range(64);
                    for _ in 0..add {
                        buf.push(rng.next_u64() as u8);
                    }
                }
            }
            4 => {
                // Splice / duplicate slice
                if buf.len() < 16_384 && buf.len() > 4 {
                    let s = rng.range(buf.len() - 2);
                    let l = (1 + rng.range(32)).min(buf.len() - s);
                    let chunk = buf[s..s + l].to_vec();
                    let dst = rng.range(buf.len());
                    buf.splice(dst..dst, chunk);
                }
            }
            _ => {
                // Delete slice
                if buf.len() > min_off + 2 {
                    let s = min_off + rng.range(buf.len() - min_off);
                    let l = (1 + rng.range(32)).min(buf.len() - s);
                    buf.drain(s..s + l);
                }
            }
        }
    }
    buf
}

fn mutate_json_value(v: &mut Value, rng: &mut MutRng, depth: usize) {
    if depth > 12 {
        return;
    }
    match v {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            if keys.is_empty() {
                map.insert("fuzz".into(), json!(u64::MAX));
                return;
            }
            let k = &keys[rng.range(keys.len())];
            match rng.range(6) {
                0 => {
                    map.remove(k);
                }
                1 => {
                    map.insert(k.clone(), extreme_json_scalar(rng));
                }
                2 => {
                    // Deep nesting
                    let mut nested = json!(1);
                    for _ in 0..24 {
                        nested = json!({"a": [nested]});
                    }
                    map.insert(k.clone(), nested);
                }
                3 => {
                    map.insert(format!("{k}_extra"), extreme_json_scalar(rng));
                }
                _ => {
                    if let Some(child) = map.get_mut(k) {
                        mutate_json_value(child, rng, depth + 1);
                    }
                }
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                arr.push(extreme_json_scalar(rng));
                return;
            }
            let idx = rng.range(arr.len());
            match rng.range(5) {
                0 => {
                    arr.remove(idx);
                }
                1 => {
                    let clone = arr[idx].clone();
                    if arr.len() < 128 {
                        arr.push(clone);
                    }
                }
                2 => {
                    arr[idx] = extreme_json_scalar(rng);
                }
                _ => mutate_json_value(&mut arr[idx], rng, depth + 1),
            }
        }
        _ => {
            *v = extreme_json_scalar(rng);
        }
    }
}

fn extreme_json_scalar(rng: &mut MutRng) -> Value {
    match rng.range(10) {
        0 => json!(-1),
        1 => json!(i64::MIN),
        2 => json!(u64::MAX),
        3 => json!(1e308_f64),
        4 => json!(0),
        5 => json!(null),
        6 => json!(true),
        7 => json!(""),
        8 => json!("../../etc/passwd\0\u{202E}😈"),
        _ => json!([1, -999999999i64, u64::MAX]),
    }
}

/// Progress beacon shared with the watchdog (#24): each target calls [`Heartbeat::beat`] before
/// every input, so a genuine per-input hang is told apart from "many iterations" and the offending
/// input can be dumped for reproduction.
#[derive(Clone, Default)]
struct Heartbeat(Arc<HeartbeatInner>);

#[derive(Default)]
struct HeartbeatInner {
    beats: AtomicU64,
    current: Mutex<(String, Vec<u8>)>,
}

impl Heartbeat {
    fn beat(&self, label: &str, input: &[u8]) {
        *self.0.current.lock().unwrap() = (label.to_string(), input.to_vec());
        self.0.beats.fetch_add(1, Ordering::Release);
    }
}

/// No single input may run longer than this.
const PER_INPUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a fuzz target on a worker thread. Fails on a panic, when one input stalls for
/// [`PER_INPUT_TIMEOUT`] (dumping that input), or when the whole run exceeds `base_timeout`
/// plus a per-iteration allowance (catches pathological slowness at high iteration counts).
fn run_with_watchdog<F>(target: &'static str, base_timeout: Duration, iters: usize, f: F)
where
    F: FnOnce(Heartbeat) + Send + 'static,
{
    let hb = Heartbeat::default();
    let worker_hb = hb.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let res = catch_unwind(AssertUnwindSafe(|| f(worker_hb)));
        let _ = tx.send(res);
    });
    let total_cap = base_timeout + Duration::from_millis(20) * iters as u32;
    let started = Instant::now();
    let mut last_beats = 0u64;
    let mut last_progress = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(())) => return,
            Ok(Err(payload)) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("non-string panic");
                panic!("[fuzz_parsers:{target}] panicked: {msg}");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("[fuzz_parsers:{target}] worker thread exited without a result");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let beats = hb.0.beats.load(Ordering::Acquire);
        if beats != last_beats {
            last_beats = beats;
            last_progress = Instant::now();
        }
        if last_progress.elapsed() > PER_INPUT_TIMEOUT {
            let (label, input) = hb.0.current.lock().unwrap().clone();
            let path = dump_parser_failure(&format!("{target}_{label}_hang"), &input);
            panic!(
                "[fuzz_parsers:{target}] WATCHDOG: input #{beats} ({label}) ran > {PER_INPUT_TIMEOUT:?} \
                 (infinite loop / OOM?); dumped to {}",
                path.display()
            );
        }
        if started.elapsed() > total_cap {
            panic!(
                "[fuzz_parsers:{target}] WATCHDOG: {iters} iterations exceeded {total_cap:?} \
                 ({beats} inputs done; no single input stalled)"
            );
        }
    }
}

fn iters_budget() -> usize {
    std::env::var("PAL_PARSER_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250)
}

#[test]
fn fuzz_parser_load_save_json_and_replay_bundle_from_json() {
    let iters = iters_budget();
    run_with_watchdog("save_and_replay_json", Duration::from_secs(30), iters, move |hb| {
        let mut host = NativeHost::new();
        host.reset_new_game();
        host.enter_scene(1, false);
        let seed_save = host.game.save_json("seed", "2026-11-24T00:00:00Z");
        let seed_replay = host.game.export_replay_json();

        let mut rng = MutRng::new(0x5A7E_0001);
        for i in 0..iters {
            // 1. Structure-aware mutated save_json
            let mut v = seed_save.clone();
            let steps = 1 + rng.range(4);
            for _ in 0..steps {
                mutate_json_value(&mut v, &mut rng, 0);
            }
            hb.beat("save_json", v.to_string().as_bytes());
            let res = catch_unwind(AssertUnwindSafe(|| {
                let _ = host.game.load_save_json(&v);
            }));
            if res.is_err() {
                let p = dump_parser_failure("load_save_json", v.to_string().as_bytes());
                panic!("load_save_json panicked at iter {i}; dumped to {}", p.display());
            }

            // 2. Structure-aware mutated ReplayBundle JSON (including huge ticks / segments)
            let mut rv = seed_replay.clone();
            if i % 5 == 0 {
                rv["total_ticks"] = json!(u64::MAX);
                if let Some(seg0) = rv["segments"].get_mut(0) {
                    seg0["duration_ticks"] = json!(u64::MAX);
                    seg0["start_tick"] = json!(u64::MAX);
                }
            } else {
                for _ in 0..(1 + rng.range(4)) {
                    mutate_json_value(&mut rv, &mut rng, 0);
                }
            }
            hb.beat("replay_json", rv.to_string().as_bytes());
            let res = catch_unwind(AssertUnwindSafe(|| {
                let _ = ReplayBundle::from_json(&rv);
            }));
            if res.is_err() {
                let p = dump_parser_failure("replay_bundle_json", rv.to_string().as_bytes());
                panic!(
                    "ReplayBundle::from_json panicked at iter {i}; dumped to {}",
                    p.display()
                );
            }
        }
    });
}

#[test]
fn fuzz_parser_load_dos_rpg() {
    let iters = iters_budget();
    run_with_watchdog("load_dos_rpg", Duration::from_secs(30), iters, move |hb| {
        let mut host = NativeHost::new();
        let saves_dir = assets_dir().join("orig_saves");
        let mut seeds = Vec::new();
        for f in ["0.rpg", "10.rpg", "45.rpg"] {
            if let Ok(b) = std::fs::read(saves_dir.join(f)) {
                seeds.push(b);
            }
        }
        seeds.push(vec![]);
        seeds.push(vec![0u8; 64]);

        let mut rng = MutRng::new(0xD05_5A7E);
        for i in 0..iters {
            let base = &seeds[rng.range(seeds.len())];
            let mutated = mutate_bytes(base, &mut rng, 0);
            hb.beat("rpg", &mutated);
            let res = catch_unwind(AssertUnwindSafe(|| {
                let _ = host.game.load_dos_rpg(&mutated);
            }));
            if res.is_err() {
                let p = dump_parser_failure("load_dos_rpg", &mutated);
                panic!("load_dos_rpg panicked at iter {i}; dumped to {}", p.display());
            }
        }
    });
}

#[test]
fn fuzz_parser_yj1_mkf_rle_and_sanitize_ai_thought() {
    let iters = iters_budget().max(400);
    run_with_watchdog("yj1_mkf_rle_thought", Duration::from_secs(20), iters, move |hb| {
        let mut rng = MutRng::new(0x71_4B_F00D);
        let pal = Palette::from_vga6(&vec![32u8; 768]).unwrap();

        // Real YJ_1 header seed + adversarial headers (huge uncomp_len, cyclic tree, 0 block)
        let yj1_seeds: Vec<Vec<u8>> = vec![
            b"YJ_1\x10\x00\x00\x00\x18\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            b"YJ_1\xFF\xFF\xFF\x7F\x20\x00\x00\x00\x01\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            b"YJ_1\x40\x00\x00\x00\x40\x00\x00\x00\x02\x00\x08\x00\x01\x02\x03\x04\x05\x06\x07\x08".to_vec(),
        ];

        for i in 0..iters {
            // 1. YJ1 decompressor (preserve 4-byte "YJ_1" magic on 75% of inputs to reach deep loops)
            let base = &yj1_seeds[rng.range(yj1_seeds.len())];
            let keep = if rng.range(4) == 0 { 0 } else { 4 };
            let yj = mutate_bytes(base, &mut rng, keep);
            hb.beat("yj1", &yj);
            let res = catch_unwind(AssertUnwindSafe(|| {
                let _ = yj1_decompress(&yj);
            }));
            if res.is_err() {
                let p = dump_parser_failure("yj1", &yj);
                panic!("yj1_decompress panicked at iter {i}; dumped to {}", p.display());
            }

            // 2. MKF & SMKF parsers
            let mkf_buf = mutate_bytes(&yj, &mut rng, 0);
            hb.beat("mkf_rle", &mkf_buf);
            let res = catch_unwind(AssertUnwindSafe(|| {
                let n = mkf_chunk_count(&mkf_buf).min(32);
                for k in 0..n {
                    let _ = mkf_chunk(&mkf_buf, k);
                }
                let _ = mkf_chunk(&mkf_buf, usize::MAX);
                let sn = smkf_frame_count(&mkf_buf).min(32);
                for k in 0..sn {
                    let _ = smkf_frame(&mkf_buf, k);
                }
                let _ = smkf_frame(&mkf_buf, usize::MAX);
            }));
            if res.is_err() {
                let p = dump_parser_failure("mkf", &mkf_buf);
                panic!("mkf/smkf parser panicked at iter {i}; dumped to {}", p.display());
            }

            // 3. RLE sprite decoder
            let res = catch_unwind(AssertUnwindSafe(|| {
                let _ = decode_rle_sprite(&mkf_buf, &pal);
            }));
            if res.is_err() {
                let p = dump_parser_failure("rle", &mkf_buf);
                panic!("decode_rle_sprite panicked at iter {i}; dumped to {}", p.display());
            }

            // 4. sanitize_ai_thought (must strip emoji/bidi/controls and enforce char cap)
            let alphabet = [
                'a', '中', '文', ' ', '\n', '\r', '\t', '\0', ';', '"', '\'',
                '\u{202E}', '\u{2066}', '\u{200D}', '\u{FE0F}', '🚀', '💭', '⚔',
                '\u{20000}', // CJK Ext B (must NOT crash)
            ];
            let len = rng.range(320);
            let mut raw = String::with_capacity(len * 3);
            for _ in 0..len {
                raw.push(alphabet[rng.range(alphabet.len())]);
            }
            hb.beat("thought", raw.as_bytes());
            let (cleaned, _trunc) = sanitize_ai_thought(&raw);
            assert!(
                cleaned.chars().count() <= MAX_AI_THOUGHT_CHARS,
                "sanitize_ai_thought exceeded max chars: {}",
                cleaned.chars().count()
            );
            assert!(
                !cleaned.chars().any(|c| c.is_control() || matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069)),
                "sanitize_ai_thought leaked control/bidi char in: {cleaned:?}"
            );
        }
    });
}
