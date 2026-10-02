#!/usr/bin/env python3
"""E2E testleri için sahte PayTR sunucusu.

- POST /odeme            → sync ödeme yanıtı; e-postada "fail" geçiyorsa reddeder.
- POST /odeme/capi/list  → utoken için tek kartlık liste.
- POST /odeme/capi/delete→ başarılı (utoken'da "bad" geçiyorsa hata).
- POST /odeme/durum-sorgu → oid'de "paid": başarılı, "refund": başarılı+iade, "unk": geçici hata, diğer: 004.
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
            email = form.get("email", "")
            # Gerçek PayTR: sync_mode=0'da JSON yok, ok/fail adresine yönlendirir; sonuç callback'le gelir.
            if "async" in email and form.get("sync_mode") != "1":
                self.send_response(302)
                self.send_header("Location", form.get("merchant_ok_url", "/"))
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            if "merchant" in email:
                body = {"status": "failed", "err_msg": "Bu islem icin magazanin yetkisi yok (sync_mode)"}
            elif "fail" in email:
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
            if "bad" in form.get("utoken", ""):
                body = {"status": "error", "err_msg": "silinemedi"}
            else:
                body = {"status": "success"}
        elif self.path == "/odeme/durum-sorgu":
            oid = form.get("merchant_oid", "")
            if "refund" in oid:
                body = {"status": "success", "payment_amount": "299", "payment_total": "299", "returns": [{"return_amount": "299"}]}
            elif "paid" in oid:
                body = {"status": "success", "payment_amount": "299", "payment_total": "299", "returns": []}
            elif "unk" in oid:
                body = {"status": "error", "err_no": "010", "err_msg": "gecici hata"}
            else:
                body = {"status": "error", "err_no": "004", "err_msg": "merchant_oid ile basarili odeme bulunamadi"}
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
