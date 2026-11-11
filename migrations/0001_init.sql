-- SPDX-License-Identifier: Apache-2.0
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE spins (
  id INTEGER PRIMARY KEY,
  spin_id TEXT UNIQUE NOT NULL,
  number INTEGER NOT NULL CHECK (number BETWEEN 0 AND 36),
  ts INTEGER NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('ws','mo','ext','ocr')),
  received_at INTEGER NOT NULL,
  latency_ms INTEGER
);
CREATE INDEX idx_spins_ts ON spins(ts DESC);

CREATE TABLE intents (
  id INTEGER PRIMARY KEY,
  spin_id TEXT NOT NULL,
  strategy_id TEXT NOT NULL,
  ladder_step INTEGER NOT NULL,
  stake_cents INTEGER NOT NULL,
  ev_cents INTEGER NOT NULL,
  confidence REAL NOT NULL,
  decision TEXT NOT NULL CHECK (decision IN ('placed','skipped','aborted','orphan')),
  reason TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE (strategy_id, spin_id)
);
CREATE INDEX idx_intents_created ON intents(created_at DESC);

CREATE TABLE fills (
  id INTEGER PRIMARY KEY,
  intent_id INTEGER NOT NULL REFERENCES intents(id),
  cover TEXT NOT NULL,
  stake_cents INTEGER NOT NULL,
  acked INTEGER NOT NULL DEFAULT 0,
  confirmed_at INTEGER
);
CREATE INDEX idx_fills_intent ON fills(intent_id);

CREATE TABLE ladder_state (
  strategy_id TEXT PRIMARY KEY,
  step INTEGER NOT NULL DEFAULT 0,
  loss_cents INTEGER NOT NULL DEFAULT 0,
  reset_at INTEGER
);

CREATE TABLE errors (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  detail TEXT,
  ts INTEGER NOT NULL
);
