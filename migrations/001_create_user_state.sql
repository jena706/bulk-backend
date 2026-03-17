-- migrations/001_create_user_state.sql
CREATE TABLE IF NOT EXISTS user_state (
    pubkey          TEXT        PRIMARY KEY,
    daily_loss      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    daily_preds     INTEGER     NOT NULL DEFAULT 0,
    last_pred_time  BIGINT      NOT NULL DEFAULT 0,
    prediction_history JSONB   NOT NULL DEFAULT '[]'::jsonb,
    risk_settings   JSONB       NOT NULL DEFAULT '{
        "maxTrade": 200,
        "dailyLoss": 500,
        "maxPreds": 10,
        "cooldown": 120
    }'::jsonb,
    daily_reset_at  DATE        NOT NULL DEFAULT CURRENT_DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_user_state_updated ON user_state (updated_at);

-- Auto-update updated_at on every row change
CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER user_state_updated_at
    BEFORE UPDATE ON user_state
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- Prediction verifications table (tamper-proof server-side record)
CREATE TABLE IF NOT EXISTS prediction_verifications (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    pubkey          TEXT        NOT NULL,
    coin            TEXT        NOT NULL,
    direction       TEXT        NOT NULL CHECK (direction IN ('UP', 'DOWN')),
    entry_price     DOUBLE PRECISION NOT NULL,
    exit_price      DOUBLE PRECISION,
    trade_size_usdt DOUBLE PRECISION NOT NULL,
    won             BOOLEAN,
    resolved_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_verif_pubkey ON prediction_verifications (pubkey);
CREATE INDEX IF NOT EXISTS idx_verif_created ON prediction_verifications (created_at);
