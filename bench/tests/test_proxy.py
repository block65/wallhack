"""Proxy integration tests: verify wallhack exit connects via HTTP CONNECT and SOCKS5 proxies."""

from __future__ import annotations

import os
import select
import socket
import struct
import subprocess
import threading
import time

import pytest

from lib.constants import EXIT_ID_WS, PROCESS_STARTUP_DELAY, WALLHACK_LISTEN_PORT_WS

# Separate port from the WebSocket topology port to avoid TIME_WAIT conflicts
# when both proxy tests run in the same pytest session.
_SOCKS_ENTRY_PORT = 6567

pytestmark = pytest.mark.connect


class HttpConnectProxy(threading.Thread):
    """Minimal HTTP CONNECT proxy for testing.

    Listens on a random localhost port, tunnels CONNECT requests.
    Thread-safe connection counter lets tests assert the proxy was actually used.
    """

    def __init__(self) -> None:
        super().__init__(daemon=True)
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(16)
        self.port: int = self._sock.getsockname()[1]
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self.connections = 0

    def run(self) -> None:
        self._sock.settimeout(0.5)
        while not self._stop.is_set():
            try:
                client, _ = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=self._handle, args=(client,), daemon=True).start()

    def _handle(self, client: socket.socket) -> None:
        target: socket.socket | None = None
        try:
            # Read until end of HTTP headers
            data = b""
            while b"\r\n\r\n" not in data:
                chunk = client.recv(4096)
                if not chunk:
                    return
                data += chunk

            first_line = data.split(b"\r\n")[0].decode(errors="replace")
            parts = first_line.split()
            if len(parts) < 2 or parts[0] != "CONNECT":
                client.close()
                return

            host, _, port_str = parts[1].rpartition(":")
            port = int(port_str)

            target = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            target.connect((host, port))

            client.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")

            with self._lock:
                self.connections += 1

            # Bidirectional tunnel
            socks = [client, target]
            while True:
                r, _, _ = select.select(socks, [], [], 1.0)
                if client in r:
                    d = client.recv(65536)
                    if not d:
                        break
                    target.sendall(d)
                if target in r:
                    d = target.recv(65536)
                    if not d:
                        break
                    client.sendall(d)
        except Exception:
            pass
        finally:
            try:
                client.close()
            except Exception:
                pass
            if target is not None:
                try:
                    target.close()
                except Exception:
                    pass

    def stop(self) -> None:
        self._stop.set()
        try:
            self._sock.close()
        except Exception:
            pass


def _start_wallhack(
    args: list[str],
    binary: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.Popen[bytes]:
    env = os.environ.copy()
    if extra_env:
        env.update(extra_env)
    return subprocess.Popen(
        [binary, *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
    )


def _read_output(proc: subprocess.Popen[bytes]) -> str:
    import selectors

    if proc.stdout is None:
        return ""
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    chunks: list[bytes] = []
    while sel.select(timeout=0):
        data = proc.stdout.read1(4096)  # type: ignore[attr-defined]
        if not data:
            break
        chunks.append(data)
    sel.close()
    return b"".join(chunks).decode(errors="replace")


def _stop(proc: subprocess.Popen[bytes]) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def test_exit_connects_via_http_proxy(wallhack_bin: str) -> None:
    """Exit node connects to entry node through an HTTP CONNECT proxy.

    Topology (all localhost, no netns):
      entry  :6566/tcp  (WebSocket)
      proxy  :random    (HTTP CONNECT, in-process)
      exit   -> HTTPS_PROXY=http://127.0.0.1:{proxy.port}
    """
    proxy = HttpConnectProxy()
    proxy.start()

    entry = _start_wallhack(
        ["entry", "-l", f":{WALLHACK_LISTEN_PORT_WS}/tcp", "-v"],
        wallhack_bin,
    )
    time.sleep(PROCESS_STARTUP_DELAY)

    exit_proc = _start_wallhack(
        [
            "exit",
            "-c", f"127.0.0.1:{WALLHACK_LISTEN_PORT_WS}/tcp",
            "-i", EXIT_ID_WS,
            "-v",
        ],
        wallhack_bin,
        extra_env={"HTTPS_PROXY": f"http://127.0.0.1:{proxy.port}"},
    )
    # Give the tunnel a moment to negotiate
    time.sleep(PROCESS_STARTUP_DELAY * 3)

    try:
        assert entry.poll() is None, f"entry crashed:\n{_read_output(entry)}"
        assert exit_proc.poll() is None, f"exit crashed:\n{_read_output(exit_proc)}"
        assert proxy.connections >= 1, (
            f"proxy received no CONNECT requests (port {proxy.port})"
        )
    finally:
        _stop(exit_proc)
        _stop(entry)
        proxy.stop()


class Socks5Proxy(threading.Thread):
    """Minimal unauthenticated SOCKS5 proxy for testing (RFC 1928).

    Supports IPv4 and domain-name address types. No authentication.
    """

    def __init__(self) -> None:
        super().__init__(daemon=True)
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(16)
        self.port: int = self._sock.getsockname()[1]
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self.connections = 0

    def run(self) -> None:
        self._sock.settimeout(0.5)
        while not self._stop.is_set():
            try:
                client, _ = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=self._handle, args=(client,), daemon=True).start()

    def _handle(self, client: socket.socket) -> None:
        target: socket.socket | None = None
        try:
            # Greeting: VER(1) NAUTH(1) METHODS(n)
            header = client.recv(2)
            if len(header) < 2 or header[0] != 0x05:
                return
            client.recv(header[1])  # discard offered methods
            client.sendall(b"\x05\x00")  # select: no authentication

            # Request: VER CMD RSV ATYP ...
            req = client.recv(4)
            if len(req) < 4 or req[0] != 0x05 or req[1] != 0x01:
                return
            atyp = req[3]

            if atyp == 0x01:  # IPv4
                host = socket.inet_ntoa(client.recv(4))
            elif atyp == 0x03:  # domain name
                n = client.recv(1)[0]
                host = client.recv(n).decode()
            else:
                client.sendall(b"\x05\x08\x00\x01\x00\x00\x00\x00\x00\x00")
                return

            port = struct.unpack("!H", client.recv(2))[0]

            target = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            target.connect((host, port))

            # Success: VER REP RSV ATYP BND.ADDR(4) BND.PORT(2)
            client.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")

            with self._lock:
                self.connections += 1

            # Bidirectional tunnel
            socks = [client, target]
            while True:
                r, _, _ = select.select(socks, [], [], 1.0)
                if client in r:
                    d = client.recv(65536)
                    if not d:
                        break
                    target.sendall(d)
                if target in r:
                    d = target.recv(65536)
                    if not d:
                        break
                    client.sendall(d)
        except Exception:
            pass
        finally:
            try:
                client.close()
            except Exception:
                pass
            if target is not None:
                try:
                    target.close()
                except Exception:
                    pass

    def stop(self) -> None:
        self._stop.set()
        try:
            self._sock.close()
        except Exception:
            pass


def test_exit_connects_via_socks5_proxy(wallhack_bin: str) -> None:
    """Exit node connects to entry node through a SOCKS5 proxy.

    Topology (all localhost, no netns):
      entry  :6567/tcp  (WebSocket, separate port to avoid TIME_WAIT with HTTP test)
      proxy  :random    (SOCKS5, in-process)
      exit   -> ALL_PROXY=socks5://127.0.0.1:{proxy.port}
    """
    proxy = Socks5Proxy()
    proxy.start()

    entry = _start_wallhack(
        ["entry", "-l", f":{_SOCKS_ENTRY_PORT}/tcp", "-v"],
        wallhack_bin,
    )
    time.sleep(PROCESS_STARTUP_DELAY)

    exit_proc = _start_wallhack(
        [
            "exit",
            "-c", f"127.0.0.1:{_SOCKS_ENTRY_PORT}/tcp",
            "-i", EXIT_ID_WS,
            "-v",
        ],
        wallhack_bin,
        extra_env={"ALL_PROXY": f"socks5://127.0.0.1:{proxy.port}"},
    )
    time.sleep(PROCESS_STARTUP_DELAY * 3)

    try:
        assert entry.poll() is None, f"entry crashed:\n{_read_output(entry)}"
        assert exit_proc.poll() is None, f"exit crashed:\n{_read_output(exit_proc)}"
        assert proxy.connections >= 1, (
            f"SOCKS5 proxy received no connections (port {proxy.port})"
        )
    finally:
        _stop(exit_proc)
        _stop(entry)
        proxy.stop()
