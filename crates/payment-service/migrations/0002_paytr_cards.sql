-- utoken altındaki bireysel kartlar (ctoken bazlı)
CREATE TABLE paytr_cards (
    id           SERIAL      PRIMARY KEY,
    utoken       VARCHAR     NOT NULL REFERENCES paytr_user_tokens(utoken) ON DELETE CASCADE,
    ctoken       VARCHAR     NOT NULL UNIQUE,
    last_4       VARCHAR(4)  NOT NULL,
    card_bank    VARCHAR(100),
    card_schema  VARCHAR(20),   -- VISA, MASTERCARD, TROY, AMEX
    card_type    VARCHAR(10),   -- credit, debit
    expiry_month VARCHAR(2)  NOT NULL,
    expiry_year  VARCHAR(2)  NOT NULL,
    require_cvv  BOOLEAN     NOT NULL DEFAULT FALSE,
    is_default   BOOLEAN     NOT NULL DEFAULT FALSE,
    is_active    BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMP   NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_paytr_cards_utoken ON paytr_cards(utoken);
