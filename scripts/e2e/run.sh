#!/usr/bin/env bash
# payment-service uçtan uca testleri: Docker Postgres (Europe/Istanbul, prod gibi) +
# sahte PayTR (mock_paytr.py). Gerçek PayTR'a ve production'a hiçbir istek gitmez.
#
#   bash scripts/e2e/run.sh        (WSL; docker, python3, openssl, curl gerekir)
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PG=paytr-e2e-pg
PGPORT=55432
SVC_PORT=33002
MOCK_PORT=38080
KEY=testkey
SALT=testsalt
TOKEN=e2e-internal-token-0123456789abcdefghij
WORK=$(mktemp -d)
MOCK_LOG=$WORK/mock.log
SVC_LOG=$WORK/service.log
BASE=http://127.0.0.1:$SVC_PORT

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  \033[32mOK\033[0m   %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "${2:-}"; }
eq()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "beklenen '$3', gelen '$2'"; }

cleanup() {
  [ -n "${SVC_PID:-}" ] && kill "$SVC_PID" 2>/dev/null
  [ -n "${MOCK_PID:-}" ] && kill "$MOCK_PID" 2>/dev/null
  docker rm -f $PG >/dev/null 2>&1
  [ "$FAIL" = 0 ] && rm -rf "$WORK" || echo "Loglar: $WORK"
}
trap cleanup EXIT

sql() { docker exec -i $PG psql -U postgres -X -q -A -t -v ON_ERROR_STOP=1 -c "$1"; }
UTC="(now() at time zone 'utc')"   # psql oturumu Europe/Istanbul; servis UTC yazar

callback() { # oid status total [utoken] → HTTP kodu
  local hash
  hash=$(printf '%s' "$1$SALT$2$3" | openssl dgst -sha256 -hmac "$KEY" -binary | base64)
  curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/v1/payments/callback" \
    --data-urlencode "merchant_oid=$1" --data-urlencode "status=$2" \
    --data-urlencode "total_amount=$3" --data-urlencode "hash=$hash" \
    ${4:+--data-urlencode "utoken=$4"}
}
post() { # path json [token] → HTTP kodu
  curl -s -o "$WORK/last.json" -w '%{http_code}' -X POST "$BASE$1" \
    -H 'Content-Type: application/json' ${3:+-H "X-Internal-Token: $3"} -d "$2"
}
tick() { sleep 5; }  # scheduler aralığı 3 sn

echo "== Hazırlık"
( cd "$ROOT" && cargo build -q ) || { echo "build başarısız"; exit 1; }
docker rm -f $PG >/dev/null 2>&1
docker run -d --name $PG -e POSTGRES_PASSWORD=pw -e TZ=Europe/Istanbul -p $PGPORT:5432 \
  postgres:17 -c timezone=Europe/Istanbul >/dev/null
for _ in $(seq 1 30); do docker exec $PG pg_isready -U postgres >/dev/null 2>&1 && break; sleep 1; done
sleep 2
docker exec -i $PG psql -U postgres -X -q -v ON_ERROR_STOP=1 < "$HERE/customers_schema.sql" >/dev/null
sql "INSERT INTO customers (member_id,name,email,user_type,subscription_status,subscription_id) VALUES
 (1,'A','a@x.test','Gold','active','101'), (2,'B','fail-b@x.test','Gold','active','201'),
 (3,'C','c@x.test','Silver','active','301'), (4,'D','d@x.test','Gold','active','401'),
 (5,'E','e@x.test','Gold','active','501'), (6,'F','f@x.test','Gold','active','601'),
 (7,'G','g@x.test','Standard',NULL,NULL), (8,'H','h@x.test','Gold','active','801'),
 (9,'I','i@x.test','Silver','active','901');"

python3 "$HERE/mock_paytr.py" $MOCK_PORT "$MOCK_LOG" & MOCK_PID=$!
( cd "$WORK" && env -i PATH="$PATH" \
    DATABASE_URL="postgres://postgres:pw@127.0.0.1:$PGPORT/postgres" \
    MERCHANT_ID=m1 MERCHANT_KEY=$KEY MERCHANT_SALT=$SALT HOST=127.0.0.1 PORT=$SVC_PORT TEST_MODE=0 \
    BASE_URL=$BASE SCHEDULER_INTERVAL_SECS=3 SCHEDULER_START_DELAY_SECS=1 GRACE_DAYS=4 \
    MAX_FAILED_ATTEMPTS=3 INTERNAL_API_TOKEN=$TOKEN PAYTR_BASE_URL=http://127.0.0.1:$MOCK_PORT \
    RUST_LOG=payment_service=debug "$ROOT/target/debug/payment-service" > "$SVC_LOG" 2>&1 ) & SVC_PID=$!
for _ in $(seq 1 30); do curl -s "$BASE/health" >/dev/null && break; sleep 1; done

# Abonelik + kart tohumlama: sub id, member, plan, amount, utoken, bitiş (UTC ifadesi) [, dönem]
seed_sub() {
  sql "INSERT INTO paytr_user_tokens(member_id,utoken) VALUES ($2,'$5') ON CONFLICT DO NOTHING;
       INSERT INTO paytr_cards(utoken,ctoken,last_4,expiry_month,expiry_year,require_cvv,is_default)
         VALUES ('$5','ct$5','1111','12','30',false,true) ON CONFLICT DO NOTHING;
       INSERT INTO paytr_subscriptions(id,member_id,plan,status,utoken,ctoken,billing_cycle,amount,currency,
         started_at,expires_at,next_payment_date,user_email)
       SELECT $1,$2,'$3','active','$5','ct$5','${7:-monthly}','$4','TL',$UTC - interval '1 month',$6,$6,email
       FROM customers WHERE member_id=$2;"
}
seed_sub 101 1 gold 299.00 ut1 "$UTC - interval '1 hour'"
seed_sub 201 2 gold 299.00 ut2 "$UTC - interval '1 hour'"
seed_sub 301 3 silver 149.00 ut3 "$UTC + interval '10 days'"
seed_sub 401 4 gold 299.00 ut4 "$UTC + interval '10 days'"
seed_sub 501 5 gold 299.00 ut5 "$UTC + interval '10 days'"
seed_sub 601 6 gold 299.00 ut6 "$UTC + interval '2 hours'"
seed_sub 801 8 gold 899.00 ut8 "$UTC + interval '10 days'"
seed_sub 901 9 silver 3350.40 ut9 "$UTC + interval '360 days'" yearly
sql "SELECT setval('paytr_subscriptions_id_seq', 1000);" >/dev/null
# Callback'i gelmemiş eski pending ödemeler (PayTR durum sorgusu: old6 → 004, paid4x → başarılı,
# unk5 → geçici hata, unk9 → 8 gündür bilinmiyor)
sql "INSERT INTO paytr_payments(member_id,subscription_id,merchant_oid,amount,status,is_3d,created_at) VALUES
     (6,601,'old6',  '299.00','pending',true,  now() - interval '3 days'),
     (4,401,'paid4x','299.00','pending',false, now() - interval '3 days'),
     (5,501,'unk5',  '299.00','pending',false, now() - interval '3 days'),
     (9,901,'unk9',  '3350.40','pending',false, now() - interval '8 days');"
OLD401=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=401")
# Üye 2'nin ikinci utoken'ı ve PayTR'ın silemediği bir kartı
sql "INSERT INTO paytr_user_tokens(member_id,utoken) VALUES (2,'ut2b'),(2,'ut2bad');
     INSERT INTO paytr_cards(utoken,ctoken,last_4,expiry_month,expiry_year) VALUES
       ('ut2b','ctut2b','2222','12','30'), ('ut2bad','ctut2bad','3333','12','30');"

echo "== Güvenlik / doğrulama"
eq "health" "$(curl -s -o /dev/null -w '%{http_code}' $BASE/health)" 200
INIT='{"member_id":3,"plan":"gold","billing_cycle":"monthly","user_ip":"1.2.3.4","merchant_oid":"u3t1","email":"c@x.test","payment_amount":"899.00","user_name":"C","user_address":"Online","user_phone":"5000000000","user_basket":[{"name":"Gold","price":"899.00","quantity":1}],"merchant_ok_url":"https://nlink.tr/ok","merchant_fail_url":"https://nlink.tr/fail"}'
eq "init token'sız → 401" "$(post /api/v1/payments/init "$INIT")" 401
eq "init yanlış token → 401" "$(post /api/v1/payments/init "$INIT" wrong-token)" 401
eq "init yearly → 400" "$(post /api/v1/payments/init "${INIT/monthly/yearly}" $TOKEN)" 400
eq "init USD → 400" "$(post /api/v1/payments/init "${INIT/\"user_ip\"/\"currency\":\"USD\",\"user_ip\"}" $TOKEN)" 400
ENT='{"member_id":4,"email":"d@x.test","users":1,"extra_links":-9,"extra_clicks":-1,"user_name":"D","user_ip":"1.2.3.4","merchant_oid":"ent4t1","merchant_ok_url":"https://nlink.tr/ok","merchant_fail_url":"https://nlink.tr/fail"}'
eq "enterprise negatif ekstra → 400" "$(post /api/v1/payments/init-enterprise "$ENT" $TOKEN)" 400
eq "stored-card route kaldırıldı" "$(post /api/v1/payments/stored-card '{}' $TOKEN)" 404
eq "cards/list route kaldırıldı" "$(post /api/v1/cards/list '{"member_id":1}' $TOKEN)" 404
eq "callback geçersiz hash → 401" "$(curl -s -o /dev/null -w '%{http_code}' -X POST $BASE/api/v1/payments/callback -d 'merchant_oid=x&status=success&total_amount=1&hash=AAAA')" 401

echo "== Scheduler (saat dilimi, yenileme, red, eski pending)"
tick
P101=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=101 ORDER BY id DESC LIMIT 1")
[[ "$P101" =~ ^r101t[0-9]+$ ]] && ok "yenileme ödemesi oluşturuldu, merchant_oid alfanümerik ($P101)" || bad "yenileme ödemesi" "merchant_oid='$P101'"
eq "vadesi gelen abonelik expire EDİLMEDİ" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=101")" active
eq "PayTR formu: sepet fiyatı TL" "$(grep "\"merchant_oid\": \"$P101\"" "$MOCK_LOG" | grep -c '299.00')" 1
eq "PayTR formu: lang=tr" "$(grep "\"merchant_oid\": \"$P101\"" "$MOCK_LOG" | grep -c '"lang": "tr"')" 1
eq "reddedilen yenileme: ödeme failed" "$(sql "SELECT status FROM paytr_payments WHERE subscription_id=201")" failed
eq "reddedilen yenileme: 1 deneme sayıldı" "$(sql "SELECT renewal_attempts FROM paytr_subscriptions WHERE id=201")" 1
eq "reddedilen yenileme: grace içinde hâlâ aktif" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=201")" active
eq "saat dilimi: 2 saat sonra biten abonelik dokunulmadı" "$(sql "SELECT status||'/'||(SELECT count(*) FROM paytr_payments WHERE subscription_id=601 AND merchant_oid<>'old6') FROM paytr_subscriptions WHERE id=601")" "active/0"
eq "callback'i gelmeyen, PayTR'da başarısız (004) → failed" "$(sql "SELECT status||':'||failed_reason_msg FROM paytr_payments WHERE merchant_oid='old6'")" "failed:no_callback"
eq "callback'i gelmeyen ama PayTR'da başarılı → işlendi, 1 ay uzadı" "$(sql "SELECT p.status||'/'||(s.expires_at = timestamp '$OLD401' + interval '1 month') FROM paytr_payments p JOIN paytr_subscriptions s ON s.id=p.subscription_id WHERE p.merchant_oid='paid4x'")" "success/true"
eq "durum öğrenilemeyen ödeme pending kaldı" "$(sql "SELECT status FROM paytr_payments WHERE merchant_oid='unk5'")" pending
eq "7 günü aşan bilinmeyen durum → review" "$(sql "SELECT status||':'||failed_reason_msg FROM paytr_payments WHERE merchant_oid='unk9'")" "review:status_unknown"
eq "health: scheduler son çalışması görünüyor" "$(curl -s $BASE/health | python3 -c "import json,sys;print(json.load(sys.stdin)['scheduler_last_ok_age_secs'] is not None)")" True
tick
eq "reddedilen yenileme aynı gün tekrar denenmedi" "$(sql "SELECT count(*) FROM paytr_payments WHERE subscription_id=201")" 1

echo "== Yenileme callback'i + çift callback"
OLD_EXP=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")
eq "başarılı yenileme callback → 200" "$(callback "$P101" success 29900)" 200
eq "abonelik 1 ay uzadı" "$(sql "SELECT expires_at = timestamp '$OLD_EXP' + interval '1 month' FROM paytr_subscriptions WHERE id=101")" t
eq "ödeme success" "$(sql "SELECT status FROM paytr_payments WHERE merchant_oid='$P101'")" success
NEW_EXP=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")
callback "$P101" success 29900 >/dev/null
eq "tekrar callback ikinci kez uzatmadı" "$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")" "$NEW_EXP"
# Eşzamanlı çift callback: yeni bir yenileme ödemesi üret
sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC - interval '1 minute', last_renewal_attempt_at=NULL WHERE id=101"
tick
P101B=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=101 AND status='pending' ORDER BY id DESC LIMIT 1")
callback "$P101B" success 29900 >/dev/null & C1=$!
callback "$P101B" success 29900 >/dev/null & C2=$!
wait $C1 $C2
eq "eşzamanlı çift callback tek uzatma" "$(sql "SELECT expires_at = timestamp '$NEW_EXP' + interval '1 month' FROM paytr_subscriptions WHERE id=101")" t
eq "düşük tutarlı callback aktivasyon yapmaz" "$(sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC, last_renewal_attempt_at=NULL WHERE id=101"; tick; P=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=101 AND status='pending' ORDER BY id DESC LIMIT 1"); callback "$P" success 100 >/dev/null; sql "SELECT status FROM paytr_payments WHERE merchant_oid='$P'")" review

echo "== Grace sonrası expire + kart silme"
sql "UPDATE paytr_subscriptions SET renewal_attempts=3, expires_at=$UTC - interval '5 days' WHERE id=201"
tick
eq "grace sonrası abonelik expired" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=201")" expired
eq "müşteri Standard'a düştü" "$(sql "SELECT user_type||'/'||coalesce(subscription_id,'null') FROM customers WHERE member_id=2")" "Standard/null"
eq "kartlar silindi (DB)" "$(sql "SELECT count(*) FROM paytr_cards WHERE utoken='ut2' AND is_active")" 0
eq "kartlar silindi (PayTR)" "$(grep -c '"path": "/odeme/capi/delete".*"utoken": "ut2"' "$MOCK_LOG")" 1
eq "ikinci utoken'daki kart kendi utoken'ıyla silindi" "$(grep -c '"path": "/odeme/capi/delete".*"utoken": "ut2b",.*"ctoken": "ctut2b"' "$MOCK_LOG")" 1
eq "PayTR'ın silemediği kart DB'de aktif kaldı (yeniden denenecek)" "$(sql "SELECT count(*) FROM paytr_cards WHERE utoken='ut2bad' AND is_active")" 1
eq "boşalan utoken pasif, dolu olan aktif" "$(sql "SELECT string_agg(utoken||':'||is_active, ',' ORDER BY utoken) FROM paytr_user_tokens WHERE member_id=2")" "ut2:false,ut2b:false,ut2bad:true"

echo "== Upgrade (Silver → Gold, fark ücreti)"
eq "upgrade quote → 200" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":3,"plan":"gold","billing_cycle":"monthly"}' $TOKEN)" 200
QCHARGE=$(python3 -c "import json;print(json.load(open('$WORK/last.json'))['charge_amount'])")
eq "upgrade init → 200" "$(post /api/v1/payments/init "$INIT" $TOKEN)" 200
NEWSUB=$(sql "SELECT id FROM paytr_subscriptions WHERE member_id=3 AND status='pending'")
CHARGE=$(sql "SELECT amount FROM paytr_payments WHERE merchant_oid='u3t1'")
eq "tahsil edilen = liste − kalan değer (≈10/30 × 149 TL düşüldü)" "$(sql "SELECT '$CHARGE'::numeric BETWEEN 845 AND 855")" t
eq "teklif ile tahsilat aynı (±1 kuruş, saniye farkı)" "$(sql "SELECT abs('$QCHARGE'::numeric - '$CHARGE'::numeric) <= 0.01")" t
eq "PayTR formu tahsil tutarını taşıyor" "$(python3 -c "import json;print(json.load(open('$WORK/last.json'))['form_params']['payment_amount'])")" "$CHARGE"
eq "yenileme tutarı liste fiyatı" "$(sql "SELECT amount||'/'||(metadata->>'upgrade_from') FROM paytr_subscriptions WHERE id=$NEWSUB")" "899.00/301"
CHARGE_KURUS=$(sql "SELECT (('$CHARGE'::numeric)*100)::int")
eq "upgrade callback → 200" "$(callback u3t1 success $CHARGE_KURUS ut3new)" 200
eq "yeni abonelik aktif, kart bağlı" "$(sql "SELECT status||'/'||ctoken FROM paytr_subscriptions WHERE id=$NEWSUB")" "active/ctut3new"
eq "eski abonelik replaced" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=301")" replaced
eq "yeni dönem hemen başladı (1 ay, süre aktarılmadı)" "$(sql "SELECT expires_at BETWEEN $UTC + interval '1 month' - interval '1 hour' AND $UTC + interval '1 month' + interval '1 hour' FROM paytr_subscriptions WHERE id=$NEWSUB")" t
eq "müşteri Gold" "$(sql "SELECT user_type||'/'||subscription_id FROM customers WHERE member_id=3")" "Gold/$NEWSUB"

echo "== Geç callback'ler müşteri planını ezmez"
sql "INSERT INTO paytr_payments(member_id,subscription_id,merchant_oid,amount,is_3d) VALUES (3,301,'late301','149.00',false)"
eq "replaced aboneliğe yenileme callback'i → 200" "$(callback late301 success 14900)" 200
eq "ödeme review, müşteri hâlâ Gold" "$(sql "SELECT p.status||'/'||c.user_type||'/'||c.subscription_id FROM paytr_payments p, customers c WHERE p.merchant_oid='late301' AND c.member_id=3")" "review/Gold/$NEWSUB"
INIT7='{"member_id":7,"plan":"silver","billing_cycle":"monthly","user_ip":"1.2.3.4","merchant_oid":"u7a","email":"g@x.test","payment_amount":"349.00","user_name":"G","user_address":"Online","user_phone":"5000000000","user_basket":[{"name":"Silver","price":"349.00","quantity":1}],"merchant_ok_url":"https://nlink.tr/ok","merchant_fail_url":"https://nlink.tr/fail"}'
post /api/v1/payments/init "$INIT7" $TOKEN >/dev/null
post /api/v1/payments/init "${INIT7/u7a/u7b}" $TOKEN >/dev/null
SUB7B=$(sql "SELECT subscription_id FROM paytr_payments WHERE merchant_oid='u7b'")
callback u7b success 34900 ut7 >/dev/null
eq "ikinci sekmedeki ödeme aktifleşti" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=$SUB7B")" active
eq "iptal edilmiş ilk ödeme sonradan tamamlanınca → 200" "$(callback u7a success 34900 ut7)" 200
eq "ilk ödeme review, yeni abonelik yerinde" "$(sql "SELECT p.status||'/'||s.status||'/'||c.subscription_id FROM paytr_payments p, paytr_subscriptions s, customers c WHERE p.merchant_oid='u7a' AND s.id=$SUB7B AND c.member_id=7")" "review/active/$SUB7B"
eq "tekrar gelen review callback'i yine 200" "$(callback u7a success 34900 ut7)" 200

echo "== Fark ücreti sınırları"
eq "iptal edilmiş geçerli abonelik: aynı plan tekrar alınamaz" "$(post /api/v1/subscriptions/cancel '{"member_id":8,"subscription_id":801}' $TOKEN; post /api/v1/payments/init "$(echo "$INIT" | sed 's/"member_id":3/"member_id":8/; s/u3t1/u8t1/')" $TOKEN)" "204400"
eq "Silver yıllık → Gold aylık: kalan değer yetiyor → 400" "$(post /api/v1/payments/init "$(echo "$INIT" | sed 's/"member_id":3/"member_id":9/; s/u3t1/u9t1/')" $TOKEN)" 400
post /api/v1/subscriptions/upgrade-quote '{"member_id":9,"plan":"gold","billing_cycle":"yearly"}' $TOKEN >/dev/null
eq "Silver yıllık → Gold yıllık: fark ≈ 8630,40 − ~3300" "$(python3 -c "import json;print(5200 < float(json.load(open('$WORK/last.json'))['charge_amount']) < 5400)")" True
eq "Silver yıllık → Enterprise aylık: kalan değer yetiyor → 400" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":9,"plan":"enterprise","billing_cycle":"monthly","users":1,"extra_links":0,"extra_clicks":0}' $TOKEN)" 400

echo "== Abonelik başına tek pending ödeme"
sql "INSERT INTO paytr_payments(member_id,subscription_id,merchant_oid,amount) VALUES (4,401,'dupA','299.00')"
eq "ikinci pending reddedilir" "$(sql "INSERT INTO paytr_payments(member_id,subscription_id,merchant_oid,amount) VALUES (4,401,'dupB','299.00')" 2>&1 | grep -c uniq_paytr_payments_pending_per_subscription)" 1
sql "DELETE FROM paytr_payments WHERE merchant_oid='dupA'"
eq "aynı planı tekrar satın alma → 400" "$(post /api/v1/payments/init "${INIT/u3t1/u3t2}" $TOKEN)" 400
sql "UPDATE paytr_subscriptions SET expires_at=$UTC - interval '1 hour' WHERE id=301"
tick
eq "eski dönem bitince kullanıcı düşmedi" "$(sql "SELECT (SELECT status FROM paytr_subscriptions WHERE id=301)||'/'||user_type FROM customers WHERE member_id=3")" "replaced/Gold"

echo "== İptal / geri alma"
eq "iptal → 204" "$(post /api/v1/subscriptions/cancel '{"member_id":4,"subscription_id":401}' $TOKEN)" 204
eq "iptalde kart silinmedi" "$(sql "SELECT count(*) FROM paytr_cards WHERE utoken='ut4' AND is_active")" 1
eq "başkasının aboneliği iptal edilemez" "$(post /api/v1/subscriptions/cancel '{"member_id":1,"subscription_id":501}' $TOKEN)" 400
eq "geri alma → 204" "$(post /api/v1/subscriptions/reactivate '{"member_id":4}' $TOKEN)" 204
eq "abonelik tekrar aktif" "$(sql "SELECT status FROM paytr_subscriptions WHERE id=401")" active
post /api/v1/subscriptions/cancel "{\"member_id\":3,\"subscription_id\":$NEWSUB}" $TOKEN >/dev/null
post /api/v1/subscriptions/reactivate '{"member_id":3}' $TOKEN >/dev/null
eq "upgrade sonrası geri alma yalnızca güncel aboneliği canlandırır" "$(sql "SELECT count(*) FILTER (WHERE status='active')||'/'||(SELECT status FROM paytr_subscriptions WHERE id=301) FROM paytr_subscriptions WHERE member_id=3")" "1/replaced"

echo "== Ücretsiz plana geçiş"
eq "önce silver'a downgrade → 200" "$(post /api/v1/subscriptions/schedule-downgrade '{"member_id":5,"new_plan":"silver"}' $TOKEN)" 200
eq "standard'a downgrade → 200" "$(post /api/v1/subscriptions/schedule-downgrade '{"member_id":5,"new_plan":"standard"}' $TOKEN)" 200
eq "abonelik dönem sonunda bitecek şekilde iptal" "$(sql "SELECT s.status||'/'||c.scheduled_plan FROM paytr_subscriptions s JOIN customers c USING(member_id) WHERE s.id=501")" "cancelled/standard"
eq "downgrade iptali → 200" "$(post /api/v1/subscriptions/cancel-schedule '{"member_id":5}' $TOKEN)" 200
eq "abonelik tekrar aktif, plan temiz (müşteri ve abonelik)" "$(sql "SELECT s.status||'/'||coalesce(c.scheduled_plan,'null')||'/'||coalesce(s.scheduled_plan,'null') FROM paytr_subscriptions s JOIN customers c USING(member_id) WHERE s.id=501")" "active/null/null"

echo
grep -iE "panic|ERROR" "$SVC_LOG" | grep -v "Yenileme hatası\|Tahsil edilen tutar\|incelemeye alındı" | head -5
echo "== Sonuç: $PASS geçti, $FAIL başarısız"
[ "$FAIL" = 0 ]
