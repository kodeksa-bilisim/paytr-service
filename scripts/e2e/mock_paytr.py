#!/usr/bin/env python3
"""E2E testleri için sahte PayTR sunucusu.

- POST /odeme            → sync ödeme yanıtı; e-postada "fail" geçiyorsa reddeder.
- POST /odeme/capi/list  → utoken için tek kartlık liste.
- POST /odeme/capi/delete→ başarılı.
Her isteği JSON satırı olarak LOG dosyasına yazar (testler form alanlarını doğrular).
"""
import json
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        form = {k: v[0] for k, v in urllib.parse.parse_qs(self.rfile.read(length).decode()).items()}
        with open(LOG, "a") as f:
            f.write(json.dumps({"path": self.path, "form": form}) + "\n")

        if self.path == "/odeme":
            if "fail" in form.get("email", ""):
                body = {"status": "failed", "err_msg": "Yetersiz bakiye"}
            else:
                body = {"status": "success"}
        elif self.path == "/odeme/capi/list":
            ut = form.get("utoken", "")
            body = [{
                "ctoken": "ct" + ut, "last_4": "1111", "require_cvv": "0",
                "month": "12", "year": "30", "c_bank": "Test", "c_type": "credit", "schema": "VISA",
            }]
        elif self.path == "/odeme/capi/delete":
            body = {"status": "success"}
        else:
            self.send_response(404)
            self.end_headers()
            return

        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
