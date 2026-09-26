-- Yenileme denemeleri arasında bekleme (günde bir) ve CVV bildirimi tekrarını sınırlamak için.
ALTER TABLE paytr_subscriptions
    ADD COLUMN IF NOT EXISTS last_renewal_attempt_at TIMESTAMP;
