-- Kullanıcı başına PayTR utoken (kart grubunu temsil eder)
CREATE TABLE paytr_user_tokens (
    id         SERIAL PRIMARY KEY,
    member_id  INT         NOT NULL REFERENCES customers(member_id) ON DELETE CASCADE,
    utoken     VARCHAR     NOT NULL UNIQUE,
    is_active  BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at TIMESTAMP   NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP   NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_paytr_user_tokens_member_id ON paytr_user_tokens(member_id);
