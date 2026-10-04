-- Yönetici işlemleri iz kaydı (üye detay sayfası): kim, ne zaman, hangi üyede, önce/sonra.
-- FK yok: üye anonimleştirilse de kayıt kalır.
CREATE TABLE IF NOT EXISTS admin_actions (
    id          SERIAL       PRIMARY KEY,
    actor_id    INT          NOT NULL,
    actor_email VARCHAR(255),
    member_id   INT          NOT NULL,
    action      VARCHAR(40)  NOT NULL,
    before      JSONB,
    after       JSONB,
    note        TEXT,
    created_at  TIMESTAMP    NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_admin_actions_member ON admin_actions (member_id, id DESC);
