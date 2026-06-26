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

`.env` de güncellenecekse (yeni env değişkeni eklendi vb.):

```bash
bash deploy.sh --env
```

### deploy.sh ne yapar?

1. `cargo build --release` ile binary üretir
2. `sudo systemctl stop paytr-service` — servisi durdurur (çalışan binary üzerine yazılamaz)
3. `scp` ile binary'yi kopyalar
4. `sudo systemctl start paytr-service` — servisi başlatır
5. Hata olursa bile servisi yeniden başlatmayı dener (trap)
6. Son durumu ekrana basar

> **Neden önce stop?** Linux'ta çalışan bir binary'nin üzerine `scp` ile yazmak
> "Text file busy" hatası verir. Servisi durdurup kopyalamak çözümdür.

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
