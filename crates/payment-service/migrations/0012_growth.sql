-- Büyüme: kupon/indirim kodu, referans programı ve kredi bakiyesi.
-- Ücretsiz deneme ayrı tablo kullanmaz: `paytr_subscriptions.metadata.trial = true` (kartsız, tutarsız).

-- İndirim kodu. `value`: kind='percent' → 1..100 (%), kind='fixed' → kuruş.
-- `duration_cycles`: kaç ödemede geçerli (1 = yalnızca ilk ödeme, NULL = süresiz).
CREATE TABLE IF NOT EXISTS coupons (
    code             VARCHAR(40)  PRIMARY KEY,
    kind             VARCHAR(10)  NOT NULL CHECK (kind IN ('percent', 'fixed')),
    value            INT          NOT NULL CHECK (value > 0),
    plans            TEXT[],
    billing_cycles   TEXT[],
    duration_cycles  INT          CHECK (duration_cycles IS NULL OR duration_cycles > 0),
    max_redemptions  INT          CHECK (max_redemptions IS NULL OR max_redemptions > 0),
    redemptions      INT          NOT NULL DEFAULT 0,
    valid_until      TIMESTAMP,
    active           BOOLEAN      NOT NULL DEFAULT TRUE,
    note             TEXT,
    created_at       TIMESTAMP    NOT NULL DEFAULT NOW(),
    CONSTRAINT coupon_percent_range CHECK (kind <> 'percent' OR value <= 100),
    CONSTRAINT coupon_code_format CHECK (code ~ '^[A-Z0-9_-]{3,40}$')
);

-- Kullanım: ilk ödeme başarıyla tamamlanınca yazılır (üye başına bir kez).
CREATE TABLE IF NOT EXISTS coupon_redemptions (
    id              SERIAL       PRIMARY KEY,
    code            VARCHAR(40)  NOT NULL REFERENCES coupons(code),
    member_id       INT          NOT NULL,
    subscription_id INT,
    merchant_oid    VARCHAR(64)  NOT NULL UNIQUE,
    discount_kurus  BIGINT       NOT NULL,
    created_at      TIMESTAMP    NOT NULL DEFAULT NOW(),
    CONSTRAINT one_redemption_per_member UNIQUE (code, member_id)
);

-- Üyenin referans kodu (ilk istendiğinde üretilir).
CREATE TABLE IF NOT EXISTS referral_codes (
    member_id  INT          PRIMARY KEY,
    code       VARCHAR(16)  NOT NULL UNIQUE,
    created_at TIMESTAMP    NOT NULL DEFAULT NOW()
);

-- Davet edilen üye → davet eden. İlk başarılı ödemeden 14 gün sonra davet eden ödüllendirilir.
CREATE TABLE IF NOT EXISTS referrals (
    referred_id        INT          PRIMARY KEY,
    referrer_id        INT          NOT NULL,
    created_at         TIMESTAMP    NOT NULL DEFAULT NOW(),
    first_paid_at      TIMESTAMP,
    first_payment_oid  VARCHAR(64),
    rewarded_at        TIMESTAMP,
    -- 'extension' (abonelik +1 ay) | 'credit' (bakiye) | 'capped' (yıllık sınır) | 'void' (ödeme geçersiz)
    reward             VARCHAR(12),
    CONSTRAINT no_self_referral CHECK (referred_id <> referrer_id)
);
CREATE INDEX IF NOT EXISTS idx_referrals_referrer ON referrals (referrer_id);
CREATE INDEX IF NOT EXISTS idx_referrals_due ON referrals (first_paid_at) WHERE rewarded_at IS NULL;

-- Kredi hareketleri (kuruş): + kazanılan (referans), − kullanılan (ödemede düşülen).
CREATE TABLE IF NOT EXISTS member_credits (
    id           SERIAL       PRIMARY KEY,
    member_id    INT          NOT NULL,
    amount_kurus BIGINT       NOT NULL,
    reason       VARCHAR(20)  NOT NULL,
    ref          VARCHAR(64),
    created_at   TIMESTAMP    NOT NULL DEFAULT NOW(),
    CONSTRAINT one_credit_use_per_payment UNIQUE (reason, ref)
);
CREATE INDEX IF NOT EXISTS idx_member_credits_member ON member_credits (member_id);
