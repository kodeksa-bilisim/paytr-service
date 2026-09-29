-- Mali kayıtların saklanması: `customers` satırı silinince ödemeler ON DELETE CASCADE ile
-- siliniyordu (VUK saklama yükümlülüğü). Silinen her ödeme satırı önce arşive kopyalanır;
-- mevcut silme akışları (hesap silme) değişmeden çalışmaya devam eder.
-- DİKKAT: paytr_payments'a kolon eklenirse aynı kolon arşive de eklenmeli (OLD.* sırası).
CREATE TABLE IF NOT EXISTS paytr_payments_archive (
    LIKE paytr_payments INCLUDING DEFAULTS,
    archived_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE OR REPLACE FUNCTION paytr_payments_archive_on_delete() RETURNS trigger AS $$
BEGIN
    INSERT INTO paytr_payments_archive SELECT OLD.*, NOW();
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_paytr_payments_archive ON paytr_payments;
CREATE TRIGGER trg_paytr_payments_archive
    BEFORE DELETE ON paytr_payments
    FOR EACH ROW EXECUTE FUNCTION paytr_payments_archive_on_delete();
