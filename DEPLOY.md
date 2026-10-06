# PayTR Service — Deployment Rehberi

## Gereksinimler (Lokal)

- Rust (1.94.0+)
- WSL2 (Ubuntu)
- SSH erişimi sunucuya

---

## 1. Rust Güncelle

```powershell
rustup update stable
```

---

## 2. sqlx-cli Kur

```powershell
cargo install sqlx-cli --no-default-features --features postgres
```

---

## 3. SSH Tüneli Aç (sqlx prepare için)

Sunucudaki PostgreSQL dışarıya açık olmadığı için SSH tüneli gerekli.
Yeni bir terminalde aç ve açık tut:

```powershell
ssh -N -L 5433:localhost:5432 nlink
```

---

## 4. .env Ayarla

`.env` dosyasındaki `DATABASE_URL`'i tünel portuna (5433) yönlendir.
Şifredeki özel karakterler (`+`) URL encode edilmeli:

```env
DATABASE_URL=postgres://actix_user:sifre%2B...@localhost:5433/actix_prod_db
```

---

## 5. sqlx Offline Cache Oluştur

`paytr-service/` dizininde çalıştır:

```powershell
cd paytr-service
cargo sqlx prepare --workspace
```

Bu komut `.sqlx/` klasörü oluşturur. Sonraki build'lerde DB bağlantısı gerekmez.

---

## 6. Linux Binary Build (WSL2)

WSL2 Ubuntu terminalini aç:

```bash
wsl
```

OpenSSL kurulu değilse:

```bash
sudo apt update && sudo apt install -y pkg-config libssl-dev
```

Build:

```bash
cd /mnt/c/Dev/MyWorks/paytr_subscription/paytr-service
cargo build --release
```

> **Not:** `SQLX_OFFLINE=true` **gerekmez.** Proje yalnızca `sqlx::query()` fonksiyon formunu kullanıyor;
> compile-time DB bağlantısı gerektiren `query!()` makrosu yok.

Binary çıktısı: `target/release/payment-service`

---

## 7. WSL2 SSH Ayarı (ilk sefer)

### Deploy anahtarı oluştur ve sunucuya ekle

WSL terminalinde çalıştır:

```bash
# 1. Passphrase'siz deploy key üret
ssh-keygen -t ed25519 -f ~/.ssh/nlink_deploy -N '' -C 'nlink-deploy-wsl'

# 2. Public key'i sunucuya ekle — sunucu şifresini bir kez girmeni ister
ssh-copy-id -i ~/.ssh/nlink_deploy.pub -p 25416 admin@104.249.19.39

# 3. Test et (şifre sormamalı)
ssh nlink 'echo OK'
```

### WSL SSH config

`~/.ssh/config` içeriği (otomatik oluşturuldu):

```
Host nlink
    HostName 104.249.19.39
    User admin
    Port 25416
    IdentityFile ~/.ssh/nlink_deploy
    IdentitiesOnly yes
```

> **Neden ayrı key?** Windows'taki `id_rsa` Windows SSH agent'ı tarafından yönetilir.
> WSL'de doğrudan `/mnt/c/...` yolundaki key'i kullanmak izin hataları verir (777 izinler).
> Ayrı bir WSL deploy key kullanmak hem güvenli hem sorunsuz çalışır.

---

## 8. Sunucuda Dizin Oluştur (ilk sefer)

```bash
ssh -t nlink "sudo mkdir -p /opt/paytr-service && sudo chown admin:admin /opt/paytr-service"
```

---

## 9. Binary ve .env'i Sunucuya Gönder

Binary gönder (WSL2 içinde):

```bash
scp target/release/payment-service nlink:/opt/paytr-service/payment-service
```

.env gönder (DATABASE_URL portunu 5432'ye çevirerek):

```bash
sed 's/localhost:5433/localhost:5432/' /mnt/c/Dev/MyWorks/paytr_subscription/paytr-service/.env | \
  ssh nlink "cat > /opt/paytr-service/.env"
```

---

## 10. systemd Servisi Kur (ilk sefer)

Sunucuda:

```bash
sudo nano /etc/systemd/system/paytr-service.service
```

İçerik:

```ini
[Unit]
Description=PayTR Payment Service
After=network.target postgresql.service

[Service]
Type=simple
User=admin
WorkingDirectory=/opt/paytr-service
EnvironmentFile=/opt/paytr-service/.env
ExecStart=/opt/paytr-service/payment-service
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Servisi etkinleştir ve başlat:

```bash
sudo systemctl daemon-reload
sudo systemctl enable paytr-service
sudo systemctl start paytr-service
sudo systemctl status paytr-service
```

---

## Güncelleme (Sonraki Sürümler)

Kod değişikliği sonrası WSL2 terminalinde:

```bash
# İlk seferinde execute biti ver (bir kez yeterli):
chmod +x /mnt/c/Dev/MyWorks/paytr_subscription/paytr-service/deploy.sh

bash /mnt/c/Dev/MyWorks/paytr_subscription/paytr-service/deploy.sh
```

Sunucudaki `.env` tek doğruluk kaynağıdır; deploy varsayılan olarak onu **değiştirmez**.
Yerel `.env`'i bilerek göndermek için (sunucudaki önce `.env.bak.<zaman>` olarak yedeklenir):

```bash
bash deploy.sh --env
```

### deploy.sh ne yapar?

1. `cargo test` + `cargo build --release`
2. Binary'yi `payment-service.new` olarak yükler (çalışan binary'ye dokunmaz)
3. Eskisini `payment-service.bak` yedekler, `mv` ile atomik değiştirir, `systemctl restart`
4. `/health` kontrolü; başarısızsa log basar ve otomatik olarak `.bak`'a döner

### Zorunlu/önemli env değişkenleri
- `INTERNAL_API_TOKEN` (≥32 karakter): `/health` ve PayTR callback dışındaki tüm endpoint'ler
  `X-Internal-Token` başlığı ister. qurlfrontend'de `PAYTR_SERVICE_TOKEN` ile **aynı** değer.
- `HOST=127.0.0.1` — servis yalnızca localhost'u dinler.
- `RUST_LOG=payment_service=info,tower_http=info,sqlx=warn`
- `GRACE_DAYS` (varsayılan 4), `MAX_FAILED_ATTEMPTS` (varsayılan 3): yenileme günde bir denenir,
  ödeme alınamayan abonelik bitişten `GRACE_DAYS` gün sonra expire edilir.
- `.env` izinleri `600` olmalı (merchant key/salt içerir).
- `EINVOICE_ENABLED=1` + `TURKCELL_EFATURA_BASE_URL` + `TURKCELL_EFATURA_API_KEY`: e-Arşiv / e-Fatura
  kesimi (Turkcell e-Şirket). `EINVOICE_START_DATE` (TR günü) öncesi kayıtlar ve geriye dönük
  (`source='backfill'`) kayıtlar otomatik kesilmez. Canlı ödemeler varken TEST adresini açmayın:
  Turkcell faturayı alıcının e-postasına gönderir. Ayrıntı: aşağıdaki "e-Fatura" bölümü.

### e-Fatura (Turkcell e-Şirket)
Ödeme callback'i `invoices` kaydını `pending` açar; arka plan görevi (`src/einvoice/worker.rs`)
dakikada bir bekleyenleri keser. Alıcının VKN/TCKN'si GİB e-Fatura listesindeyse e-Fatura (temel
senaryo, posta kutusu listeden), değilse e-Arşiv (Turkcell müşteriye e-posta gönderir). Liste günde
bir `einvoice_users` tablosuna indirilir.
- ETTN gönderimden önce yazılır; yeniden denemede önce ETTN ile durum sorgulanır → çift fatura yok.
- KDV ve dip toplam bizden gider (`useCalculatedVatAmount`): fatura toplamı tahsilatla kuruşu kuruşuna aynı.
- Geçici hata: 2, 4, 8 … dk (en çok 6 sa) beklemeyle 8 deneme; kalıcı hata (422, 401/403) ya da 8.
  deneme → `failed` + `ALERT_EMAIL`. Yönetici panelinde "Yeniden dene" / "Elle kesildi".
- Kesilen faturanın GİB sonucu 2 gün izlenir (60 onay, 40 hata → `failed` + uyarı).
- Uçlar: `GET /api/v1/members/:id/invoices`, `GET /api/v1/invoices/:id/pdf?member_id=`,
  `POST /api/v1/admin/invoices/:id/{retry,manual}`.

### Abonelik yaşam döngüsü
`pending → active → (cancelled →) expired`; upgrade ile yerini yenisine bırakan abonelik
`replaced`. Scheduler saatte bir: (1) 48 saati geçmiş callback'siz pending ödemeleri `failed`
yapar, (2) vadesi gelenleri kayıtlı kartla tahsil eder, (3) süresi dolanları expire eder,
güncel aboneliği bitenleri Standard'a düşürür ve geçerli aboneliği kalmayan üyelerin kartlarını
PayTR + DB'den siler (iptal anında kart silinmez; dönem sonuna kadar iptal geri alınabilir).
DB oturumu UTC'ye sabitlenir (`SET TIME ZONE 'UTC'`); tüm tarih sütunları UTC'dir
(bu değişiklikten önce yazılmış `created_at/updated_at/cancelled_at` değerleri Europe/Istanbul).

### Testler
```bash
cargo test                    # unit testler
bash scripts/e2e/run.sh       # Docker Postgres + mock PayTR ile uçtan uca (WSL)
```

---

## Nginx Ayarı

> Bir sonraki adım: `api.nlink.tr` nginx bloğuna PayTR callback yönlendirmesi eklenecek.

`/etc/nginx/sites-enabled/api.nlink.tr` içine eklenecek:

```nginx
# PayTR callback — sadece bu endpoint dışarıya açık
location = /paytr/api/v1/payments/callback {
    proxy_pass http://127.0.0.1:3002/api/v1/payments/callback;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
}

# Diğer paytr endpointleri dışarıya kapalı
location /paytr/ {
    return 404;
}
```

Sonra:

```bash
sudo nginx -t && sudo systemctl reload nginx
```

---

## Logları İzleme

```bash
sudo journalctl -u paytr-service -f
```
