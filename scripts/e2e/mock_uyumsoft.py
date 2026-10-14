#!/usr/bin/env python3
"""E2E testleri için sahte Uyumsoft e-Dönüşüm SOAP servisi (/Services/Integration).

Alıcı unvanına göre davranış (TargetCustomer Title):
- "Hata422"    → IsSucceded=false (kalıcı ret)
- "Gecici"     → ilk istek SOAP hatası (500), sonrakiler başarılı
- "DusenYanit" → faturayı kaydeder ama SOAP hatası döner (gönderimden sonra düşme)
- diğer        → kaydeder, numara verir

UUID zaten varsa IsSucceded=false "daha önce gönderilmiş". Durum: kayıtlıysa Approved, yoksa
boş liste. IsEInvoiceUser: yalnız 1234567802. Kullanıcı adı/şifre uy-user/uy-pass değilse
güvenlik hatası. Her isteği JSON satırı olarak LOG dosyasına yazar.
"""
import base64
import json
import sys
import xml.etree.ElementTree as ET
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]
NS = {
    "s": "http://schemas.xmlsoap.org/soap/envelope/",
    "o": "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd",
    "t": "http://tempuri.org/",
    "cac": "urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2",
    "cbc": "urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2",
}
EINVOICE_USERS = {"1234567802"}
STORE = {}   # uuid -> {"number", "scenario"}
SEEN = {}    # unvan -> istek sayısı
COUNTER = [0]


def envelope(body):
    return f'<s:Envelope xmlns:s="{NS["s"]}"><s:Body>{body}</s:Body></s:Envelope>'.encode()


def fault(msg):
    return envelope(f"<s:Fault><faultcode>s:Client</faultcode><faultstring xml:lang=\"tr-TR\">{msg}</faultstring></s:Fault>")


def result(op, attrs="", inner=""):
    return envelope(f'<{op}Response xmlns="http://tempuri.org/"><{op}Result {attrs}>{inner}</{op}Result></{op}Response>')


def text(el, path):
    x = el.find(path, NS)
    return x.text if x is not None else None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, data):
        self.send_response(code)
        self.send_header("Content-Type", "text/xml; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log(self, entry):
        with open(LOG, "a") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

    def do_POST(self):
        if self.path != "/Services/Integration":
            return self.reply(404, b"yok")
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        root = ET.fromstring(raw)
        sec = root.find("s:Header/o:Security", NS)
        if (sec is None or text(sec, "o:UsernameToken/o:Username") != "uy-user"
                or text(sec, "o:UsernameToken/o:Password") != "uy-pass" or sec.find("{*}Timestamp") is None):
            return self.reply(500, fault("An error occurred when verifying security for the message."))
        op_el = root.find("s:Body", NS)[0]
        op = op_el.tag.split("}")[1]
        action = self.headers.get("SOAPAction", "").strip('"')
        if action != f"http://tempuri.org/IIntegration/{op}":
            return self.reply(500, fault(f"SOAPAction uyuşmuyor: {action}"))
        getattr(self, "op_" + op)(op_el)

    def op_SendInvoice(self, el):
        info = el.find("t:invoices/t:InvoiceInfo", NS)
        inv = info.find("t:Invoice", NS)
        cust = info.find("t:TargetCustomer", NS)
        uuid = text(inv, "cbc:UUID")
        title = cust.get("Title")
        SEEN[title] = SEEN.get(title, 0) + 1
        mail = info.find("t:Notification/t:Mailing", NS)
        self.log({
            "op": "SendInvoice",
            "local": info.get("LocalDocumentId"),
            "uuid": uuid,
            "profile": text(inv, "cbc:ProfileID"),
            "scenario": text(info, "t:Scenario"),
            "customer_id": text(inv, "cac:AccountingCustomerParty/cac:Party/cac:PartyIdentification/cbc:ID"),
            "supplier_id": text(inv, "cac:AccountingSupplierParty/cac:Party/cac:PartyIdentification/cbc:ID"),
            "alias": cust.get("Alias"),
            "net": text(inv, "cac:LegalMonetaryTotal/cbc:LineExtensionAmount"),
            "vat": text(inv, "cac:TaxTotal/cbc:TaxAmount"),
            "payable": text(inv, "cac:LegalMonetaryTotal/cbc:PayableAmount"),
            "internet_sale": info.find("t:EArchiveInvoiceInfo/t:InternetSalesInfo", NS) is not None,
            "mail_to": mail.get("To") if mail is not None else None,
        })
        if uuid in STORE:
            return self.reply(200, result("SendInvoice", 'IsSucceded="false" Message="Bu fatura daha önce gönderilmiş."'))
        if "Hata422" in title:
            return self.reply(200, result("SendInvoice", 'IsSucceded="false" Message="Alıcı bilgisi hatalı."'))
        if "Gecici" in title and SEEN[title] == 1:
            return self.reply(500, fault("Object reference not set to an instance of an object."))
        COUNTER[0] += 1
        number = f"NLK2026{COUNTER[0]:09d}"
        scenario = text(info, "t:Scenario")
        STORE[uuid] = {"number": number, "scenario": scenario}
        if "DusenYanit" in title:
            return self.reply(500, fault("Bağlantı koptu."))
        self.reply(200, result("SendInvoice", 'IsSucceded="true"', f'<Value Id="{uuid}" Number="{number}" InvoiceScenario="{scenario}"/>'))

    def op_QueryOutboxInvoiceStatus(self, el):
        ids = [s.text for s in el.findall("t:invoiceIds/t:string", NS)]
        self.log({"op": "QueryOutboxInvoiceStatus", "ids": ids})
        values = "".join(f'<Value Status="Approved" StatusCode="1" InvoiceId="{i}" Message="Onaylandı"/>' for i in ids if i in STORE)
        self.reply(200, result("QueryOutboxInvoiceStatus", 'IsSucceded="true"', values))

    def op_GetOutboxInvoicePdf(self, el):
        uuid = text(el, "t:invoiceId")
        self.log({"op": "GetOutboxInvoicePdf", "id": uuid})
        if uuid not in STORE:
            return self.reply(200, result("GetOutboxInvoicePdf", 'IsSucceded="false" Message="Fatura bulunamadı."'))
        data = base64.b64encode(b"%PDF-1.4 sahte fatura " + STORE[uuid]["number"].encode()).decode()
        self.reply(200, result("GetOutboxInvoicePdf", 'IsSucceded="true"', f'<Value InvoiceId="{uuid}"><Data>{data}</Data></Value>'))

    def op_IsEInvoiceUser(self, el):
        vkn = text(el, "t:vknTckn")
        self.log({"op": "IsEInvoiceUser", "vkn": vkn})
        flag = "true" if vkn in EINVOICE_USERS else "false"
        self.reply(200, result("IsEInvoiceUser", f'IsSucceded="true" Value="{flag}"'))


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
