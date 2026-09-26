#!/usr/bin/env bash
# PayTR Service — Deploy (WSL'den çalıştır): bash deploy.sh [--env]
#   --env : yerel .env'i de gönderir (sunucudaki önce .env.bak.<zaman> olarak yedeklenir).
#           Varsayılan: .env GÖNDERİLMEZ — sunucudaki .env tek doğruluk kaynağıdır.
# Adımlar: test → release build → binary .new olarak yükle → .bak yedek → atomik değiştir →
# restart → sağlık kontrolü; başarısızsa otomatik eski binary'ye döner.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="target/release/payment-service"
REMOTE_HOST="nlink"
REMOTE_DIR="/opt/paytr-service"
SERVICE="paytr-service"
SEND_ENV=false

for arg in "$@"; do
  [[ "$arg" == "--env" ]] && SEND_ENV=true
done

ok()   { echo -e "\033[32m✓ $*\033[0m"; }
info() { echo -e "\033[34m→ $*\033[0m"; }
err()  { echo -e "\033[31m✗ $*\033[0m" >&2; }

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cd "$PROJECT_DIR"

info "Testler çalıştırılıyor..."
cargo test -q
info "Release binary build ediliyor..."
cargo build --release
ok "Build tamamlandı → $BINARY"

if [[ "$SEND_ENV" == true ]]; then
  info ".env gönderiliyor (sunucudaki yedekleniyor)..."
  ssh "$REMOTE_HOST" "cp $REMOTE_DIR/.env $REMOTE_DIR/.env.bak.\$(date +%Y%m%d%H%M%S)"
  sed 's/localhost:5433/localhost:5432/' "$PROJECT_DIR/.env" | \
    ssh "$REMOTE_HOST" "umask 077 && cat > $REMOTE_DIR/.env && chmod 600 $REMOTE_DIR/.env"
  ok ".env güncellendi (chmod 600)"
fi

info "Binary yükleniyor..."
scp -q "$BINARY" "$REMOTE_HOST:$REMOTE_DIR/payment-service.new"

info "Servis güncelleniyor..."
ssh "$REMOTE_HOST" bash -s <<REMOTE
set -euo pipefail
cd $REMOTE_DIR
cp payment-service payment-service.bak
chmod +x payment-service.new
mv payment-service.new payment-service
sudo systemctl restart $SERVICE
sleep 3
PORT=\$(grep -E '^PORT=' .env | cut -d= -f2 | tr -d '\r')
if systemctl is-active --quiet $SERVICE && curl -fsS "http://127.0.0.1:\${PORT:-3002}/health" >/dev/null; then
  echo "Servis sağlıklı."
else
  echo "HATA: servis sağlıksız — eski binary'ye dönülüyor"
  journalctl -u $SERVICE -n 30 --no-pager || true
  cp payment-service.bak payment-service
  sudo systemctl restart $SERVICE
  exit 1
fi
REMOTE
ok "Deploy tamamlandı."
