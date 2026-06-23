CREATE TABLE paytr_payments (
    id                   SERIAL      PRIMARY KEY,
    member_id            INT         NOT NULL REFERENCES customers(member_id) ON DELETE CASCADE,
    subscription_id      INT         REFERENCES paytr_subscriptions(id),
    merchant_oid         VARCHAR(64) NOT NULL UNIQUE,
    amount               VARCHAR     NOT NULL,
    currency             VARCHAR(5)  NOT NULL DEFAULT 'TL',
    status               VARCHAR(20) NOT NULL DEFAULT 'pending',  -- pending, success, failed
    payment_type         VARCHAR(20) NOT NULL DEFAULT 'card',
    installment_count    INT         NOT NULL DEFAULT 0,
    is_3d                BOOLEAN     NOT NULL DEFAULT TRUE,
    test_mode            BOOLEAN     NOT NULL DEFAULT FALSE,
    failed_reason_code   VARCHAR(10),
    failed_reason_msg    TEXT,
    utoken               VARCHAR,
    ctoken               VARCHAR,
    callback_received_at TIMESTAMP,
    created_at           TIMESTAMP   NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_paytr_payments_member_id     ON paytr_payments(member_id);
CREATE INDEX idx_paytr_payments_subscription  ON paytr_payments(subscription_id);
