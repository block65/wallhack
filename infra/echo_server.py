import socket
import argparse
import threading
import datetime
import json
import base64
from http.server import HTTPServer, BaseHTTPRequestHandler

# Global lock for synchronized printing
print_lock = threading.Lock()


def synchronized_print(*args, **kwargs):
    """Prints messages to the console, ensuring thread safety."""
    with print_lock:
        print(*args, **kwargs, flush=True)


def log_message(protocol, client_address, message, data=None):
    """Helper function to print log messages. Data is always logged as hex."""
    timestamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    log_entry_core = f"[{timestamp}] [{protocol}] {message} from {client_address[0]}:{client_address[1]}"
    if data:
        # Always print data as hex
        log_entry = f"{log_entry_core} | Data (hex): {data.hex()} ({len(data)} bytes)"
    else:
        log_entry = log_entry_core
    synchronized_print(log_entry)


def handle_tcp_client(conn, addr):
    """Handles an incoming TCP connection."""
    log_message("TCP", addr, "Connected")
    try:
        while True:
            data = conn.recv(1024)
            if not data:
                log_message("TCP", addr, "Connection closed by client (no data)")
                break
            log_message("TCP", addr, "Received", data)
            conn.sendall(data)
            log_message("TCP", addr, f"Echoed {len(data)} bytes")
    except ConnectionResetError:
        log_message("TCP", addr, "Connection reset by client")
    except socket.error as e:
        # More specific error handling for socket issues
        log_message("TCP", addr, f"Socket Error: {e}")
    except Exception as e:
        log_message("TCP", addr, f"Error: {e}")
    finally:
        conn.close()
        log_message("TCP", addr, "Connection closed")


def start_tcp_server(host, port):
    """Starts the TCP echo server."""
    server_socket = None  # Initialize to None
    try:
        server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server_socket.bind((host, port))
        server_socket.listen(5)
        synchronized_print(f"TCP Echo Server listening on {host}:{port}")
        while True:
            try:
                conn, addr = server_socket.accept()
                client_thread = threading.Thread(
                    target=handle_tcp_client, args=(conn, addr)
                )
                client_thread.daemon = (
                    True  # Allow main program to exit even if threads are running
                )
                client_thread.start()
            except OSError as e:
                # This can happen if the socket is closed by shutdown_server or during shutdown
                synchronized_print(
                    f"TCP Server: Error accepting connection: {e}. Server might be shutting down."
                )
                break
            except Exception as e:
                synchronized_print(
                    f"TCP Server: Unexpected error accepting connection: {e}"
                )
                break  # Exit loop on unexpected errors during accept
    except Exception as e:
        synchronized_print(
            f"TCP Server: Could not start or critical error on {host}:{port}. Error: {e}"
        )
    finally:
        synchronized_print("TCP Server: Shutting down...")
        if server_socket:
            try:
                server_socket.close()
            except Exception as e:
                synchronized_print(f"TCP Server: Error closing server socket: {e}")


def start_udp_server(host, port):
    """Starts the UDP echo server."""
    server_socket = None  # Initialize to None
    try:
        server_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_socket.bind((host, port))
        synchronized_print(f"UDP Echo Server listening on {host}:{port}")
        while True:
            try:
                data, addr = server_socket.recvfrom(1024)
                if not data:
                    log_message("UDP", addr, "Received empty packet")
                    continue
                log_message("UDP", addr, "Received", data)
                server_socket.sendto(data, addr)
                log_message("UDP", addr, f"Echoed {len(data)} bytes")
            except OSError as e:
                # This can happen if the socket is closed or during shutdown
                synchronized_print(
                    f"UDP Server: Error receiving data: {e}. Server might be shutting down."
                )
                break
            except Exception as e:
                synchronized_print(f"UDP Server: Unexpected error receiving data: {e}")
                break  # Exit loop on unexpected errors
    except Exception as e:
        synchronized_print(
            f"UDP Server: Could not start or critical error on {host}:{port}. Error: {e}"
        )
    finally:
        synchronized_print("UDP Server: Shutting down...")
        if server_socket:
            try:
                server_socket.close()
            except Exception as e:
                synchronized_print(f"UDP Server: Error closing server socket: {e}")


class HttpEchoHandler(BaseHTTPRequestHandler):
    """Handles incoming HTTP requests and echoes back details."""

    def do_GET(self):
        self._send_echo_response()

    def do_POST(self):
        self._send_echo_response()

    def do_PUT(self):
        self._send_echo_response()

    def do_DELETE(self):
        self._send_echo_response()

    def do_PATCH(self):
        self._send_echo_response()

    def _send_echo_response(self):
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length)
        body_base64 = base64.b64encode(body).decode("utf-8")

        response_data = {
            "method": self.command,
            "path": self.path,
            "headers": dict(self.headers),
            "body_base64": body_base64,
        }

        response_body = json.dumps(response_data, indent=2).encode("utf-8")

        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body)
        log_message(
            "HTTP",
            self.client_address,
            f"Echoed {self.command} request to {self.path}",
        )


def start_http_server(host, port):
    """Starts the HTTP echo server."""
    server_address = (host, port)
    httpd = None
    try:
        httpd = HTTPServer(server_address, HttpEchoHandler)
        synchronized_print(f"HTTP Echo Server listening on {host}:{port}")
        httpd.serve_forever()
    except Exception as e:
        synchronized_print(
            f"HTTP Server: Could not start or critical error on {host}:{port}. Error: {e}"
        )
    finally:
        synchronized_print("HTTP Server: Shutting down...")
        if httpd:
            try:
                httpd.server_close()
            except Exception as e:
                synchronized_print(f"HTTP Server: Error closing server: {e}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="TCP and/or UDP Echo Server with Hex Logging and Thread-Safe Prints"
    )
    parser.add_argument(
        "--tcp-port",
        type=int,
        help="Port for TCP server. If not specified, TCP server won't start.",
    )
    parser.add_argument(
        "--udp-port",
        type=int,
        help="Port for UDP server. If not specified, UDP server won't start.",
    )
    parser.add_argument(
        "--http-port",
        type=int,
        help="Port for HTTP server. If not specified, HTTP server won't start.",
    )
    parser.add_argument(
        "--host", type=str, default="0.0.0.0", help="Host to bind to (default: 0.0.0.0)"
    )
    args = parser.parse_args()

    if not args.tcp_port and not args.udp_port and not args.http_port:
        synchronized_print(
            "Error: You must specify at least one port (--tcp-port, --udp-port, or --http-port)."
        )
        parser.print_help()
        exit(1)

    threads = []
    active_servers = []

    if args.tcp_port:
        tcp_thread = threading.Thread(
            target=start_tcp_server, args=(args.host, args.tcp_port), daemon=True
        )
        threads.append(tcp_thread)
        active_servers.append("TCP")
        tcp_thread.start()

    if args.udp_port:
        udp_thread = threading.Thread(
            target=start_udp_server, args=(args.host, args.udp_port), daemon=True
        )
        threads.append(udp_thread)
        active_servers.append("UDP")
        udp_thread.start()

    if args.http_port:
        http_thread = threading.Thread(
            target=start_http_server, args=(args.host, args.http_port), daemon=True
        )
        threads.append(http_thread)
        active_servers.append("HTTP")
        http_thread.start()

    if active_servers:
        synchronized_print(
            f"Echo server(s) ({', '.join(active_servers)}) started. Press Ctrl+C to exit."
        )
    else:
        synchronized_print("No server ports specified. Exiting.")
        exit(0)

    try:
        for t in threads:
            if t.is_alive():  # Only join if the thread actually started and is running
                t.join()
    except KeyboardInterrupt:
        synchronized_print("\nCtrl+C received. Shutting down servers...")
    except Exception as e:
        synchronized_print(f"Main thread encountered an error: {e}")
    finally:
        synchronized_print(
            "All server threads have been signaled to shut down or have completed."
        )
