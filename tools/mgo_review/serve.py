#!/usr/bin/env python3
"""Localhost-first multi-threaded static + review-sync server for RePal MGO Review."""
import argparse
import functools
import json
import os
import secrets
import threading
import urllib.parse
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
REVIEWS_PATH = ROOT / ".tmp" / "mgo_reviews.json"
REVIEWS_PATH.parent.mkdir(parents=True, exist_ok=True)
LOCK = threading.Lock()
REVIEW_TOKEN = secrets.token_urlsafe(24)
MAX_POST_BYTES = 2 * 1024 * 1024  # 2 MiB hard body cap (#258)

# Explicit static directory allowlist (never serve repo root, .git, or .tmp; #258).
ALLOWED_PREFIXES = (
    ("/tools/mgo_review/", (ROOT / "tools" / "mgo_review").resolve()),
    ("/site/assets/", (ROOT / "site" / "assets").resolve()),
)


def _validate_reviews_schema(obj: object) -> dict:
    if not isinstance(obj, dict) or len(obj) > 5000:
        raise ValueError("Expected JSON object with at most 5000 entries")
    cleaned = {}
    for k, v in obj.items():
        if not isinstance(k, str) or not k or len(k) > 128:
            raise ValueError("Invalid review key")
        if not isinstance(v, dict):
            raise ValueError(f"Review entry for {k!r} must be an object")
        entry = {}
        verdict = v.get("verdict", "")
        if not isinstance(verdict, str) or verdict not in ("", "ok", "warn", "bad"):
            raise ValueError(f"Invalid verdict for {k!r}")
        entry["verdict"] = verdict
        note = v.get("note", "")
        if not isinstance(note, str) or len(note) > 4096:
            raise ValueError(f"Invalid note for {k!r}")
        entry["note"] = note
        if "reviewed_hash" in v:
            rh = v["reviewed_hash"]
            if not isinstance(rh, str) or len(rh) > 128:
                raise ValueError(f"Invalid reviewed_hash for {k!r}")
            entry["reviewed_hash"] = rh
        cleaned[k] = entry
    return cleaned


class FastHandler(SimpleHTTPRequestHandler):
    def address_string(self):
        return self.client_address[0]

    def log_message(self, format, *args):
        pass

    def _send_json(self, code: int, payload: bytes):
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _resolve_allowed_static(self, raw_path: str) -> Path | None:
        clean = urllib.parse.unquote(raw_path.split("?", 1)[0])
        if clean in ("/", "/tools/mgo_review", "/tools/mgo_review/"):
            clean = "/tools/mgo_review/index.html"
        for prefix, base_dir in ALLOWED_PREFIXES:
            if clean.startswith(prefix):
                rel = clean[len(prefix):]
                if "\x00" in rel or rel.startswith("/"):
                    return None
                candidate = (base_dir / rel).resolve()
                try:
                    candidate.relative_to(base_dir)
                except ValueError:
                    return None
                # Block hidden dotfiles/directories (e.g. .git, .tmp)
                if any(part.startswith(".") for part in candidate.relative_to(base_dir).parts):
                    return None
                return candidate
        return None

    def do_GET(self):
        path_only = self.path.split("?", 1)[0]
        if path_only == "/api/reviews":
            with LOCK:
                if REVIEWS_PATH.exists():
                    data = REVIEWS_PATH.read_bytes()
                else:
                    data = b"{}"
            self._send_json(200, data)
            return

        resolved = self._resolve_allowed_static(self.path)
        if resolved is None or not resolved.is_file():
            self.send_error(403 if resolved is None else 404, "Forbidden" if resolved is None else "Not Found")
            return

        # Inject per-session POST token into index.html so browser sync works automatically (#258).
        if resolved == (ROOT / "tools" / "mgo_review" / "index.html").resolve():
            html = resolved.read_text(encoding="utf-8")
            meta = f'<meta name="mgo-review-token" content="{REVIEW_TOKEN}">'
            if "</head>" in html:
                html = html.replace("</head>", f"  {meta}\n</head>", 1)
            payload = html.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        super().do_GET()

    def do_HEAD(self):
        resolved = self._resolve_allowed_static(self.path)
        if resolved is None or not resolved.is_file():
            self.send_error(403 if resolved is None else 404, "Forbidden" if resolved is None else "Not Found")
            return
        super().do_HEAD()

    def do_POST(self):
        if self.path.split("?", 1)[0] == "/api/reviews":
            token = self.headers.get("X-Review-Token", "")
            if not secrets.compare_digest(token, REVIEW_TOKEN):
                self._send_json(403, b'{"ok":false,"error":"missing or invalid X-Review-Token"}')
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if length < 0 or length > MAX_POST_BYTES:
                    self._send_json(413, b'{"ok":false,"error":"payload too large"}')
                    return
                raw = self.rfile.read(length)
                parsed = json.loads(raw.decode("utf-8"))
                cleaned = _validate_reviews_schema(parsed)
                out = json.dumps(cleaned, ensure_ascii=False, indent=2).encode("utf-8")
                with LOCK:
                    tmp = REVIEWS_PATH.with_suffix(".json.tmp")
                    tmp.write_bytes(out)
                    os.replace(tmp, REVIEWS_PATH)
                self._send_json(200, b'{"ok":true}')
            except Exception as e:
                err = json.dumps({"ok": False, "error": str(e)}).encode("utf-8")
                self._send_json(400, err)
            return
        self.send_error(404, "Not Found")


class FastServer(ThreadingHTTPServer):
    allow_reuse_address = True
    request_queue_size = 128


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description="RePal MGO Review Server")
    ap.add_argument("--lan", action="store_true", help="Bind 0.0.0.0 instead of 127.0.0.1")
    ap.add_argument("--port", type=int, default=8765, help="Port to listen on (default: 8765)")
    args = ap.parse_args()
    host = "0.0.0.0" if args.lan else "127.0.0.1"
    handler = functools.partial(FastHandler, directory=str(ROOT))
    srv = FastServer((host, args.port), handler)
    print(
        f"Fast MGO Review Server listening on http://{host}:{args.port}/tools/mgo_review/ "
        f"(reviews -> {REVIEWS_PATH}, token={REVIEW_TOKEN})",
        flush=True,
    )
    srv.serve_forever()
