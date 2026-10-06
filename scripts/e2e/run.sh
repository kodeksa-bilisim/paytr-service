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
  [ -n "${TC_PID:-}" ] && kill "$TC_PID" 2>/dev/null
  docker rm -f $PG >/dev/null 2>&1
  [ "$FAIL" = 0 ] && rm -rf "$WORK" || echo "Loglar: $WORK"
}
trap cleanup EXIT

sql() { docker exec -i $PG psql -U postgres -X -q -A -t -v ON_ERROR_STOP=1 -c "$1"; }
UTC="(now() at time zone 'utc')"   # psql oturumu Europe/Istanbul; servis UTC yazar

callback() { # oid status total [utoken] [failed_reason_msg] → HTTP kodu
  local hash
  hash=$(printf '%s' "$1$SALT$2$3" | openssl dgst -sha256 -hmac "$KEY" -binary | base64)
  curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/v1/payments/callback" \
    --data-urlencode "merchant_oid=$1" --data-urlencode "status=$2" \
    --data-urlencode "total_amount=$3" --data-urlencode "hash=$hash" \
    ${4:+--data-urlencode "utoken=$4"} ${5:+--data-urlencode "failed_reason_msg=$5"}
}
post() { # path json [token] → HTTP kodu
  curl -s -o "$WORK/last.json" -w '%{http_code}' -X POST "$BASE$1" \
    -H 'Content-Type: application/json' ${3:+-H "X-Internal-Token: $3"} -d "$2"
}
get() { # path → HTTP kodu (iç token'la; gövde last.json'a)
  curl -s -o "$WORK/last.json" -w '%{http_code}' -H "X-Internal-Token: $TOKEN" "$BASE$1"
}
pyj() { python3 -c "import json;d=json.load(open('$WORK/last.json'));print($1)"; }  # last.json üzerinde ifade
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
 (9,'I','i@x.test','Silver','active','901'), (10,'J','merchant-j@x.test','Gold','active','102'),
 (11,'K','async-k@x.test','Gold','active','103'), (12,'L','l@x.test','Gold','active','105');"

python3 "$HERE/mock_paytr.py" $MOCK_PORT "$MOCK_LOG" & MOCK_PID=$!
( cd "$WORK" && env -i PATH="$PATH" \
    DATABASE_URL="postgres://postgres:pw@127.0.0.1:$PGPORT/postgres" \
    MERCHANT_ID=m1 MERCHANT_KEY=$KEY MERCHANT_SALT=$SALT HOST=127.0.0.1 PORT=$SVC_PORT TEST_MODE=0 \
    BASE_URL=$BASE SCHEDULER_INTERVAL_SECS=3 SCHEDULER_START_DELAY_SECS=1 GRACE_DAYS=4 \
    MAX_FAILED_ATTEMPTS=3 INTERNAL_API_TOKEN=$TOKEN PAYTR_BASE_URL=http://127.0.0.1:$MOCK_PORT \
    INVOICE_EXEMPT_MEMBERS=12 \
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
seed_sub 102 10 gold 299.00 ut10 "$UTC - interval '1 hour'"
seed_sub 103 11 gold 299.00 ut11 "$UTC - interval '1 hour'"
seed_sub 105 12 gold 299.00 ut12 "$UTC - interval '1 hour'"   # şirket içi hesap (faturalanmaz)
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
eq "PayTR formu: sync_mode varsayılan 0" "$(grep "\"merchant_oid\": \"$P101\"" "$MOCK_LOG" | grep -c '"sync_mode": "0"')" 1
eq "mağaza yetki hatası: ödeme failed" "$(sql "SELECT status FROM paytr_payments WHERE subscription_id=102")" failed
eq "mağaza yetki hatası: deneme SAYILMADI" "$(sql "SELECT s.renewal_attempts||'/'||coalesce(c.failed_payment_attempts,0) FROM paytr_subscriptions s JOIN customers c ON c.member_id=s.member_id WHERE s.id=102")" "0/0"
eq "mağaza yetki hatası: loglandı" "$(grep -c 'mağaza kaynaklı hatayla yapılamadı' "$SVC_LOG")" 1
P103=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=103 ORDER BY id DESC LIMIT 1")
eq "sync_mode=0 yönlendirmesi: ödeme pending (sonuç callback'le)" "$(sql "SELECT status FROM paytr_payments WHERE merchant_oid='$P103'")" pending
eq "sync_mode=0 yönlendirmesi: deneme sayılmadı" "$(sql "SELECT renewal_attempts FROM paytr_subscriptions WHERE id=103")" 0
eq "callback'te mağaza hatası → 200" "$(callback "$P103" failed 29900 "" "Bu islem icin magazanin yetkisi yok")" 200
eq "callback'te mağaza hatası: failed, deneme SAYILMADI" "$(sql "SELECT p.status||'/'||s.renewal_attempts||'/'||coalesce(c.failed_payment_attempts,0) FROM paytr_payments p JOIN paytr_subscriptions s ON s.id=p.subscription_id JOIN customers c ON c.member_id=s.member_id WHERE p.merchant_oid='$P103'")" "failed/0/0"
eq "saat dilimi: 2 saat sonra biten abonelik dokunulmadı" "$(sql "SELECT status||'/'||(SELECT count(*) FROM paytr_payments WHERE subscription_id=601 AND merchant_oid<>'old6') FROM paytr_subscriptions WHERE id=601")" "active/0"
eq "callback'i gelmeyen, PayTR'da başarısız (004) → failed" "$(sql "SELECT status||':'||failed_reason_msg FROM paytr_payments WHERE merchant_oid='old6'")" "failed:no_callback"
eq "callback'i gelmeyen ama PayTR'da başarılı → işlendi, 1 ay uzadı" "$(sql "SELECT p.status||'/'||(s.expires_at = timestamp '$OLD401' + interval '1 month') FROM paytr_payments p JOIN paytr_subscriptions s ON s.id=p.subscription_id WHERE p.merchant_oid='paid4x'")" "success/true"
eq "durum öğrenilemeyen ödeme pending kaldı" "$(sql "SELECT status FROM paytr_payments WHERE merchant_oid='unk5'")" pending
eq "7 günü aşan bilinmeyen durum → review" "$(sql "SELECT status||':'||failed_reason_msg FROM paytr_payments WHERE merchant_oid='unk9'")" "review:status_unknown"
eq "health: scheduler son çalışması görünüyor" "$(curl -s $BASE/health | python3 -c "import json,sys;print(json.load(sys.stdin)['scheduler_last_ok_age_secs'] is not None)")" True
tick
eq "reddedilen yenileme aynı gün tekrar denenmedi" "$(sql "SELECT count(*) FROM paytr_payments WHERE subscription_id=201")" 1

echo "== Yönetici paneli (salt-okunur uçlar)"
eq "admin/payments token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/admin/payments")" 401
eq "admin/overview token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/admin/overview")" 401
eq "admin/payments geçersiz durum → 400" "$(get '/api/v1/admin/payments?status=drop')" 400
eq "admin/payments geçersiz tür → 400" "$(get '/api/v1/admin/payments?kind=x')" 400
eq "başarısız yenilemeler → 200" "$(get '/api/v1/admin/payments?status=failed&kind=renewal')" 200
eq "başarısız yenilemeler: abonelik:sınıf" "$(pyj "','.join(sorted(f\"{i['subscription_id']}:{i['failure_class']}\" for i in d['items']))")" "102:merchant,103:merchant,201:bank"
eq "tür filtresi yalnızca yenileme döndürür" "$(pyj "','.join(sorted({i['kind'] for i in d['items']}))")" renewal
eq "tutar kuruş olarak" "$(pyj "d['items'][0]['amount_kurus']")" 29900
get '/api/v1/admin/payments?q=merchant-j' >/dev/null
eq "e-posta araması yalnızca o üyeyi bulur" "$(pyj "','.join(sorted({str(i['member_id']) for i in d['items']}))")" 10
get '/api/v1/admin/payments?q=%25' >/dev/null
eq "arama % karakterini joker saymaz" "$(pyj "len(d['items'])")" 0
get '/api/v1/admin/payments?limit=1' >/dev/null
eq "sayfalama: limit=1 → 1 kayıt + devamı var" "$(pyj "f\"{len(d['items'])}/{d['has_more']}\"")" "1/True"
eq "admin/overview → 200" "$(get '/api/v1/admin/overview')" 200
eq "7 gün başarısız dağılımı (abandoned/bank/merchant/system)" "$(pyj "'/'.join(str(v) for v in d['failed_7d_by_class'].values())")" "1/1/2/0"
eq "sorunlu abonelik 201: ek sürede + başarısız deneme" "$(pyj "','.join(next(s['reasons'] for s in d['subscriptions'] if s['id']==201))")" "in_grace,failed_attempts"
eq "sorunlu abonelik 201: son hata sınıfı banka" "$(pyj "next(s['last_failure_class'] for s in d['subscriptions'] if s['id']==201)")" bank
eq "dikkat: incelemedeki + 48 saati geçen bekleyen ödeme" "$(pyj "','.join(sorted(p['merchant_oid'] for p in d['payments']))")" "unk5,unk9"
eq "zamanlayıcı çalışıyor, sync_mode kapalı" "$(pyj "f\"{d['scheduler']['stalled']}/{d['scheduler']['sync_mode']}\"")" "False/False"

echo "== Yenileme callback'i + çift callback"
OLD_EXP=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")
eq "başarılı yenileme callback → 200" "$(callback "$P101" success 29900)" 200
eq "abonelik 1 ay uzadı" "$(sql "SELECT expires_at = timestamp '$OLD_EXP' + interval '1 month' FROM paytr_subscriptions WHERE id=101")" t
eq "ödeme success" "$(sql "SELECT status FROM paytr_payments WHERE merchant_oid='$P101'")" success
NEW_EXP=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")
callback "$P101" success 29900 >/dev/null
eq "tekrar callback ikinci kez uzatmadı" "$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=101")" "$NEW_EXP"
eq "fatura kaydı: tek, bekliyor, KDV dahil %20 ayrımı" "$(sql "SELECT count(*)||'/'||min(status)||'/'||min(net_kurus)||'+'||min(vat_kurus)||'='||min(total_kurus)||'/'||min(vat_rate) FROM invoices WHERE merchant_oid='$P101'")" "1/pending/24917+4983=29900/20"
eq "fatura: bireysel alıcı (ad + e-posta)" "$(sql "SELECT (buyer->>'type')||'/'||(buyer->>'name')||'/'||(buyer->>'email') FROM invoices WHERE merchant_oid='$P101'")" "individual/A/a@x.test"
eq "fatura satırı: yenileme dönemi" "$(sql "SELECT (lines->0->>'name') LIKE 'nlink Gold plan aboneliği (aylık) — %.%.% – %.%.%' FROM invoices WHERE merchant_oid='$P101'")" t
eq "PayTR sorgusuyla işlenen ödemeye de fatura" "$(sql "SELECT count(*) FROM invoices WHERE merchant_oid='paid4x'")" 1
P105=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=105 ORDER BY id DESC LIMIT 1")
callback "$P105" success 29900 >/dev/null
eq "şirket içi hesap: yenileme işlendi, fatura kaydı yok" "$(sql "SELECT p.status||'/'||(SELECT count(*) FROM invoices WHERE merchant_oid='$P105') FROM paytr_payments p WHERE p.merchant_oid='$P105'")" "success/0"
# Eşzamanlı çift callback: yeni bir yenileme ödemesi üret
sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC - interval '1 minute', last_renewal_attempt_at=NULL WHERE id=101"
tick
P101B=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=101 AND status='pending' ORDER BY id DESC LIMIT 1")
callback "$P101B" success 29900 >/dev/null & C1=$!
callback "$P101B" success 29900 >/dev/null & C2=$!
wait $C1 $C2
eq "eşzamanlı çift callback tek uzatma" "$(sql "SELECT expires_at = timestamp '$NEW_EXP' + interval '1 month' FROM paytr_subscriptions WHERE id=101")" t
eq "eşzamanlı çift callback tek fatura" "$(sql "SELECT count(*) FROM invoices WHERE merchant_oid='$P101B'")" 1
eq "düşük tutarlı callback aktivasyon yapmaz" "$(sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC, last_renewal_attempt_at=NULL WHERE id=101"; tick; P=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=101 AND status='pending' ORDER BY id DESC LIMIT 1"); callback "$P" success 100 >/dev/null; sql "SELECT status FROM paytr_payments WHERE merchant_oid='$P'")" review
eq "incelemeye alınan ödemeye fatura kaydı yok" "$(sql "SELECT count(*) FROM invoices i JOIN paytr_payments p USING (merchant_oid) WHERE p.status='review'")" 0

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

echo "== Fatura bilgisi (kurumsalda zorunlu alanlar)"
put() { # path json → HTTP kodu (iç token'la)
  curl -s -o "$WORK/last.json" -w '%{http_code}' -X PUT "$BASE$1" \
    -H 'Content-Type: application/json' -H "X-Internal-Token: $TOKEN" -d "$2"
}
CORP='{"member_id":3,"kind":"corporate","company_title":"C Bilişim A.Ş.","tax_number":"1234567890","tax_office":"Kadıköy","address":"Örnek Mah. 1. Sok. No:2","city":"İstanbul","district":"Kadıköy"}'
eq "fatura bilgisi token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/billing-profile/3")" 401
get /api/v1/billing-profile/3 >/dev/null
eq "kayıt yok → null (bireysel sayılır)" "$(pyj "d['profile']")" None
eq "kurumsal: vergi dairesi eksik → 400" "$(put /api/v1/billing-profile "$(echo "$CORP" | sed 's/"tax_office":"Kadıköy",//')")" 400
eq "kurumsal: VKN'de harf → 400" "$(put /api/v1/billing-profile "$(echo "$CORP" | sed 's/1234567890/12345678ab/')")" 400
eq "olmayan üye → 400" "$(put /api/v1/billing-profile "$(echo "$CORP" | sed 's/"member_id":3/"member_id":999/')")" 400
IND='{"member_id":3,"kind":"individual","full_name":" Cem  Kaya ","tax_number":"12345678901","address":"Örnek Mah. 1. Sok. No:2","city":"İstanbul","district":"Kadıköy","company_title":"atılır"}'
eq "bireysel: TCKN 10 hane → 400" "$(put /api/v1/billing-profile "$(echo "$IND" | sed 's/12345678901/1234567890/')")" 400
eq "bireysel: yarım adres → 400" "$(put /api/v1/billing-profile "$(echo "$IND" | sed 's/"district":"Kadıköy",//')")" 400
eq "bireysel kaydedildi → 200" "$(put /api/v1/billing-profile "$IND")" 200
get /api/v1/billing-profile/3 >/dev/null
eq "bireysel: ad normalleşti, unvan atıldı, TCKN ve adres var" "$(pyj "f\"{d['profile']['kind']}/{d['profile']['full_name']}/{d['profile']['company_title']}/{d['profile']['tax_number']}/{d['profile']['city']}\"")" "individual/Cem Kaya/None/12345678901/İstanbul"
eq "bireysel: yalnızca tür (hepsi boş) → 200" "$(put /api/v1/billing-profile '{"member_id":3,"kind":"individual"}')" 200
get /api/v1/billing-profile/3 >/dev/null
eq "bireysel boş: ad/TCKN/adres temizlendi" "$(pyj "f\"{d['profile']['full_name']}/{d['profile']['tax_number']}/{d['profile']['address']}\"")" "None/None/None"
eq "kurumsal kaydedildi → 200" "$(put /api/v1/billing-profile "$CORP")" 200
get /api/v1/billing-profile/3 >/dev/null
eq "kayıtlı bilgi okunuyor" "$(pyj "d['profile']['kind']+'/'+d['profile']['tax_number']+'/'+d['profile']['country']")" "corporate/1234567890/Türkiye"

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
eq "yükseltme faturası: kurumsal alıcı, fark ücreti satırı" "$(sql "SELECT (buyer->>'type')||'/'||(buyer->>'tax_number')||'/'||(lines->0->>'name') FROM invoices WHERE merchant_oid='u3t1'")" "corporate/1234567890/nlink Gold plan yükseltmesi (aylık) — fark ücreti"
eq "yükseltme faturası tutarı = tahsilat" "$(sql "SELECT total_kurus FROM invoices WHERE merchant_oid='u3t1'")" "$CHARGE_KURUS"

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
echo "== Muhasebe dışa aktarımı + panelde fatura"
MONTH=$(TZ=Europe/Istanbul date +%Y-%m)
eq "geçersiz ay → 400" "$(get '/api/v1/admin/invoices/export?month=2026-13')" 400
eq "aylık CSV → 200" "$(get "/api/v1/admin/invoices/export?month=$MONTH")" 200
eq "CSV: bireysel satır, KDV ayrımıyla" "$(grep -c "$P101;1;Bireysel;A;.*;249,17;20;49,83;299,00;pending" "$WORK/last.json")" 1
eq "CSV: kurumsal satır" "$(grep -c "u3t1;3;Kurumsal;C Bilişim A.Ş.;1234567890;Kadıköy;" "$WORK/last.json")" 1
eq "CSV: toplam satırı tüm faturaları sayıyor" "$(grep -c "^TOPLAM ($(sql "SELECT count(*) FROM invoices WHERE kind='sale'") fatura)" "$WORK/last.json")" 1
get /api/v1/admin/overview >/dev/null
eq "faturası olmayan başarılı ödeme yok" "$(pyj "d['payments_without_invoice']")" 0
get "/api/v1/admin/payments?q=$P101" >/dev/null
eq "ödeme listesinde fatura durumu" "$(pyj "d['items'][0]['invoice']['status']")" pending

echo "== Geriye dönük fatura kaydı"
# Faturalama öncesinden kalmış başarılı ödemeler: biri normal, biri test modu, biri okunamayan tutar
sql "INSERT INTO paytr_payments(member_id,subscription_id,merchant_oid,amount,status,is_3d,test_mode,created_at,callback_received_at) VALUES
     (1,101,'r101t1000','299.00','success',false,false,$UTC - interval '40 days',$UTC - interval '40 days'),
     (1,101,'u1t2000',  '299.00','success',true, true, $UTC - interval '40 days',$UTC - interval '40 days'),
     (1,101,'r101t3000','abc',   'success',false,false,$UTC - interval '39 days',$UTC - interval '39 days');"
eq "backfill token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/api/v1/admin/invoices/backfill")" 401
eq "deneme modu (varsayılan) → 200" "$(post /api/v1/admin/invoices/backfill '' $TOKEN)" 200
eq "deneme: 3 aday (test ödemesi hariç), hiçbir şey yazılmadı" "$(pyj "f\"{d['dry_run']}/{d['candidates']}/{d['created']}/{d['skipped']}\"")/$(sql "SELECT count(*) FROM invoices WHERE merchant_oid IN ('r101t1000','r101t3000','u1t2000')")" "True/3/0/2/0"
eq "şirket içi hesap atlanıyor" "$(pyj "next(i['reason'] for i in d['items'] if i['merchant_oid']=='$P105')")" "şirket içi hesap"
eq "gerçek çalıştırma → 200" "$(post '/api/v1/admin/invoices/backfill?dry_run=false' '' $TOKEN)" 200
eq "1 oluşturuldu; okunamayan tutar ve şirket içi hesap atlandı" "$(pyj "f\"{d['created']}/{d['skipped']}/\" + ','.join(i['action'] for i in d['items'])")" "1/2/created,skipped,skipped"
eq "şirket içi hesaba geriye dönük kayıt da açılmadı" "$(sql "SELECT count(*) FROM invoices WHERE merchant_oid='$P105'")" 0
eq "kayıt ödeme anına tarihli, işaretli, açıklamalı" "$(sql "SELECT source||'/'||status||'/'||(created_at = (SELECT callback_received_at FROM paytr_payments WHERE merchant_oid='r101t1000'))||'/'||(lines->0->>'name')||'/'||total_kurus FROM invoices WHERE merchant_oid='r101t1000'")" "backfill/pending/true/nlink Gold plan aboneliği (aylık) — yenileme/29900"
eq "test ödemesine fatura kaydı yok" "$(sql "SELECT count(*) FROM invoices WHERE merchant_oid='u1t2000'")" 0
post '/api/v1/admin/invoices/backfill?dry_run=false' '' $TOKEN >/dev/null
eq "tekrar çalıştırmak güvenli (yeni kayıt yok)" "$(pyj "d['created']")/$(sql "SELECT count(*) FROM invoices WHERE merchant_oid='r101t1000'")" "0/1"
OLD_MONTH=$(TZ=Europe/Istanbul date -d '-40 days' +%Y-%m)
get "/api/v1/admin/invoices/export?month=$OLD_MONTH" >/dev/null
eq "geriye dönük kayıt ödeme ayının CSV'sinde" "$(grep -c "r101t1000;1;Bireysel;A;" "$WORK/last.json")" 1
get /api/v1/admin/overview >/dev/null
eq "okunamayan tutarlı ödeme panelde uyarı olarak kalır" "$(pyj "d['payments_without_invoice']")" 1

echo "== Hesap silme / KVKK dışa aktarma (üye 3: kurumsal fatura bilgisi, faturalı ödeme, kart)"
eq "dışa aktarma token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/members/3/export")" 401
eq "dışa aktarma → 200" "$(get /api/v1/members/3/export)" 200
eq "abonelik, ödeme, fatura, fatura bilgisi ve kart var" "$(pyj "f\"{len(d['subscriptions'])>0}/{len(d['payments'])>0}/{len(d['invoices'])>0}/{d['billing_profile']['kind']}/{len(d['saved_cards'])>0}\"")" "True/True/True/corporate/True"
eq "dışa aktarmada kart token'ı yok" "$(grep -c 'ctoken\|utoken\|ctut3' "$WORK/last.json")" 0
eq "yenileme durdurma → 200" "$(post /api/v1/members/3/stop-renewal '' $TOKEN)" 200
eq "aktif abonelik iptal edildi (dönem sonuna kadar geçerli)" "$(sql "SELECT count(*) FILTER (WHERE status='active')||'/'||count(*) FILTER (WHERE status='cancelled' AND expires_at > $UTC) FROM paytr_subscriptions WHERE member_id=3")" "0/1"
INV3=$(sql "SELECT count(*) FROM invoices WHERE member_id=3")
PAY3=$(sql "SELECT count(*) FROM paytr_payments WHERE member_id=3")
eq "kalıcı silme → 200" "$(post /api/v1/members/3/erase '' $TOKEN)" 200
eq "PayTR'daki kartların hepsi silindi" "$(pyj "d['cards_remaining']")/$(sql "SELECT count(*) FROM paytr_cards c JOIN paytr_user_tokens t USING(utoken) WHERE t.member_id=3")" "0/0"
eq "abonelikler sonlandı, iletişim bilgisi silindi" "$(sql "SELECT count(*) FILTER (WHERE status IN ('active','pending','cancelled'))||'/'||count(user_email)||'/'||count(user_phone) FROM paytr_subscriptions WHERE member_id=3")" "0/0/0"
eq "fatura bilgisi silindi" "$(sql "SELECT count(*) FROM billing_profiles WHERE member_id=3")" 0
eq "ödeme ve fatura kayıtları saklandı (yasal)" "$(sql "SELECT count(*) FROM invoices WHERE member_id=3")/$(sql "SELECT count(*) FROM paytr_payments WHERE member_id=3")" "$INV3/$PAY3"
eq "kalıcı silme tekrar çağrılabilir" "$(post /api/v1/members/3/erase '' $TOKEN)" 200

echo "== Kupon (Gold %30, ilk 2 ödeme, 1 kullanım)"
sql "INSERT INTO customers (member_id,name,email,user_type) VALUES
 (20,'K20','k20@x.test','Standard'), (21,'K21','k21@x.test','Standard'), (22,'K22','k22@x.test','Standard'),
 (23,'K23','k23@x.test','Standard'), (24,'K24','k24@x.test','Standard');"
gold_init() { # member oid [ek alanlar]
  echo "$INIT" | sed "s/\"member_id\":3/\"member_id\":$1/; s/u3t1/$2/; s/\"email\":\"c@x.test\"/\"email\":\"k$1@x.test\"/; s/}\$/${3:-}}/"
}
eq "kupon token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Content-Type: application/json' -d '{}' "$BASE/api/v1/admin/coupons")" 401
eq "yüzde 150 → 400" "$(post /api/v1/admin/coupons '{"code":"BAD150","kind":"percent","value":"150"}' $TOKEN)" 400
eq "kupon oluşturuldu" "$(post /api/v1/admin/coupons '{"code":"yaz30","kind":"percent","value":"30","plans":["gold"],"duration_cycles":2,"max_redemptions":1}' $TOKEN)" 200
eq "aynı kod tekrar → 400" "$(post /api/v1/admin/coupons '{"code":"YAZ30","kind":"fixed","value":"50"}' $TOKEN)" 400
eq "Silver'da geçersiz → 400" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":20,"plan":"silver","billing_cycle":"monthly","coupon_code":"YAZ30"}' $TOKEN)" 400
eq "olmayan kod → 400" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":20,"plan":"gold","billing_cycle":"monthly","coupon_code":"YOK99"}' $TOKEN)" 400
post /api/v1/subscriptions/upgrade-quote '{"member_id":20,"plan":"gold","billing_cycle":"monthly","coupon_code":"yaz30"}' $TOKEN >/dev/null
eq "teklif: 899 − %30, yenileme de indirimli, 2 ödeme" "$(pyj "f\"{d['charge_amount']}/{d['discount_amount']}/{d['renewal_amount']}/{d['discount_cycles']}/{d['discount_source']}\"")" "629.30/269.70/629.30/2/coupon"
eq "kuponlu ödeme başlatıldı" "$(post /api/v1/payments/init "$(gold_init 20 u20a ',"coupon_code":"yaz30"')" $TOKEN)" 200
eq "PayTR tutarı ve sepeti indirimli" "$(pyj "d['form_params']['payment_amount']+'/'+d['discount_amount']")" "629.30/269.70"
eq "ödeme bitmeden kullanım sayılmadı" "$(sql "SELECT redemptions FROM coupons WHERE code='YAZ30'")" 0
eq "kuponlu callback → 200" "$(callback u20a success 62930 ut20)" 200
SUB20=$(sql "SELECT subscription_id FROM paytr_payments WHERE merchant_oid='u20a'")
eq "kullanım yazıldı, abonelik aktif, 1 indirimli yenileme kaldı" "$(sql "SELECT (SELECT redemptions FROM coupons WHERE code='YAZ30')||'/'||status||'/'||(metadata->'discount'->>'cycles_left') FROM paytr_subscriptions WHERE id=$SUB20")" "1/active/1"
eq "fatura indirimli tutarla" "$(sql "SELECT total_kurus FROM invoices WHERE merchant_oid='u20a'")" 62930
eq "kullanım sınırı dolu → 400" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":21,"plan":"gold","billing_cycle":"monthly","coupon_code":"YAZ30"}' $TOKEN)" 400
sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC - interval '1 minute', last_renewal_attempt_at=NULL WHERE id=$SUB20"
tick
P20=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=$SUB20 AND status='pending' ORDER BY id DESC LIMIT 1")
eq "1. yenileme indirimli çekildi" "$(sql "SELECT amount FROM paytr_payments WHERE merchant_oid='$P20'")" "629.30"
callback "$P20" success 62930 >/dev/null
eq "indirim hakkı bitti" "$(sql "SELECT metadata->'discount'->>'cycles_left' FROM paytr_subscriptions WHERE id=$SUB20")" 0
sql "UPDATE paytr_subscriptions SET next_payment_date=$UTC - interval '1 minute', last_renewal_attempt_at=NULL WHERE id=$SUB20"
tick
P20B=$(sql "SELECT merchant_oid FROM paytr_payments WHERE subscription_id=$SUB20 AND status='pending' ORDER BY id DESC LIMIT 1")
eq "2. yenileme liste fiyatından" "$(sql "SELECT amount FROM paytr_payments WHERE merchant_oid='$P20B'")" "899.00"
callback "$P20B" success 89900 >/dev/null
eq "kupon pasifleştirilebilir" "$(post /api/v1/admin/coupons/yaz30/active '{"active":false}' $TOKEN)" 200
get /api/v1/admin/coupons >/dev/null
eq "kupon listesi" "$(pyj "f\"{d['items'][0]['code']}/{d['items'][0]['active']}/{d['items'][0]['redemptions']}\"")" "YAZ30/False/1"

echo "== Ücretsiz deneme (kartsız 7 gün Gold)"
get /api/v1/members/24/growth >/dev/null
eq "deneme hakkı var" "$(pyj "d['trial']['eligible']")" True
eq "deneme başladı" "$(post /api/v1/trials/start '{"member_id":24,"email":"k24@x.test"}' $TOKEN)" 200
eq "müşteri Gold, durum trial, ödeme tarihi yok" "$(sql "SELECT user_type||'/'||subscription_status||'/'||coalesce(next_payment_date::text,'yok') FROM customers WHERE member_id=24")" "Gold/trial/yok"
eq "ikinci deneme → 400" "$(post /api/v1/trials/start '{"member_id":24,"email":"k24@x.test"}' $TOKEN)" 400
eq "abone olmuş üye deneme alamaz → 400" "$(post /api/v1/trials/start '{"member_id":20,"email":"k20@x.test"}' $TOKEN)" 400
post /api/v1/subscriptions/upgrade-quote '{"member_id":24,"plan":"gold","billing_cycle":"monthly"}' $TOKEN >/dev/null
eq "deneme sırasında Gold alınabilir (tam fiyat)" "$(pyj "d['charge_amount']")" "899.00"
TRIAL24=$(sql "SELECT id FROM paytr_subscriptions WHERE member_id=24")
sql "UPDATE paytr_subscriptions SET expires_at=$UTC + interval '1 day' WHERE id=$TRIAL24"
tick
eq "bitimine 48 saat kala hatırlatıldı" "$(sql "SELECT metadata->>'reminded' FROM paytr_subscriptions WHERE id=$TRIAL24")" true
sql "UPDATE paytr_subscriptions SET expires_at=$UTC - interval '1 minute' WHERE id=$TRIAL24"
tick
eq "süresi dolunca ek süresiz Standard" "$(sql "SELECT s.status||'/'||c.user_type FROM paytr_subscriptions s JOIN customers c USING (member_id) WHERE s.id=$TRIAL24")" "expired/Standard"
eq "denemede tahsilat denenmedi" "$(sql "SELECT count(*) FROM paytr_payments WHERE member_id=24")" 0
eq "deneme bitti, hak yok" "$(get /api/v1/members/24/growth >/dev/null; pyj "d['trial']['eligible']")" False

echo "== Referans (davetliye ilk ödemede %20, davet edene 14 gün sonra 1 ay)"
get /api/v1/members/22/growth >/dev/null
CODE22=$(pyj "d['referral']['code']")
eq "referans kodu 8 karakter" "${#CODE22}" 8
eq "aynı kod tekrar istenince değişmez" "$(get /api/v1/members/22/growth >/dev/null; pyj "d['referral']['code']")" "$CODE22"
eq "kendini davet edemez" "$(post /api/v1/referrals/claim "{\"member_id\":22,\"code\":\"$CODE22\"}" $TOKEN >/dev/null; pyj "d['claimed']")" False
eq "ödemesi olan üye davet edilemez" "$(post /api/v1/referrals/claim "{\"member_id\":20,\"code\":\"$CODE22\"}" $TOKEN >/dev/null; pyj "d['claimed']")" False
eq "geçersiz kod sessizce yok sayılır" "$(post /api/v1/referrals/claim '{"member_id":23,"code":"ZZZZZZZZ"}' $TOKEN >/dev/null; pyj "d['claimed']")" False
eq "davet kaydedildi (küçük harf de olur)" "$(post /api/v1/referrals/claim "{\"member_id\":23,\"code\":\"${CODE22,,}\"}" $TOKEN >/dev/null; pyj "d['claimed']")" True
eq "davetliye %20 gösteriliyor" "$(get /api/v1/members/23/growth >/dev/null; pyj "d['referral_discount_percent']")" 20
eq "davetli ilk ödemesi %20 indirimli" "$(post /api/v1/payments/init "$(gold_init 23 u23a)" $TOKEN >/dev/null; pyj "d['form_params']['payment_amount']")" "719.20"
callback u23a success 71920 ut23 >/dev/null
SUB23=$(sql "SELECT subscription_id FROM paytr_payments WHERE merchant_oid='u23a'")
eq "ilk ödeme işaretlendi; indirim yenilemede yok" "$(sql "SELECT (first_paid_at IS NOT NULL)||'/'||(SELECT metadata->'discount'->>'cycles_left' FROM paytr_subscriptions WHERE id=$SUB23) FROM referrals WHERE referred_id=23")" "true/0"
tick
eq "14 gün dolmadan ödül yok" "$(sql "SELECT coalesce(reward,'yok') FROM referrals WHERE referred_id=23")" yok
sql "UPDATE referrals SET first_paid_at = $UTC - interval '15 days' WHERE referred_id=23"
tick
eq "aboneliği olmayan davet edene 899 TL kredi" "$(sql "SELECT reward FROM referrals WHERE referred_id=23")/$(sql "SELECT sum(amount_kurus) FROM member_credits WHERE member_id=22")" "credit/89900"
post /api/v1/subscriptions/upgrade-quote '{"member_id":22,"plan":"silver","billing_cycle":"monthly"}' $TOKEN >/dev/null
eq "kredi ilk ödemeden düşülür (en az 1 TL)" "$(pyj "d['charge_amount']+'/'+d['balance_used']")" "1.00/348.00"
SILVER22=$(echo "$INIT7" | sed 's/"member_id":7/"member_id":22/; s/u7a/u22a/; s/g@x.test/k22@x.test/')
eq "kredili ödeme başlatıldı" "$(post /api/v1/payments/init "$SILVER22" $TOKEN >/dev/null; pyj "d['form_params']['payment_amount']")" "1.00"
callback u22a success 100 ut22 >/dev/null
eq "kalan kredi" "$(sql "SELECT sum(amount_kurus) FROM member_credits WHERE member_id=22")" 55100
eq "aynı ödeme için kredi ikinci kez düşülmez" "$(callback u22a success 100 ut22 >/dev/null; sql "SELECT count(*) FROM member_credits WHERE member_id=22 AND reason='used'")" 1
# Aboneliği olan davet eden: +1 ay uzatma
get /api/v1/members/20/growth >/dev/null
CODE20=$(pyj "d['referral']['code']")
post /api/v1/referrals/claim "{\"member_id\":21,\"code\":\"$CODE20\"}" $TOKEN >/dev/null
post /api/v1/payments/init "$(gold_init 21 u21a)" $TOKEN >/dev/null
callback u21a success 71920 ut21 >/dev/null
EXP20=$(sql "SELECT expires_at FROM paytr_subscriptions WHERE id=$SUB20")
sql "UPDATE referrals SET first_paid_at = $UTC - interval '15 days' WHERE referred_id=21"
tick
eq "aktif aboneliği olan davet edene +1 ay" "$(sql "SELECT reward FROM referrals WHERE referred_id=21")/$(sql "SELECT expires_at = timestamp '$EXP20' + interval '1 month' FROM paytr_subscriptions WHERE id=$SUB20")" "extension/t"
eq "müşteri kaydında bitiş de uzadı" "$(sql "SELECT c.subscription_expires_at = s.expires_at FROM customers c JOIN paytr_subscriptions s ON s.id=$SUB20 WHERE c.member_id=20")" t

echo "== Yönetici üye işlemleri (plan atama, sınırlar, deneme, iz kaydı)"
sql "INSERT INTO customers (member_id,name,email,user_type) VALUES (30,'Y30','y30@x.test','Standard');"
ACT='"actor_id":1,"actor_email":"admin@x.test"'
UNTIL=$(date -u -d '+10 days' +%F)
eq "yönetici uçları token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/admin/members/30")" 401
eq "üye özeti → 200" "$(get /api/v1/admin/members/30)" 200
eq "özet: plan + deneme hakkı" "$(pyj "f\"{d['plan']['user_type']}/{d['trial_eligible']}/{d['live_paid_subscription_id']}\"")" "Standard/True/None"
eq "geçersiz plan → 400" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"grant\",\"plan\":\"platinum\",\"until\":\"$UNTIL\"}" $TOKEN)" 400
eq "geçmiş tarih → 400" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"grant\",\"plan\":\"gold\",\"until\":\"2020-01-01\"}" $TOKEN)" 400
eq "Enterprise ata (3 koltuk) → 200" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"grant\",\"plan\":\"enterprise\",\"until\":\"$UNTIL\",\"users\":3,\"note\":\"pilot\"}" $TOKEN)" 200
eq "müşteri Enterprise, koltuk 3, durum trial" "$(sql "SELECT user_type||'/'||subscription_status||'/'||(custom_plan::json->>'users_limit') FROM customers WHERE member_id=30")" "Enterprise/trial/3"
eq "atanan plan tutarsız ve manual" "$(sql "SELECT amount||'/'||(metadata->>'manual')||'/'||(metadata->>'trial') FROM paytr_subscriptions WHERE member_id=30 AND status='active'")" "0.00/true/true"
eq "sınır: koltuk 5 → 200" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"limits\",\"users\":5}" $TOKEN)" 200
eq "koltuk 5, link sınırı korunur" "$(sql "SELECT (custom_plan::json->>'users_limit')||'/'||(custom_plan::json->>'links_limit') FROM customers WHERE member_id=30")" "5/10000"
eq "sınır: koltuk 0 → 400" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"limits\",\"users\":0}" $TOKEN)" 400
eq "Gold'a geçir (önceki atama biter) → 200" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"grant\",\"plan\":\"gold\",\"until\":\"$UNTIL\"}" $TOKEN)" 200
eq "tek aktif atama, custom_plan boş" "$(sql "SELECT count(*) FILTER (WHERE status='active')||'/'||(SELECT coalesce(custom_plan,'yok') FROM customers WHERE member_id=30) FROM paytr_subscriptions WHERE member_id=30")" "1/yok"
eq "Gold'da sınır değiştirilemez → 400" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"limits\",\"users\":2}" $TOKEN)" 400
eq "atanmış plan sırasında satın alma ilk ödeme (tam fiyat)" "$(post /api/v1/subscriptions/upgrade-quote '{"member_id":30,"plan":"gold","billing_cycle":"monthly"}' $TOKEN >/dev/null; pyj "d['charge_amount']")" "899.00"
eq "geri al → 200" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"revoke\"}" $TOKEN)" 200
eq "geri alınınca Standard" "$(sql "SELECT user_type||'/'||subscription_status FROM customers WHERE member_id=30")" "Standard/expired"
eq "geri alınacak yok → 400" "$(post /api/v1/admin/members/30/plan "{$ACT,\"mode\":\"revoke\"}" $TOKEN)" 400
eq "ücretli aboneliği olana plan atanmaz → 400" "$(post /api/v1/admin/members/20/plan "{$ACT,\"mode\":\"grant\",\"plan\":\"gold\",\"until\":\"$UNTIL\"}" $TOKEN)" 400
eq "ücretli üyede deneme hakkı yenilenmez → 400" "$(post /api/v1/admin/members/20/trial "{$ACT,\"action\":\"reset\"}" $TOKEN)" 400
eq "deneme hakkını yenile (24: deneme bitmişti) → 200" "$(post /api/v1/admin/members/24/trial "{$ACT,\"action\":\"reset\"}" $TOKEN)" 200
eq "24 yeniden deneme alabilir" "$(post /api/v1/trials/start '{"member_id":24,"email":"k24@x.test"}' $TOKEN)" 200
eq "denemeyi bitir → 200" "$(post /api/v1/admin/members/24/trial "{$ACT,\"action\":\"end\"}" $TOKEN)" 200
eq "deneme bitince Standard" "$(sql "SELECT user_type FROM customers WHERE member_id=24")" "Standard"
eq "aktif deneme yokken bitir → 400" "$(post /api/v1/admin/members/24/trial "{$ACT,\"action\":\"end\"}" $TOKEN)" 400
eq "başka servisin iz kaydı → 200" "$(post /api/v1/admin/audit "{$ACT,\"member_id\":30,\"action\":\"account.purge_now\",\"note\":\"test\"}" $TOKEN)" 200
eq "geçersiz işlem adı → 400" "$(post /api/v1/admin/audit "{$ACT,\"member_id\":30,\"action\":\"DROP TABLE\"}" $TOKEN)" 400
get "/api/v1/admin/audit?member_id=30" >/dev/null
eq "üye 30 iz kaydı (en yeni önce)" "$(pyj "','.join(i['action'] for i in d['items'])")" "account.purge_now,plan.revoke,plan.grant,plan.limits,plan.grant"
eq "iz kaydında önce/sonra ve not" "$(pyj "f\"{d['items'][-1]['before']['user_type']}/{d['items'][-1]['after']['user_type']}/{d['items'][-1]['note']}/{d['items'][-1]['actor_email']}\"")" "Standard/Enterprise/pilot/admin@x.test"

echo "== e-Fatura / e-Arşiv kesimi (sahte Turkcell)"
TC_PORT=38081
TC_LOG=$WORK/turkcell.log
python3 "$HERE/mock_turkcell.py" $TC_PORT "$TC_LOG" & TC_PID=$!
# Servisi süreç adıyla durdur ($SVC_PID alt kabuktur) ve portun boşalmasını bekle.
pkill -f "$ROOT/target/debug/payment-service" 2>/dev/null
for _ in $(seq 1 30); do curl -s -o /dev/null "$BASE/health" || break; sleep 0.5; done
# Önceki bölümlerin bekleyen kayıtları bu bölümü karıştırmasın.
sql "UPDATE invoices SET status='manual' WHERE status='pending' AND source IS DISTINCT FROM 'backfill';" >/dev/null
inv() { # oid member buyer_json net vat [source]
  sql "INSERT INTO invoices (merchant_oid, member_id, kind, status, buyer, lines, vat_rate, net_kurus, vat_kurus, total_kurus, source)
       VALUES ('$1', $2, 'sale', 'pending', '$3'::jsonb, '[{\"name\":\"nlink Gold plan aboneliği (aylık)\"}]'::jsonb, 20, $4, $5, $4 + $5, ${6:-NULL});"
}
inv ein1 40 '{"type":"individual","name":"Ayşe Yılmaz","email":"ayse@x.test"}' 24917 4983
inv ein2 41 '{"type":"corporate","title":"Test Kurum İki","tax_number":"1234567802","tax_office":"Kadıköy","address":"Örnek Mah. 1","city":"İstanbul","district":"Kadıköy","country":"Türkiye","email":"k@x.test"}' 24917 4983
inv ein3 42 '{"type":"individual","name":"Hata422 Test","email":"h@x.test"}' 24917 4983
inv ein4 43 '{"type":"individual","name":"Gecici Test","email":"g@x.test"}' 88 17
inv ein5 44 '{"type":"individual","name":"DusenYanit Test","email":"d@x.test"}' 24917 4983
inv ein6 45 '{"type":"individual","name":"Eski Kayıt","email":"e@x.test"}' 24917 4983 "'backfill'"
inv ein7 46 '{"type":"corporate","title":"Liste Dışı A.Ş.","tax_number":"1111111112","tax_office":"Şişli","address":"Örnek 2","city":"İstanbul","district":"Şişli","country":"Türkiye","email":"l@x.test"}' 24917 4983

( cd "$WORK" && env -i PATH="$PATH" \
    DATABASE_URL="postgres://postgres:pw@127.0.0.1:$PGPORT/postgres" \
    MERCHANT_ID=m1 MERCHANT_KEY=$KEY MERCHANT_SALT=$SALT HOST=127.0.0.1 PORT=$SVC_PORT TEST_MODE=0 \
    BASE_URL=$BASE SCHEDULER_INTERVAL_SECS=3600 SCHEDULER_START_DELAY_SECS=3600 GRACE_DAYS=4 \
    MAX_FAILED_ATTEMPTS=3 INTERNAL_API_TOKEN=$TOKEN PAYTR_BASE_URL=http://127.0.0.1:$MOCK_PORT \
    INVOICE_EXEMPT_MEMBERS=12 \
    EINVOICE_ENABLED=1 TURKCELL_EFATURA_BASE_URL=http://127.0.0.1:$TC_PORT TURKCELL_EFATURA_API_KEY=tc-test-key \
    EINVOICE_INTERVAL_SECS=2 EINVOICE_START_DELAY_SECS=1 \
    RUST_LOG=payment_service=debug "$ROOT/target/debug/payment-service" >> "$SVC_LOG" 2>&1 ) & SVC_PID=$!
for _ in $(seq 1 30); do curl -s "$BASE/health" >/dev/null && break; sleep 1; done
sleep 6
istate() { sql "SELECT status||'/'||coalesce(doc_type,'-')||'/'||coalesce(provider,'-') FROM invoices WHERE merchant_oid='$1'"; }
tcreq() { python3 -c "
import json,sys
for l in open('$TC_LOG'):
    r=json.loads(l)
    if r.get('body',{}).get('localReferenceId')=='$1': print(eval(sys.argv[1])); break
" "$2"; }
eq "GİB listesi: etkin alıcı kutusu, defaultpk tercih" "$(sql "SELECT count(*)||'/'||max(alias) FROM einvoice_users")" "1/urn:mail:defaultpk@kurum2.com"
eq "bireysel → e-Arşiv kesildi" "$(istate ein1)" "issued/earchive/turkcell"
eq "fatura no + ETTN yazıldı" "$(sql "SELECT (invoice_no LIKE 'NLK2026%')::text||'/'||(provider_ref = ettn::text)::text FROM invoices WHERE merchant_oid='ein1'")" "true/true"
eq "e-Arşiv modeli: kayıt türü, KDV bizden, e-posta" "$(tcreq ein1 "f\"{r['body']['recordType']}/{r['body']['invoiceLines'][0]['lineExtensionAmount']}/{r['body']['invoiceLines'][0]['vatAmount']}/{r['body']['eArsivInfo']['sendEMail']}/{r['body']['addressBook']['identificationNumber']}\"")" "0/249.17/49.83/True/11111111111"
eq "GİB listesindeki kurumsal → e-Fatura (temel)" "$(istate ein2)" "issued/efatura/turkcell"
eq "e-Fatura modeli: posta kutusu, senaryo" "$(tcreq ein2 "f\"{r['path']}/{r['body']['addressBook']['alias']}/{r['body']['generalInfoModel']['invoiceProfileType']}\"")" "/v1/outboxinvoice/create/urn:mail:defaultpk@kurum2.com/0"
eq "listede olmayan kurumsal → e-Arşiv, VKN ile" "$(istate ein7)/$(tcreq ein7 "r['body']['addressBook']['identificationNumber']")" "issued/earchive/turkcell/1111111112"
eq "422 → kalıcı hata" "$(sql "SELECT status||'/'||(last_error LIKE '%reddetti%')::text FROM invoices WHERE merchant_oid='ein3'")" "failed/true"
eq "geçici hata → beklemede, yeniden denenecek" "$(sql "SELECT status||'/'||attempts||'/'||(next_attempt_at > now() at time zone 'utc')::text FROM invoices WHERE merchant_oid='ein4'")" "pending/1/true"
eq "gönderildi ama yanıt düştü → beklemede" "$(sql "SELECT status FROM invoices WHERE merchant_oid='ein5'")" "pending"
eq "geriye dönük kayıt kesilmez" "$(sql "SELECT status||'/'||attempts FROM invoices WHERE merchant_oid='ein6'")" "pending/0"
sql "UPDATE invoices SET next_attempt_at = NULL WHERE merchant_oid IN ('ein4','ein5');" >/dev/null
sleep 5
eq "geçici hatadan sonra kesildi" "$(istate ein4)" "issued/earchive/turkcell"
eq "1,05 TL: KDV 0,17 (tahsilatla aynı)" "$(python3 -c "
import json
r=[json.loads(l) for l in open('$TC_LOG') if json.loads(l).get('body',{}).get('localReferenceId')=='ein4'][-1]
print(r['body']['invoiceLines'][0]['vatAmount'])")" "0.17"
eq "yanıtı düşen fatura: durumdan tamamlandı" "$(istate ein5)" "issued/earchive/turkcell"
eq "yanıtı düşen fatura: ikinci kez gönderilmedi" "$(grep -c '"localReferenceId": "ein5"' "$TC_LOG")" "1"
ID1=$(sql "SELECT id FROM invoices WHERE merchant_oid='ein1'")
ID3=$(sql "SELECT id FROM invoices WHERE merchant_oid='ein3'")
ID6=$(sql "SELECT id FROM invoices WHERE merchant_oid='ein6'")
H=$(curl -s -D - -o "$WORK/f.pdf" -H "X-Internal-Token: $TOKEN" "$BASE/api/v1/invoices/$ID1/pdf?member_id=40" | tr -d '\r' | grep -i '^content-type' | cut -d' ' -f2)
eq "PDF (sahibi) → application/pdf" "$H/$(head -c 4 "$WORK/f.pdf")" "application/pdf/%PDF"
eq "PDF başka üye → 404" "$(get "/api/v1/invoices/$ID1/pdf?member_id=41")" 404
eq "PDF token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/invoices/$ID1/pdf")" 401
get /api/v1/members/40/invoices >/dev/null
eq "üyenin faturaları" "$(pyj "f\"{len(d['items'])}/{d['items'][0]['has_pdf']}/{d['items'][0]['invoice_no'][:7]}\"")" "1/True/NLK2026"
ACTI='"actor":{"actor_id":1,"actor_email":"admin@x.test"}'
eq "yönetici: yeniden dene → 200" "$(post /api/v1/admin/invoices/$ID3/retry "{$ACTI,\"note\":\"alıcı düzeltildi\"}" $TOKEN)" 200
sleep 4
eq "yeniden denendi (alıcı hâlâ hatalı → yine hata, iki gönderim)" "$(sql "SELECT status FROM invoices WHERE id=$ID3")/$(grep -c '"localReferenceId": "ein3"' "$TC_LOG")" "failed/2"
eq "yönetici: elle kesildi → 200" "$(post /api/v1/admin/invoices/$ID6/manual "{$ACTI,\"invoice_no\":\"ABC2026000000001\"}" $TOKEN)" 200
eq "elle kesildi + numara" "$(sql "SELECT status||'/'||invoice_no FROM invoices WHERE id=$ID6")" "manual/ABC2026000000001"
eq "kesilmiş fatura elle kapatılamaz → 400" "$(post /api/v1/admin/invoices/$ID1/manual "{$ACTI}" $TOKEN)" 400
eq "iz kaydı" "$(sql "SELECT string_agg(action, ',' ORDER BY id) FROM admin_actions WHERE action LIKE 'invoice.%'")" "invoice.retry,invoice.manual"
kill "$TC_PID" 2>/dev/null

echo "== Komisyon oranları (yönetici)"
sql "INSERT INTO paytr_payments(member_id,merchant_oid,amount,status,callback_received_at,created_at) VALUES (3,'fee1','299.00','success',$UTC,$UTC)"
get /api/v1/admin/overview >/dev/null
eq "oran yokken komisyon → null" "$(pyj "d['fee_30d_kurus']")" "None"
eq "komisyon token'sız → 401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/admin/fee-rates")" 401
eq "oran ekle (%2,49 + KDV) → 200" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"2,49\",\"vat_rate\":20,\"effective_from\":\"2026-01-01\",\"note\":\"PayTR sözleşmesi\"}" $TOKEN)" 200
eq "aynı gün ikinci oran → 400" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"2\",\"vat_rate\":20,\"effective_from\":\"2026-01-01\"}" $TOKEN)" 400
eq "oran %20'den büyük → 400" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"25\",\"vat_rate\":20}" $TOKEN)" 400
eq "oran sayı değil → 400" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"abc\",\"vat_rate\":20}" $TOKEN)" 400
eq "sabit ücret negatif → 400" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"2\",\"fixed\":\"-1\",\"vat_rate\":20}" $TOKEN)" 400
eq "ileri tarihli oran → 200" "$(post /api/v1/admin/fee-rates "{$ACTI,\"rate\":\"3\",\"fixed\":\"0,25\",\"vat_rate\":0,\"effective_from\":\"2099-01-01\"}" $TOKEN)" 200
FUTURE_ID=$(pyj "d['id']")
get /api/v1/admin/fee-rates >/dev/null
eq "liste: en yeni önce, geçerli olan işaretli" "$(pyj "'/'.join(f\"{i['rate_percent']}:{i['fixed_kurus']}:{i['current']}\" for i in d['items'])")" "3:25:False/2.49:0:True"
eq "Türkiye gün başı (UTC 21:00)" "$(pyj "d['items'][1]['effective_from']")" "2025-12-31T21:00:00Z"
get '/api/v1/admin/payments?q=fee1' >/dev/null
eq "ödeme komisyonu: 299 TL × %2,49 + KDV = 8,94" "$(pyj "d['items'][0]['fee_kurus']")" "894"
get '/api/v1/admin/payments?status=failed&limit=1' >/dev/null
eq "başarısız ödemede komisyon yok" "$(pyj "d['items'][0]['fee_kurus']")" "None"
eq "çok baytlı uzun arama panik yapmaz" "$(get "/api/v1/admin/payments?q=a$(printf '%%C5%%9F%.0s' $(seq 1 120))")" 200
get /api/v1/admin/overview >/dev/null
eq "özet: komisyon hesaplandı, oransız ödeme yok" "$(pyj "f\"{d['fee_30d_kurus'] >= 894}/{d['fee_30d_uncovered']}\"")" "True/0"
eq "oran sil → 200" "$(post /api/v1/admin/fee-rates/$FUTURE_ID/delete "{$ACTI,\"note\":\"yanlış girildi\"}" $TOKEN)" 200
eq "olmayan oranı sil → 400" "$(post /api/v1/admin/fee-rates/$FUTURE_ID/delete "{$ACTI}" $TOKEN)" 400
eq "iz kaydı (sistem geneli)" "$(sql "SELECT string_agg(action||':'||member_id, ',' ORDER BY id) FROM admin_actions WHERE action LIKE 'fee_rate.%'")" "fee_rate.create:0,fee_rate.create:0,fee_rate.delete:0"

echo "== Sonuç: $PASS geçti, $FAIL başarısız"
[ "$FAIL" = 0 ]
