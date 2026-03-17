# bulk-backend

Rust/Axum API for the Bulk Trade Recovery Predictor.
Persists user state, daily counters, risk settings, and prediction verifications.

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness probe |
| GET | `/state/:pubkey` | Load user state (auto-resets daily counters at midnight) |
| POST | `/state/:pubkey` | Save user state after each prediction |
| POST | `/verify` | Record a prediction server-side at entry time |
| POST | `/resolve` | Record the result after the 60s window closes |

## Local development

### 1. Start Postgres

```bash
docker run --name bulk-pg -e POSTGRES_PASSWORD=password -p 5432:5432 -d postgres:16
```

### 2. Create the database

```bash
psql postgres://postgres:password@localhost:5432/postgres -c "CREATE DATABASE bulk_backend;"
```

### 3. Configure environment

```bash
cp .env.example .env
# Edit .env — the defaults work for the docker container above
```

### 4. Run

```bash
cargo run
# Server starts on http://localhost:8080
curl http://localhost:8080/health
```

---

## Deploy to Railway (recommended — free tier available)

### 1. Create a Railway account at railway.app

### 2. Install the Railway CLI

```bash
npm install -g @railway/cli
railway login
```

### 3. Create a new project

```bash
cd bulk-backend
railway init
# Choose "Empty project"
```

### 4. Add a Postgres database

In the Railway dashboard: New → Database → PostgreSQL.
Copy the `DATABASE_URL` from the Variables tab.

### 5. Set environment variables

```bash
railway variables set DATABASE_URL="postgres://..."
railway variables set FRONTEND_URL="https://your-app.vercel.app"
railway variables set RUST_LOG="bulk_backend=info"
```

### 6. Deploy

```bash
railway up
```

Railway detects the Dockerfile automatically. First build takes ~3 minutes.
Your API will be live at `https://bulk-backend-production.up.railway.app`.

---

## Deploy to Fly.io (alternative)

### 1. Install flyctl and login

```bash
brew install flyctl
fly auth login
```

### 2. Launch

```bash
fly launch --name bulk-backend --region ord --no-deploy
```

### 3. Add Postgres

```bash
fly postgres create --name bulk-pg --region ord
fly postgres attach bulk-pg
# This sets DATABASE_URL automatically
```

### 4. Set remaining env vars

```bash
fly secrets set FRONTEND_URL="https://your-app.vercel.app"
fly secrets set RUST_LOG="bulk_backend=info"
```

### 5. Deploy

```bash
fly deploy
```

---

## Wire the frontend

In `bulk-recovery-app.html`, add this near the top of the `<script>` section:

```js
const BACKEND_URL = 'https://your-backend.up.railway.app';
```

Then add these two calls:

**On wallet connect** — restore state:
```js
async function loadUserState(pubkey) {
  const r = await fetch(`${BACKEND_URL}/state/${pubkey}`);
  const { data } = await r.json();
  if (data) {
    daily.loss          = data.daily_loss;
    daily.preds         = data.daily_preds;
    daily.lastPredTime  = data.last_pred_time;
    predHistory         = data.prediction_history;
    risk                = data.risk_settings;
    pStats.rec          = predHistory.filter(p => p.won).reduce((s,p) => s+p.sz, 0);
    pStats.total        = predHistory.length;
    pStats.wins         = predHistory.filter(p => p.won).length;
    updatePStats();
    renderPredHistory();
    updateRiskUI();
  }
}
```

**After each prediction resolves** — save state:
```js
async function saveUserState(pubkey) {
  await fetch(`${BACKEND_URL}/state/${pubkey}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      daily_loss:         daily.loss,
      daily_preds:        daily.preds,
      last_pred_time:     daily.lastPredTime,
      prediction_history: predHistory.slice(0, 100),
      risk_settings:      risk,
    })
  });
}
```

**At prediction submit** — record server-side:
```js
async function recordPrediction(pubkey, coin, direction, entryPrice, tradeSzUSDT) {
  const r = await fetch(`${BACKEND_URL}/verify`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ pubkey, coin, direction,
      entry_price: entryPrice, trade_size_usdt: tradeSzUSDT })
  });
  const { data } = await r.json();
  return data?.id; // store this, pass it to /resolve
}
```

**After result** — resolve server-side:
```js
async function resolvePrediction(verificationId, exitPrice, won) {
  await fetch(`${BACKEND_URL}/resolve`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: verificationId, exit_price: exitPrice, won })
  });
}
```

---

## Database schema

```
user_state
├── pubkey              TEXT PRIMARY KEY   (Phantom wallet public key)
├── daily_loss          FLOAT              (resets at midnight)
├── daily_preds         INT                (resets at midnight)
├── last_pred_time      BIGINT             (ms timestamp of last prediction)
├── prediction_history  JSONB              (last 100 predictions)
├── risk_settings       JSONB              (maxTrade, dailyLoss, maxPreds, cooldown)
├── daily_reset_at      DATE               (last reset date — triggers auto-reset)
├── created_at          TIMESTAMPTZ
└── updated_at          TIMESTAMPTZ        (auto-updated by trigger)

prediction_verifications
├── id              UUID PRIMARY KEY
├── pubkey          TEXT
├── coin            TEXT
├── direction       TEXT (UP | DOWN)
├── entry_price     FLOAT
├── exit_price      FLOAT (null until resolved)
├── trade_size_usdt FLOAT
├── won             BOOLEAN (null until resolved)
├── resolved_at     TIMESTAMPTZ (null until resolved)
└── created_at      TIMESTAMPTZ
```
