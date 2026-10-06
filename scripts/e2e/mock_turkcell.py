#!/usr/bin/env python3
"""E2E testleri için sahte Turkcell e-Şirket e-Fatura/e-Arşiv servisi.

Alıcı adına göre davranış (addressBook.name):
- "Hata422"    → 422 doğrulama hatası (kalıcı)
- "Gecici"     → ilk istek 500, sonrakiler başarılı
- "DusenYanit" → faturayı kaydeder ama 500 döner (gönderimden sonra düşme)
- diğer        → kaydeder, numara verir

ETTN zaten varsa 422 "sistemde mevcut". Durum sorgusu: kayıtlı ETTN için 60, yoksa 404.
x-api-key "tc-test-key" değilse 401. Her isteği JSON satırı olarak LOG dosyasına yazar.
"""
import io
import json
import re
import sys
import zipfile
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]
KEY = "tc-test-key"
STORE = {}   # ettn -> {"doc", "number"}
SEEN = {}    # ad -> istek sayısı
COUNTER = [0]

USERS = [
    {"Identifier": "1234567802", "Title": "Test Kurum İki", "GibUserType": 1, "Alias": "urn:mail:a_pk@x.com", "AppType": 1, "IsActive": True},
    {"Identifier": "1234567802", "Title": "Test Kurum İki", "GibUserType": 1, "Alias": "urn:mail:defaultpk@kurum2.com", "AppType": 1, "IsActive": True},
    {"Identifier": "9999999999", "Title": "Pasif", "GibUserType": 1, "Alias": "urn:mail:defaultpk@p.com", "AppType": 1, "IsActive": False},
]


def users_zip():
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("gibusers_invoice_receipt_list.json", "﻿" + json.dumps(USERS, ensure_ascii=False))
    return buf.getvalue()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log(self, entry):
        with open(LOG, "a") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

    def authed(self):
        if self.headers.get("x-api-key") != KEY:
            self.reply(401, "Yetkisiz")
            return False
        return True

    def do_POST(self):
        if not self.authed():
            return
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        doc = {"/v2/earchive/create": "earchive", "/v1/outboxinvoice/create": "efatura"}.get(self.path)
        if not doc:
            return self.reply(404, "yok")
        ettn = body["generalInfoModel"]["ettn"]
        name = body["addressBook"]["name"]
        SEEN[name] = SEEN.get(name, 0) + 1
        self.log({"path": self.path, "ettn": ettn, "body": body})
        if ettn in STORE:
            return self.reply(422, {"uyarı": [f"Yeni kayıt için gönderdiğiniz ettn '{ettn}' sistemde mevcut."]})
        if "Hata422" in name:
            return self.reply(422, {"model.AddressBook": ["Alıcı bilgisi hatalı."]})
        if "Gecici" in name and SEEN[name] == 1:
            return self.reply(500, "Sunucu hatası")
        COUNTER[0] += 1
        number = f"NLK2026{COUNTER[0]:09d}"
        STORE[ettn] = {"doc": doc, "number": number}
        if "DusenYanit" in name:
            return self.reply(500, "bağlantı koptu")
        self.reply(200, {"id": ettn, "invoiceNumber": number})

    def do_GET(self):
        if not self.authed():
            return
        self.log({"path": self.path})
        if self.path == "/v2/gibuser/recipient/zip":
            return self.reply(200, users_zip(), "application/zip")
        m = re.match(r"^/v2/(earchive|outboxinvoice)/([0-9a-fA-F-]{36})/(status|pdf/true)$", self.path)
        if not m:
            return self.reply(404, "yok")
        inv = STORE.get(m.group(2))
        if not inv:
            return self.reply(404, "E-Arşiv Fatura bulunamadı!")
        if m.group(3) == "status":
            return self.reply(200, {"id": m.group(2), "invoiceNumber": inv["number"], "message": "", "status": 60})
        self.reply(200, b"%PDF-1.4 sahte fatura " + inv["number"].encode(), "application/pdf")


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
