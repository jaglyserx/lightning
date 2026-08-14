ALTER TABLE apps
    ADD COLUMN generation bigint NOT NULL DEFAULT 1,
    ADD COLUMN reconciled_generation bigint NOT NULL DEFAULT 0,
    ADD COLUMN last_reconciled_at timestamptz;

CREATE INDEX apps_reconciliation_idx
    ON apps (reconciled_generation, generation, last_reconciled_at);

