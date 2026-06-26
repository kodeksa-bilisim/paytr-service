ALTER TABLE paytr_subscriptions
  ADD COLUMN IF NOT EXISTS scheduled_plan   VARCHAR,
  ADD COLUMN IF NOT EXISTS scheduled_amount VARCHAR;

ALTER TABLE customers
  ADD COLUMN IF NOT EXISTS scheduled_plan VARCHAR;
