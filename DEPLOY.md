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
SQLX_OFFLINE=true cargo build --release
```

Binary çıktısı: `target/release/payment-service`

---

## 7. WSL2 SSH Ayarı (ilk sefer)

WSL2 içinde Windows SSH config'ini kopyala:

```bash
cp -r /mnt/c/Users/coder/.ssh ~/.ssh
chmod 700 ~/.ssh
chmod 600 ~/.ssh/*
```

SSH config yoksa elle ekle:

```bash
nano ~/.ssh/config
```

```
Host nlink
    HostName 104.249.19.39
    User admin
    Port 25416
```

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

Kod değişikliği sonrası binary'yi yenilemek için:

```bash
# WSL2 içinde
cd /mnt/c/Dev/MyWorks/paytr_subscription/paytr-service
SQLX_OFFLINE=true cargo build --release
scp target/release/payment-service nlink:/opt/paytr-service/payment-service
ssh nlink "sudo systemctl restart paytr-service"
```

---

## Nginx Ayarı

> Bir sonraki adım: `api.nlink.tr` nginx bloğuna PayTR callback yönlendirmesi eklenecek.

`/etc/nginx/sites-enabled/api.nlink.tr` içine eklenecek:

```nginx
# PayTR callback — sadece bu endpoint dışarıya açık
location = /paytr/api/v1/payments/callback {
    proxy_pass http://127.0.0.1:3002;
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
