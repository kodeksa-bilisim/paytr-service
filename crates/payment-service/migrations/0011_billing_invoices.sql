-- e-Fatura altyapısı (entegratörden bağımsız kısım).
--
-- billing_profiles: üyenin fatura bilgisi. Kayıt yoksa bireysel sayılır (ad + e-posta
-- customers'tan). Kurumsalda unvan, VKN/TCKN, vergi dairesi ve adres zorunludur (uygulama
-- doğrular). Kişisel veri: üye silinince silinir; kesilmiş faturadaki kopya kalır.
CREATE TABLE IF NOT EXISTS billing_profiles (
    member_id     INT          PRIMARY KEY REFERENCES customers(member_id) ON DELETE CASCADE,
    kind          VARCHAR(12)  NOT NULL CHECK (kind IN ('individual', 'corporate')),
    company_title VARCHAR(250),
    tax_number    VARCHAR(11),
    tax_office    VARCHAR(100),
    address       TEXT,
    city          VARCHAR(60),
    district      VARCHAR(60),
    country       VARCHAR(60)  NOT NULL DEFAULT 'Türkiye',
    created_at    TIMESTAMP    NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMP    NOT NULL DEFAULT NOW()
);

-- invoices: her başarılı ödemenin faturası. Callback'te ödemeyle AYNI transaction'da
-- oluşturulur (tahsilat var, fatura kaydı yok durumu oluşamaz). Mali kayıt: müşteri ya da
-- ödeme satırı silinse de kalmalı (VUK saklama) — bu yüzden FK yok, alıcı bilgisi kopyalanır.
-- Tutarlar kuruş; fiyatlar KDV dahil, KDV hariç tutar = toplam / (1 + oran).
CREATE TABLE IF NOT EXISTS invoices (
    id            SERIAL       PRIMARY KEY,
    payment_id    INT,
    merchant_oid  VARCHAR(64)  NOT NULL,
    member_id     INT          NOT NULL,
    kind          VARCHAR(12)  NOT NULL DEFAULT 'sale' CHECK (kind IN ('sale', 'refund')),
    -- pending: kesilmeyi bekliyor · issued: entegratör kesti · failed: kesilemedi
    -- manual: entegratör dışında (ör. e-belge geçişinden önce) kesildi · cancelled: iptal
    status        VARCHAR(12)  NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'issued', 'failed', 'manual', 'cancelled')),
    buyer         JSONB        NOT NULL,
    lines         JSONB        NOT NULL,
    currency      VARCHAR(5)   NOT NULL DEFAULT 'TRY',
    vat_rate      SMALLINT     NOT NULL,
    net_kurus     BIGINT       NOT NULL,
    vat_kurus     BIGINT       NOT NULL,
    total_kurus   BIGINT       NOT NULL,
    provider      VARCHAR(30),
    provider_ref  VARCHAR(100),
    invoice_no    VARCHAR(50),
    invoice_date  TIMESTAMP,
    pdf_url       TEXT,
    last_error    TEXT,
    created_at    TIMESTAMP    NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMP    NOT NULL DEFAULT NOW(),
    CONSTRAINT uniq_invoice_per_payment UNIQUE (merchant_oid, kind),
    CONSTRAINT invoice_amounts_add_up CHECK (net_kurus + vat_kurus = total_kurus)
);

CREATE INDEX IF NOT EXISTS idx_invoices_status_created ON invoices (status, created_at);
CREATE INDEX IF NOT EXISTS idx_invoices_member ON invoices (member_id);
