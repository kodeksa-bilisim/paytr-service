CREATE TABLE paytr_subscriptions (
    id                SERIAL      PRIMARY KEY,
    member_id         INT         NOT NULL REFERENCES customers(member_id) ON DELETE CASCADE,
    plan              VARCHAR(50) NOT NULL,   -- gold, silver, standard
    status            VARCHAR(20) NOT NULL DEFAULT 'pending',  -- pending, active, cancelled, expired
    utoken            VARCHAR,
    ctoken            VARCHAR,
    billing_cycle     VARCHAR(20) NOT NULL DEFAULT 'monthly',  -- monthly, yearly
    amount            VARCHAR     NOT NULL,
    currency          VARCHAR(5)  NOT NULL DEFAULT 'TL',
    started_at        TIMESTAMP,
    expires_at        TIMESTAMP,
    next_payment_date TIMESTAMP,
    cancelled_at      TIMESTAMP,
    created_at        TIMESTAMP   NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMP   NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_paytr_subscriptions_member_id ON paytr_subscriptions(member_id);
