//! Unified line-oriented transport (`ChannelServer`) supporting both stdio and TCP sockets.
//!
//! Shared identically by headless `--text` (`--listen` / stdio) and live GUI+Text (`--gui --text` / `--listen`).

use crate::text_mode::engine::TextReply;
use crate::text_mode::response::{format_plain_response, format_prompt};
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, ErrorKind, IsTerminal, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum concurrent TCP clients served by `ChannelServer::start_tcp` (#202).
const MAX_TCP_CLIENTS: usize = 16;
/// Maximum concurrent TCP connections allowed from a single peer IP (#262).
const MAX_CLIENTS_PER_IP: usize = 8;
/// Maximum concurrent unauthenticated remote TCP clients so multi-IP floods can never
/// exhaust all 16 slots and lock out authenticated clients (#271).
const MAX_UNAUTH_TCP_CLIENTS: usize = 4;
/// Maximum bytes accepted in a single TCP command line before rejecting & closing (#202).
const MAX_LINE_BYTES: usize = 64 * 1024;
/// Maximum time a remote peer may take to complete `auth <token>` before disconnect (#262).
const AUTH_DEADLINE: Duration = Duration::from_secs(10);
/// Maximum idle time (no complete command line received) before closing a TCP client (#262).
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

static NEXT_TCP_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// Provenance of an incoming command, used for per-client notice cursors (#233)
/// and socket/remote security policy enforcement (#193).
pub use crate::text_mode::ClientOrigin;

struct ClientSlotGuard {
    total: Arc<AtomicUsize>,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    ip: IpAddr,
}

/// RAII guard for the unauthenticated remote-client cap (`MAX_UNAUTH_TCP_CLIENTS`, #271).
/// Dropped immediately once `auth <token>` succeeds or when the connection closes.
struct UnauthSlotGuard {
    unauth: Arc<AtomicUsize>,
}

impl Drop for UnauthSlotGuard {
    fn drop(&mut self) {
        self.unauth.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for ClientSlotGuard {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::AcqRel);
        if let Ok(mut map) = self.per_ip.lock() {
            if let Some(c) = map.get_mut(&self.ip) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    map.remove(&self.ip);
                }
            }
        }
    }
}

/// Detect browser/HTTP cross-protocol requests sent to the raw line-oriented TCP socket (#193).
pub fn is_http_probe(line: &str) -> bool {
    let trimmed = line.trim_start();
    let upper = trimmed.to_ascii_uppercase();
    const HTTP_VERBS: [&str; 9] = [
        "GET ",
        "POST ",
        "PUT ",
        "DELETE ",
        "HEAD ",
        "OPTIONS ",
        "PATCH ",
        "CONNECT ",
        "TRACE ",
    ];
    HTTP_VERBS.iter().any(|v| upper.starts_with(v))
        || upper.contains("HTTP/1.")
        || upper.contains("HTTP/2")
}

/// Constant-time byte comparison for socket auth tokens (#193).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Generate a 128-bit hex session token for non-loopback TCP servers (#193).
fn generate_session_token() -> String {
    let mut bytes = [0u8; 16];
    #[cfg(unix)]
    let filled = File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    #[cfg(not(unix))]
    let filled = false;
    if !filled {
        let mut h1 = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        h1.write_u128(nanos);
        h1.write_u32(std::process::id());
        let a = h1.finish();
        let mut h2 = RandomState::new().build_hasher();
        h2.write_u64(a.rotate_left(17));
        h2.write_usize(&bytes as *const _ as usize);
        let b = h2.finish();
        bytes[..8].copy_from_slice(&a.to_le_bytes());
        bytes[8..].copy_from_slice(&b.to_le_bytes());
    }
    let mut s = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

enum BoundedLine {
    Ok(String),
    Eof,
    TooLong,
    Timeout,
    Err,
}

/// Read a single newline-delimited line up to `max_bytes`, preserving partial bytes in `pending`
/// across `WouldBlock`/`TimedOut` read timeouts so chunked writes are never split into bogus
/// command fragments (#202, #229).
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    max_bytes: usize,
) -> BoundedLine {
    loop {
        match reader.fill_buf() {
            Ok([]) => {
                if pending.is_empty() {
                    return BoundedLine::Eof;
                }
                if pending.ends_with(b"\r") {
                    pending.pop();
                }
                let raw = std::mem::take(pending);
                return match String::from_utf8(raw) {
                    Ok(s) => BoundedLine::Ok(s),
                    Err(_) => BoundedLine::Err,
                };
            }
            Ok(chunk) => {
                if let Some(nl_pos) = chunk.iter().position(|&b| b == b'\n') {
                    if pending.len() + nl_pos > max_bytes {
                        reader.consume(nl_pos + 1);
                        pending.clear();
                        return BoundedLine::TooLong;
                    }
                    pending.extend_from_slice(&chunk[..nl_pos]);
                    reader.consume(nl_pos + 1);
                    if pending.ends_with(b"\r") {
                        pending.pop();
                    }
                    let raw = std::mem::take(pending);
                    return match String::from_utf8(raw) {
                        Ok(s) => BoundedLine::Ok(s),
                        Err(_) => BoundedLine::Err,
                    };
                }
                let len = chunk.len();
                if pending.len() + len > max_bytes {
                    reader.consume(len);
                    pending.clear();
                    return BoundedLine::TooLong;
                }
                pending.extend_from_slice(chunk);
                reader.consume(len);
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(ref e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return BoundedLine::Timeout;
            }
            Err(_) => {
                pending.clear();
                return BoundedLine::Err;
            }
        }
    }
}

/// Incoming line request sent from a transport thread (TCP client or stdin) to the simulation thread.
pub struct IncomingCmd {
    pub raw: String,
    pub interactive_blank: bool,
    pub origin: ClientOrigin,
    pub reply_tx: Sender<TextReply>,
}

/// Active stdio or TCP command channel feeding [`IncomingCmd`] into either headless or GUI loops.
pub struct ChannelServer {
    pub rx: Receiver<IncomingCmd>,
    pub json_mode: bool,
    local_addr: Option<SocketAddr>,
    auth_token: Option<String>,
    is_batch: bool,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    exit_code: Arc<AtomicI32>,
}

impl Drop for ChannelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl ChannelServer {
    /// Returns `true` when a finite transport (stdio EOF/quit or batch queue) has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Returns `true` when this channel was started via [`ChannelServer::start_batch`] (`-c` / `--script`).
    pub fn is_batch(&self) -> bool {
        self.is_batch
    }

    /// Returns the process exit code recorded by a finished batch channel (`0` = ok, `1` = error, `2` = golden fail).
    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Relaxed)
    }

    /// Returns the bound local socket address when started via [`ChannelServer::start_tcp`].
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// Returns the per-session authentication token required for non-loopback TCP clients (#193).
    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    /// Start a line-oriented TCP socket server (`--listen <IP:PORT>`).
    /// Supports multiple sequential or concurrent clients; `quit` disconnects only that client.
    /// Non-loopback bindings require `auth <token>` as the first line from non-loopback peers (#193).
    pub fn start_tcp(
        addr: &str,
        json_mode: bool,
        record_file: Option<PathBuf>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(addr).map_err(|e| format!("bind {addr}: {e}"))?;
        let local_addr = listener.local_addr().ok();
        let _ = listener.set_nonblocking(true);
        let auth_token = if local_addr.is_some_and(|a| !a.ip().is_loopback()) {
            let tok = generate_session_token();
            eprintln!(
                "[repal] Text Mode Socket Server listening on {addr} (format={}, LAN auth required)",
                if json_mode { "json" } else { "plain" }
            );
            eprintln!("[repal] Auth token for remote clients: {tok} (send 'auth {tok}' as first line)");
            Some(tok)
        } else {
            eprintln!(
                "[repal] Text Mode Socket Server listening on {addr} (format={})",
                if json_mode { "json" } else { "plain" }
            );
            None
        };
        let (tx, rx) = mpsc::channel::<IncomingCmd>();
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(AtomicI32::new(0));
        let stop_bg = stop.clone();
        let addr_owned = addr.to_string();
        let recorder = Arc::new(Mutex::new(open_record_file(record_file)));
        let active_clients = Arc::new(AtomicUsize::new(0));
        let unauth_clients = Arc::new(AtomicUsize::new(0));
        let per_ip_clients: Arc<Mutex<HashMap<IpAddr, usize>>> = Arc::new(Mutex::new(HashMap::new()));
        let required_token = auth_token.clone();

        thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, peer_addr)) => {
                        let prev = active_clients.fetch_add(1, Ordering::AcqRel);
                        if prev >= MAX_TCP_CLIENTS {
                            active_clients.fetch_sub(1, Ordering::AcqRel);
                            let mut w = stream;
                            let _ = writeln!(w, "{{\"ok\":false,\"error\":\"too many concurrent clients\"}}");
                            let _ = w.shutdown(Shutdown::Both);
                            continue;
                        }
                        let peer_ip = peer_addr.ip();
                        let peer_is_loopback = peer_ip.is_loopback();
                        let token_for_conn = if !peer_is_loopback {
                            required_token.clone()
                        } else {
                            None
                        };
                        let unauth_guard = if token_for_conn.is_some() {
                            let prev_u = unauth_clients.fetch_add(1, Ordering::AcqRel);
                            if prev_u >= MAX_UNAUTH_TCP_CLIENTS {
                                unauth_clients.fetch_sub(1, Ordering::AcqRel);
                                active_clients.fetch_sub(1, Ordering::AcqRel);
                                let mut w = stream;
                                let _ = writeln!(
                                    w,
                                    "{{\"ok\":false,\"error\":\"too many unauthenticated clients\"}}"
                                );
                                let _ = w.shutdown(Shutdown::Both);
                                continue;
                            }
                            Some(UnauthSlotGuard {
                                unauth: unauth_clients.clone(),
                            })
                        } else {
                            None
                        };
                        let ip_ok = per_ip_clients
                            .lock()
                            .map(|mut map| {
                                let c = map.entry(peer_ip).or_insert(0);
                                if *c >= MAX_CLIENTS_PER_IP {
                                    false
                                } else {
                                    *c += 1;
                                    true
                                }
                            })
                            .unwrap_or(false);
                        if !ip_ok {
                            drop(unauth_guard);
                            active_clients.fetch_sub(1, Ordering::AcqRel);
                            let mut w = stream;
                            let _ = writeln!(
                                w,
                                "{{\"ok\":false,\"error\":\"too many connections from this IP\"}}"
                            );
                            let _ = w.shutdown(Shutdown::Both);
                            continue;
                        }
                        let _ = stream.set_nonblocking(false);
                        // Bounded read timeout lets client threads observe `stop_conn` promptly on Drop (#202).
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                        let tx = tx.clone();
                        let stop_conn = stop_bg.clone();
                        let rec = recorder.clone();
                        let slot_guard = ClientSlotGuard {
                            total: active_clients.clone(),
                            per_ip: per_ip_clients.clone(),
                            ip: peer_ip,
                        };
                        let client_id = NEXT_TCP_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
                        let origin = ClientOrigin::Tcp {
                            client_id,
                            is_loopback: peer_is_loopback,
                        };
                        thread::spawn(move || {
                            let _guard = slot_guard;
                            let mut unauth_guard = unauth_guard;
                            let Ok(mut writer) = stream.try_clone() else {
                                return;
                            };
                            let mut reader = BufReader::new(stream);
                            let mut pending: Vec<u8> = Vec::new();
                            let mut first_line_seen = false;

                            // Non-loopback peers must authenticate with `auth <token>` before any observation (#193).
                            if let Some(expected) = token_for_conn {
                                if json_mode {
                                    let _ = writeln!(
                                        writer,
                                        "{{\"ok\":true,\"auth_required\":true,\"message\":\"send 'auth <token>' as first line\"}}"
                                    );
                                } else {
                                    let _ = writeln!(
                                        writer,
                                        "[auth] Authentication required: send 'auth <token>'"
                                    );
                                }
                                let _ = writer.flush();
                                let auth_started = Instant::now();
                                let authed = loop {
                                    if stop_conn.load(Ordering::Relaxed) {
                                        break false;
                                    }
                                    if auth_started.elapsed() >= AUTH_DEADLINE {
                                        let _ = writeln!(
                                            writer,
                                            "{{\"ok\":false,\"error\":\"authentication timed out\",\"error_code\":\"auth_timeout\"}}"
                                        );
                                        let _ = writer.flush();
                                        break false;
                                    }
                                    match read_bounded_line(&mut reader, &mut pending, MAX_LINE_BYTES) {
                                        BoundedLine::Ok(l) => {
                                            first_line_seen = true;
                                            let t = l.trim();
                                            if is_http_probe(t) {
                                                let _ = writeln!(
                                                    writer,
                                                    "{{\"ok\":false,\"error\":\"HTTP requests are not accepted on this socket\",\"error_code\":\"protocol_mismatch\"}}"
                                                );
                                                let _ = writer.flush();
                                                break false;
                                            }
                                            if let Some(tok) = t.strip_prefix("auth ").map(str::trim) {
                                                if ct_eq(tok.as_bytes(), expected.as_bytes()) {
                                                    break true;
                                                }
                                            }
                                            let _ = writeln!(
                                                writer,
                                                "{{\"ok\":false,\"error\":\"authentication required\",\"error_code\":\"unauthorized\"}}"
                                            );
                                            let _ = writer.flush();
                                            break false;
                                        }
                                        BoundedLine::Timeout => continue,
                                        BoundedLine::TooLong => {
                                            let _ = writeln!(
                                                writer,
                                                "{{\"ok\":false,\"error\":\"line exceeds {MAX_LINE_BYTES} byte limit\"}}"
                                            );
                                            let _ = writer.flush();
                                            break false;
                                        }
                                        BoundedLine::Eof | BoundedLine::Err => break false,
                                    }
                                };
                                if !authed {
                                    let _ = writer.shutdown(Shutdown::Both);
                                    return;
                                }
                                // Release unauthenticated-client slot immediately upon auth success (#271).
                                drop(unauth_guard.take());
                            }

                            // Initial `look` observation + prompt on connect
                            let Some(mut last_mode) =
                                send_initial_look(&tx, origin, &mut writer, json_mode, !json_mode)
                            else {
                                return;
                            };
                            let mut last_activity = Instant::now();
                            loop {
                                if stop_conn.load(Ordering::Relaxed) {
                                    break;
                                }
                                let line = match read_bounded_line(&mut reader, &mut pending, MAX_LINE_BYTES) {
                                    BoundedLine::Ok(l) => {
                                        last_activity = Instant::now();
                                        l
                                    }
                                    BoundedLine::Timeout => {
                                        if last_activity.elapsed() >= IDLE_TIMEOUT {
                                            let _ = writeln!(
                                                writer,
                                                "{{\"ok\":false,\"error\":\"connection idle timeout\",\"error_code\":\"idle_timeout\"}}"
                                            );
                                            let _ = writer.flush();
                                            break;
                                        }
                                        continue;
                                    }
                                    BoundedLine::TooLong => {
                                        let _ = writeln!(
                                            writer,
                                            "{{\"ok\":false,\"error\":\"line exceeds {MAX_LINE_BYTES} byte limit\"}}"
                                        );
                                        let _ = writer.flush();
                                        break;
                                    }
                                    BoundedLine::Eof | BoundedLine::Err => break,
                                };
                                let trimmed = line.trim();
                                // Drop cross-protocol HTTP requests (e.g. browser fetch to loopback, #193).
                                if !first_line_seen && !trimmed.is_empty() {
                                    first_line_seen = true;
                                    if is_http_probe(trimmed) {
                                        let _ = writeln!(
                                            writer,
                                            "{{\"ok\":false,\"error\":\"HTTP requests are not accepted on this socket\",\"error_code\":\"protocol_mismatch\"}}"
                                        );
                                        let _ = writer.flush();
                                        break;
                                    }
                                }
                                if trimmed.is_empty()
                                    || trimmed.starts_with('#')
                                    || trimmed.starts_with("//")
                                {
                                    if !json_mode {
                                        let _ = write!(writer, "{}", format_prompt(&last_mode));
                                        let _ = writer.flush();
                                    }
                                    continue;
                                }
                                record_line(&rec, trimmed);
                                let (reply_tx, reply_rx) = mpsc::channel();
                                if tx
                                    .send(IncomingCmd {
                                        raw: trimmed.to_string(),
                                        interactive_blank: false,
                                        origin,
                                        reply_tx,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                                match reply_rx.recv() {
                                    Ok(reply) => {
                                        if let Some(m) = reply.response["mode"].as_str() {
                                            last_mode = m.to_string();
                                        }
                                        let show_prompt = !json_mode && !reply.bye;
                                        if write_reply(&mut writer, json_mode, &reply, show_prompt)
                                            .is_err()
                                        {
                                            break;
                                        }
                                        if reply.bye {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let _ = writer.shutdown(Shutdown::Both);
                        });
                    }
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            ErrorKind::WouldBlock
                                | ErrorKind::Interrupted
                                | ErrorKind::ConnectionAborted
                                | ErrorKind::ConnectionReset
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(30));
                    }
                    Err(e) => {
                        // Transient OS errors (e.g. EMFILE/ENFILE): back off instead of killing the server (#202).
                        eprintln!("[repal] TCP accept transient error on {addr_owned}: {e}");
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            eprintln!("[repal] Text Mode Socket Server stopped on {addr_owned}");
        });

        Ok(Self {
            rx,
            json_mode,
            local_addr,
            auth_token,
            is_batch: false,
            stop,
            closed,
            exit_code,
        })
    }

    /// Start a stdio channel (interactive TTY or piped stdin).
    pub fn start_stdio(json_mode: bool, record_file: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel::<IncomingCmd>();
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(AtomicI32::new(0));
        let stop_bg = stop.clone();
        let closed_bg = closed.clone();
        let recorder = Arc::new(Mutex::new(open_record_file(record_file)));

        thread::spawn(move || {
            let is_tty = io::stdin().is_terminal();
            let mut stdout = io::stdout();
            if is_tty && !json_mode {
                let _ = writeln!(
                    stdout,
                    "=== RePal Text Mode (输入 help 查看指令, quit 退出) ==="
                );
            }
            let show_prompt = is_tty && !json_mode;
            let mut last_mode =
                send_initial_look(&tx, ClientOrigin::Local, &mut stdout, json_mode, show_prompt)
                    .unwrap_or_else(|| "explore".to_string());

            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                if stop_bg.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(line) = line else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.starts_with('#') || trimmed.starts_with("//") {
                    if show_prompt {
                        let _ = write!(stdout, "{}", format_prompt(&last_mode));
                        let _ = stdout.flush();
                    }
                    continue;
                }
                if trimmed.is_empty() && !is_tty {
                    continue;
                }
                if !trimmed.is_empty() {
                    record_line(&recorder, trimmed);
                }
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx
                    .send(IncomingCmd {
                        raw: trimmed.to_string(),
                        interactive_blank: is_tty,
                        origin: ClientOrigin::Local,
                        reply_tx,
                    })
                    .is_err()
                {
                    break;
                }
                match reply_rx.recv() {
                    Ok(reply) => {
                        if let Some(m) = reply.response["mode"].as_str() {
                            last_mode = m.to_string();
                        }
                        let prompt_after = show_prompt && !reply.bye;
                        if write_reply(&mut stdout, json_mode, &reply, prompt_after).is_err() {
                            break;
                        }
                        if reply.bye {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            closed_bg.store(true, Ordering::Relaxed);
        });

        Self {
            rx,
            json_mode,
            local_addr: None,
            auth_token: None,
            is_batch: false,
            stop,
            closed,
            exit_code,
        }
    }

    /// Feed a finite batch of commands (`-c` / `--script`) asynchronously (used in GUI+Text mode).
    pub fn start_batch(
        cmds: Vec<String>,
        json_mode: bool,
        record_file: Option<PathBuf>,
        keep_going: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<IncomingCmd>();
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(AtomicI32::new(0));
        let stop_bg = stop.clone();
        let closed_bg = closed.clone();
        let exit_code_bg = exit_code.clone();
        let recorder = Arc::new(Mutex::new(open_record_file(record_file)));

        thread::spawn(move || {
            let mut stdout = io::stdout();
            let mut golden_failed = false;
            let mut had_error = false;
            for cmd in cmds {
                if stop_bg.load(Ordering::Relaxed) {
                    break;
                }
                record_line(&recorder, &cmd);
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx
                    .send(IncomingCmd {
                        raw: cmd,
                        interactive_blank: false,
                        origin: ClientOrigin::Local,
                        reply_tx,
                    })
                    .is_err()
                {
                    break;
                }
                match reply_rx.recv() {
                    Ok(reply) => {
                        let _ = write_reply(&mut stdout, json_mode, &reply, false);
                        if reply.golden_failed {
                            golden_failed = true;
                        }
                        if reply.bye {
                            break;
                        }
                        let ok = reply.response["ok"].as_bool().unwrap_or(true);
                        if !ok && !reply.golden_failed {
                            had_error = true;
                            if !keep_going {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let code = if golden_failed {
                2
            } else if had_error {
                1
            } else {
                0
            };
            exit_code_bg.store(code, Ordering::Relaxed);
            closed_bg.store(true, Ordering::Relaxed);
        });

        Self {
            rx,
            json_mode,
            local_addr: None,
            auth_token: None,
            is_batch: true,
            stop,
            closed,
            exit_code,
        }
    }
}

fn open_record_file(path: Option<PathBuf>) -> Option<File> {
    let p = path?;
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = fs::create_dir_all(parent);
        }
    }
    fs::OpenOptions::new().create(true).append(true).open(p).ok()
}

fn record_line(rec: &Arc<Mutex<Option<File>>>, trimmed: &str) {
    if matches!(trimmed, "quit" | "exit" | "q") {
        return;
    }
    if let Ok(mut guard) = rec.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{trimmed}");
            let _ = f.flush();
        }
    }
}

fn send_initial_look(
    tx: &Sender<IncomingCmd>,
    origin: ClientOrigin,
    writer: &mut dyn Write,
    json_mode: bool,
    show_prompt: bool,
) -> Option<String> {
    let (init_tx, init_rx) = mpsc::channel();
    if tx
        .send(IncomingCmd {
            raw: "look".to_string(),
            interactive_blank: false,
            origin,
            reply_tx: init_tx,
        })
        .is_err()
    {
        return None;
    }
    match init_rx.recv() {
        Ok(reply) => {
            if write_reply(writer, json_mode, &reply, show_prompt).is_ok() {
                Some(
                    reply.response["mode"]
                        .as_str()
                        .unwrap_or("explore")
                        .to_string(),
                )
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Write a single [`TextReply`] in either strict JSON-Lines or human-readable plain text.
pub fn write_reply(
    writer: &mut dyn Write,
    json_mode: bool,
    reply: &TextReply,
    show_prompt: bool,
) -> io::Result<()> {
    if json_mode {
        writeln!(writer, "{}", reply.response)?;
    } else {
        write!(
            writer,
            "{}",
            format_plain_response(&reply.response, reply.show_events)
        )?;
        if show_prompt {
            let mode = reply.response["mode"].as_str().unwrap_or("explore");
            write!(writer, "{}", format_prompt(mode))?;
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct ChunkedTimeoutReader {
        steps: std::collections::VecDeque<io::Result<Vec<u8>>>,
        cur: Vec<u8>,
        pos: usize,
    }

    impl ChunkedTimeoutReader {
        fn new(steps: Vec<io::Result<Vec<u8>>>) -> Self {
            Self {
                steps: steps.into(),
                cur: Vec::new(),
                pos: 0,
            }
        }
    }

    impl Read for ChunkedTimeoutReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let src = self.fill_buf()?;
            let n = src.len().min(buf.len());
            buf[..n].copy_from_slice(&src[..n]);
            self.consume(n);
            Ok(n)
        }
    }

    impl BufRead for ChunkedTimeoutReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.pos >= self.cur.len() {
                self.cur.clear();
                self.pos = 0;
                match self.steps.pop_front() {
                    Some(Ok(bytes)) => self.cur = bytes,
                    Some(Err(e)) => return Err(e),
                    None => return Ok(&[]),
                }
            }
            Ok(&self.cur[self.pos..])
        }

        fn consume(&mut self, amt: usize) {
            self.pos = (self.pos + amt).min(self.cur.len());
        }
    }

    #[test]
    fn bounded_line_reads_normal_and_rejects_oversized() {
        let mut cur = BufReader::new(Cursor::new(b"look\r\nstatus\n".to_vec()));
        let mut pending = Vec::new();
        match read_bounded_line(&mut cur, &mut pending, 32) {
            BoundedLine::Ok(s) => assert_eq!(s, "look"),
            _ => panic!("expected Ok(look)"),
        }
        match read_bounded_line(&mut cur, &mut pending, 32) {
            BoundedLine::Ok(s) => assert_eq!(s, "status"),
            _ => panic!("expected Ok(status)"),
        }
        assert!(matches!(
            read_bounded_line(&mut cur, &mut pending, 32),
            BoundedLine::Eof
        ));

        let huge = vec![b'x'; 128];
        let mut cur2 = BufReader::new(Cursor::new(huge));
        let mut pending2 = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut cur2, &mut pending2, 64),
            BoundedLine::TooLong
        ));
    }

    #[test]
    fn bounded_line_preserves_partial_bytes_across_wouldblock_timeout() {
        let mut r = ChunkedTimeoutReader::new(vec![
            Ok(b"/sta".to_vec()),
            Err(io::Error::new(ErrorKind::WouldBlock, "timeout")),
            Ok(b"te\n".to_vec()),
        ]);
        let mut pending = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut r, &mut pending, 64),
            BoundedLine::Timeout
        ));
        assert_eq!(pending, b"/sta", "partial bytes must be preserved across timeout");
        match read_bounded_line(&mut r, &mut pending, 64) {
            BoundedLine::Ok(line) => assert_eq!(line, "/state"),
            _ => panic!("expected reconstructed '/state' line"),
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn http_probe_detection_catches_cross_protocol_requests() {
        assert!(is_http_probe("POST / HTTP/1.1"));
        assert!(is_http_probe("GET /index.html HTTP/1.1"));
        assert!(is_http_probe("OPTIONS * HTTP/1.1"));
        assert!(!is_http_probe("look"));
        assert!(!is_http_probe("goto 10"));
        assert!(!is_http_probe("auth deadbeef"));
    }

    #[test]
    fn unauth_slot_guard_caps_and_releases_on_drop_271() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut guards = Vec::new();
        for _ in 0..MAX_UNAUTH_TCP_CLIENTS {
            let prev = counter.fetch_add(1, Ordering::AcqRel);
            assert!(prev < MAX_UNAUTH_TCP_CLIENTS);
            guards.push(UnauthSlotGuard {
                unauth: counter.clone(),
            });
        }
        assert_eq!(counter.load(Ordering::Acquire), MAX_UNAUTH_TCP_CLIENTS);
        // 5th unauthenticated peer would exceed `MAX_UNAUTH_TCP_CLIENTS` (4).
        assert!(counter.load(Ordering::Acquire) >= MAX_UNAUTH_TCP_CLIENTS);
        // Dropping one guard (auth success or disconnect) immediately frees a slot.
        guards.pop();
        assert_eq!(counter.load(Ordering::Acquire), MAX_UNAUTH_TCP_CLIENTS - 1);
        drop(guards);
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }
}
