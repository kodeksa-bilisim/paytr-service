-- e-Arşiv / e-Fatura kesimi (Turkcell e-Şirket). Kayıtlar callback'te `pending` açılır; arka plan
-- görevi entegratöre gönderir. ETTN gönderimden ÖNCE yazılır: süreç gönderimden sonra düşse bile
-- yeniden denemede aynı ETTN sorgulanır, ikinci fatura kesilmez.
ALTER TABLE invoices
    ADD COLUMN IF NOT EXISTS source          VARCHAR(20),
    ADD COLUMN IF NOT EXISTS doc_type        VARCHAR(10) CHECK (doc_type IN ('earchive', 'efatura')),
    ADD COLUMN IF NOT EXISTS ettn            UUID,
    ADD COLUMN IF NOT EXISTS provider_status SMALLINT,
    ADD COLUMN IF NOT EXISTS attempts        INT       NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS next_attempt_at TIMESTAMP,
    ADD COLUMN IF NOT EXISTS issued_at       TIMESTAMP,
    ADD COLUMN IF NOT EXISTS checked_at      TIMESTAMP;

-- Kaydın kaynağı (geriye dönük kayıt) eskiden `provider` sütununa yazılıyordu.
UPDATE invoices SET source = provider, provider = NULL WHERE provider = 'backfill';

CREATE UNIQUE INDEX IF NOT EXISTS uniq_invoices_ettn ON invoices (ettn) WHERE ettn IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_invoices_issue_queue ON invoices (next_attempt_at) WHERE status = 'pending';

-- GİB e-Fatura kullanıcı listesi (alıcı posta kutuları): VKN/TCKN başına bir etkin `pk` etiketi.
-- Günlük yenilenir; alıcı bu listedeyse e-Fatura, değilse e-Arşiv kesilir.
CREATE TABLE IF NOT EXISTS einvoice_users (
    identifier VARCHAR(11) PRIMARY KEY,
    alias      TEXT        NOT NULL,
    synced_at  TIMESTAMP   NOT NULL DEFAULT NOW()
);
