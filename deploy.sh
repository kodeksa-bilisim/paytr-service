#!/usr/bin/env bash
# PayTR Service — Deploy Script
# Çalıştır: bash deploy.sh [--env]
# --env bayrağı .env dosyasını da sunucuya gönderir.

set -euo pipefail

PROJECT_DIR="/mnt/c/Dev/MyWorks/paytr_subscription/paytr-service"
BINARY="target/release/payment-service"
REMOTE_HOST="nlink"
REMOTE_DIR="/opt/paytr-service"
REMOTE_BIN="$REMOTE_DIR/payment-service"
SERVICE="paytr-service"
SEND_ENV=true

for arg in "$@"; do
  [[ "$arg" == "--env" ]] && SEND_ENV=true
done

# Renk çıktısı
ok()   { echo -e "\033[32m✓ $*\033[0m"; }
info() { echo -e "\033[34m→ $*\033[0m"; }
err()  { echo -e "\033[31m✗ $*\033[0m" >&2; }

# Servis hata sonrası bile yeniden başlatılsın
cleanup() {
  if [[ $? -ne 0 ]]; then
    err "Hata oluştu — servisi yeniden başlatmayı deniyorum..."
    ssh -t "$REMOTE_HOST" "sudo systemctl start $SERVICE" 2>/dev/null || true
  fi
}
trap cleanup EXIT

cd "$PROJECT_DIR"

# --- 1. Build ---
info "Release binary build ediliyor..."
cargo build --release
ok "Build tamamlandı → $BINARY"

# --- 2. .env gönder (isteğe bağlı) ---
if [[ "$SEND_ENV" == true ]]; then
  info ".env sunucuya gönderiliyor (localhost:5433 → 5432 dönüşümü)..."
  sed 's/localhost:5433/localhost:5432/' "$PROJECT_DIR/.env" | \
    ssh "$REMOTE_HOST" "cat > $REMOTE_DIR/.env"
  ok ".env güncellendi"
fi

# --- 3. Servisi durdur ---
info "Sunucuda $SERVICE durduruluyor..."
# -t: pseudo-TTY açar, sudo şifre sorarsa girilmesine izin verir
ssh -t "$REMOTE_HOST" "sudo systemctl stop $SERVICE"
ok "Servis durduruldu"

# --- 4. Binary kopyala ---
info "Binary kopyalanıyor..."
scp "$BINARY" "$REMOTE_HOST:$REMOTE_BIN"
ok "Binary gönderildi → $REMOTE_HOST:$REMOTE_BIN"

# --- 5. Servisi başlat ---
info "Servis başlatılıyor..."
ssh -t "$REMOTE_HOST" "sudo systemctl start $SERVICE"
ok "Servis başlatıldı"

# --- 6. Durum ---
echo ""
ssh -t "$REMOTE_HOST" "sudo systemctl status $SERVICE --no-pager -l"

echo ""
ok "Deploy tamamlandı."
