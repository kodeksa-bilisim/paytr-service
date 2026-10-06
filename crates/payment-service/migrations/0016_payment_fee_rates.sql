-- Ödeme kuruluşu komisyon oranları (yönetici panelinden girilir). Oranlar sık değiştiği için
-- güncellenmez, geçerlilik tarihli yeni satır eklenir: bir ödemenin komisyonu, ödeme anında
-- geçerli olan (effective_from <= ödeme zamanı olan en yeni) satırla hesaplanır.
-- Tahmindir: gerçek kesinti PayTR'nin hesap özetindedir; callback komisyon taşımaz.
CREATE TABLE IF NOT EXISTS payment_fee_rates (
    id               SERIAL       PRIMARY KEY,
    provider         VARCHAR(20)  NOT NULL DEFAULT 'paytr',
    -- UTC
    effective_from   TIMESTAMP    NOT NULL,
    -- Yüzde oran, milyonda bir: %2,49 = 24900
    rate_ppm         INT          NOT NULL CHECK (rate_ppm BETWEEN 0 AND 200000),
    -- İşlem başına sabit ücret
    fixed_kurus      INT          NOT NULL DEFAULT 0 CHECK (fixed_kurus BETWEEN 0 AND 100000),
    -- Komisyona eklenen KDV (%); oran zaten KDV dahilse 0
    vat_rate         SMALLINT     NOT NULL DEFAULT 20 CHECK (vat_rate BETWEEN 0 AND 50),
    note             TEXT,
    created_by       INT,
    created_by_email VARCHAR(255),
    created_at       TIMESTAMP    NOT NULL DEFAULT NOW(),
    UNIQUE (provider, effective_from)
);
