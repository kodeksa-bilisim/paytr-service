-- Abonelik başına en fazla bir bekleyen (pending) ödeme: iki scheduler süreci ya da yarış aynı
-- aboneliği iki kez çekemesin. Önce varsa eski kopyalar kapatılır (en yenisi pending kalır;
-- kapatılanın başarılı callback'i gelirse yine işlenir).
UPDATE paytr_payments p
SET status = 'failed', failed_reason_msg = 'duplicate_pending_0009'
WHERE p.status = 'pending'
  AND p.subscription_id IS NOT NULL
  AND EXISTS (
        SELECT 1 FROM paytr_payments q
        WHERE q.subscription_id = p.subscription_id AND q.status = 'pending' AND q.id > p.id
      );

CREATE UNIQUE INDEX IF NOT EXISTS uniq_paytr_payments_pending_per_subscription
    ON paytr_payments (subscription_id)
    WHERE status = 'pending';
