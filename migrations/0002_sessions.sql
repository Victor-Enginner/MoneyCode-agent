-- SPDX-License-Identifier: Apache-2.0
-- Marco de sessão: a TIMELINE e a janela de padrões só enxergam giros com ts >= started_at
-- da sessão mais recente. O histórico longo (walk-forward, laboratório) continua inteiro.
CREATE TABLE IF NOT EXISTS sessions (
  id INTEGER PRIMARY KEY,
  started_at INTEGER NOT NULL,
  reason TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
