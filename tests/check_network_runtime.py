"""Exercise production HTTP code under Wasmtime 48 (Astro API 4's runtime).

python tests/check_network_runtime.py --wasmtime PATH [--netease]
The default uses a deterministic local HTTP server; --netease also times four
anonymous EAPI calls. No account cookies or audio files are read or retained.
"""
from pathlib import Path
import argparse
import http.server
import importlib.util
import os
import shutil
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "tests/network-runtime"


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def handle(self):
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass

    def do_POST(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            body = bytearray()
            while True:
                size = int(self.rfile.readline().split(b";")[0], 16)
                if size == 0:
                    self.rfile.readline()
                    break
                body.extend(self.rfile.read(size))
                assert self.rfile.read(2) == b"\r\n"
        else:
            body = self.rfile.read(int(self.headers["Content-Length"]))
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Set-Cookie", "chem-test=ok; Path=/")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        try:
            if self.path == "/redirect":
                self.send_response(302)
                self.send_header("Location", "/slow")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            if self.path == "/slow":
                time.sleep(0.15)
            self.send_response(200)
            if self.path == "/chunked":
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                self.wfile.write(b"2\r\nre\r\n3\r\nady\r\n0\r\n\r\n")
                return
            size = 16 * 1024 * 1024 if self.path == "/large" else 5
            self.send_header("Content-Length", str(size))
            self.end_headers()
            if self.path == "/large":
                block = b"Z" * 65536
                for _ in range(size // len(block)):
                    self.wfile.write(block)
            elif self.path == "/truncated":
                self.wfile.write(b"re")
                self.close_connection = True
            else:
                self.wfile.write(b"ready")
                self.wfile.flush()
                if self.path == "/keepalive":
                    time.sleep(2)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wasmtime", default=shutil.which("wasmtime"))
    parser.add_argument("--netease", action="store_true")
    args = parser.parse_args()
    if not args.wasmtime:
        parser.error("--wasmtime must point to Wasmtime 48.0.2")
    spec = importlib.util.spec_from_file_location("chem_build", ROOT / "build.py")
    build = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(build)
    env = os.environ.copy()
    env["CARGO_TARGET_WASM32_WASIP2_LINKER"] = build.component_linker()
    env["CARGO_TARGET_DIR"] = str(ROOT / "out/cargo")
    # Use the exact clock dependency shipped in the plugin.
    clock = HARNESS / "wit/deps/wasi-clocks.wit"
    clock.write_bytes((ROOT / "wit/deps/wasi-clocks.wit").read_bytes())
    subprocess.run(["cargo", "build", "--locked", "--offline", "--release", "--target", "wasm32-wasip2",
                    "--manifest-path", str(HARNESS / "Cargo.toml")], cwd=ROOT, env=env, check=True)
    wasm = ROOT / "out/cargo/wasm32-wasip2/release/chem_network_check.wasm"
    command = [args.wasmtime, "run", "-S", "p3=y,http=y,inherit-network=y", str(wasm)]
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            subprocess.run(command + [f"http://127.0.0.1:{server.server_port}"], check=True, timeout=45)
        finally:
            server.shutdown()
    if args.netease:
        subprocess.run(command + ["netease"], check=True, timeout=90)


if __name__ == "__main__":
    main()
