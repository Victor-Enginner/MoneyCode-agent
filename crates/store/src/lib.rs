// SPDX-License-Identifier: Apache-2.0
//! Camada de persistência SQLite WAL do projeto MONSTER.
//! Gerencia migrações de schema, inserção de spins/intents/fills e varredura de órfãos.
//! Garante idempotência transacional por `(strategy_id, spin_id)` e retenção periódica.

use serde::{Deserialize, Serialize};
pub use sqlx::SqlitePool;
use sqlx::{params, OptionalExtension};
use thiserror::Error;
use tracing::{error, info, warn};

/// DDL inicial da migração `0001_init.sql`.
pub const MIGRATION_0001_INIT: &str = include_str!("../../../migrations/0001_init.sql");

/// Janela de retenção de spins: 180 dias em milissegundos.
pub const SPINS_RETENTION_MS: i64 = 180 * 24 * 60 * 60 * 1000;

/// Janela de retenção de erros: 30 dias em milissegundos.
pub const ERRORS_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Erros tipados da camada de persistência.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Violação de constraint SQLite (`UNIQUE`, `CHECK`, `FOREIGN KEY`).
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    /// Erro geral de banco de dados.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

impl StoreError {
    fn from_sqlx(err: sqlx::Error) -> Self {
        let msg = err.to_string();
        if msg.contains("UNIQUE constraint failed")
            || msg.contains("CHECK constraint failed")
            || msg.contains("FOREIGN KEY constraint failed")
            || msg.contains("constraint")
        {
            error!(error = %msg, "violação de constraint no SQLite");
            Self::ConstraintViolation(msg)
        } else {
            error!(error = %msg, "erro de operação no SQLite");
            Self::Database(err)
        }
    }
}

/// Fonte emissora de um spin canônico.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpinSource {
    /// WebSocket protobuf primário.
    Ws,
    /// MutationObserver no OOPIF da Playtech.
    Mo,
    /// Extensão Chrome (`all_frames`).
    Ext,
    /// Fallback óptico (OCR).
    Ocr,
}

impl SpinSource {
    /// Representação textual exigida pela constraint `CHECK` do banco.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ws => "ws",
            Self::Mo => "mo",
            Self::Ext => "ext",
            Self::Ocr => "ocr",
        }
    }
}

/// Dados para inserção de um spin canônico.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSpin {
    /// Identificador único monotônico do spin.
    pub spin_id: String,
    /// Número sorteado (`0..=36`).
    pub number: u8,
    /// Timestamp de origem em milissegundos.
    pub ts: i64,
    /// Fonte de captura.
    pub source: SpinSource,
    /// Timestamp de recebimento local em milissegundos.
    pub received_at: i64,
    /// Latência observada em milissegundos.
    pub latency_ms: Option<i64>,
}

/// Estado de decisão de uma intenção de aposta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntentDecision {
    /// Entrada autorizada e enviada ao executor.
    Placed,
    /// Entrada ignorada (janela curta ou cooldown).
    Skipped,
    /// Entrada abortada por limite de risco ou preview divergente.
    Aborted,
    /// Intenção sem fill confirmado detectada no boot.
    Orphan,
}

impl IntentDecision {
    /// Representação textual exigida pela constraint `CHECK` do banco.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Placed => "placed",
            Self::Skipped => "skipped",
            Self::Aborted => "aborted",
            Self::Orphan => "orphan",
        }
    }
}

/// Dados para inserção de uma intenção de aposta (`intents`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewIntent {
    /// Identificador do spin que originou a decisão.
    pub spin_id: String,
    /// Identificador da estratégia.
    pub strategy_id: String,
    /// Degrau da escada (`1..=max_steps`).
    pub ladder_step: u8,
    /// Valor em centavos inteiros.
    pub stake_cents: u64,
    /// Valor esperado em centavos inteiros (pode ser negativo).
    pub ev_cents: i64,
    /// Confiança estatística (`0.0..=1.0`).
    pub confidence: f64,
    /// Decisão tomada pelo gate.
    pub decision: IntentDecision,
    /// Justificativa opcional da decisão.
    pub reason: Option<String>,
    /// Timestamp de criação em milissegundos.
    pub created_at: i64,
}

/// Dados para inserção de uma execução de cobertura (`fills`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFill {
    /// Chave estrangeira para `intents.id`.
    pub intent_id: i64,
    /// Cobertura executada (ex.: `col:2`, `col:3`).
    pub cover: String,
    /// Valor apostado na cobertura em centavos inteiros.
    pub stake_cents: u64,
    /// Indicador se o fill foi confirmado pela mesa.
    pub acked: bool,
    /// Timestamp de confirmação em milissegundos.
    pub confirmed_at: Option<i64>,
}

/// Resultado da rotina diária de retenção.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionReport {
    /// Quantidade de spins antigos removidos (> 180 dias).
    pub deleted_spins: u64,
    /// Quantidade de erros antigos removidos (> 30 dias).
    pub deleted_errors: u64,
}

/// Estado persistido da escada por estratégia (`ladder_state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderRow {
    /// Identificador da estratégia.
    pub strategy_id: String,
    /// Degrau atual (`0..=max_steps`).
    pub step: u8,
    /// Perda acumulada atual na escada em centavos inteiros.
    pub loss_cents: u64,
    /// Timestamp do último reset da escada em milissegundos.
    pub reset_at: Option<i64>,
}

/// Inicializa um `SqlitePool` em modo WAL e aplica a migração inicial no boot.
pub fn init_pool(uri: &str) -> Result<SqlitePool, StoreError> {
    let pool = SqlitePool::connect_sync(uri).map_err(StoreError::from_sqlx)?;
    migrate(&pool)?;
    Ok(pool)
}

/// Aplica o schema DDL (`migrations/0001_init.sql`) e ativa `WAL` + `FOREIGN KEYS`.
pub fn migrate(pool: &SqlitePool) -> Result<(), StoreError> {
    pool.with_conn(|conn| {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='spins'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            conn.execute_batch(MIGRATION_0001_INIT)?;
            info!("schema 0001_init.sql aplicado com sucesso");
        }
        Ok(())
    })
    .map_err(StoreError::from_sqlx)
}

/// Insere um spin canônico em `spins` retornando o `rowid` gerado.
pub fn insert_spin(pool: &SqlitePool, spin: &NewSpin) -> Result<i64, StoreError> {
    pool.with_conn(|conn| {
        conn.execute(
            "INSERT INTO spins (spin_id, number, ts, source, received_at, latency_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                spin.spin_id,
                i64::from(spin.number),
                spin.ts,
                spin.source.as_str(),
                spin.received_at,
                spin.latency_ms,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .map_err(StoreError::from_sqlx)
}

/// Insere uma intenção em `intents` (antes do clique) retornando o `id` gerado.
pub fn insert_intent(pool: &SqlitePool, intent: &NewIntent) -> Result<i64, StoreError> {
    let stake_i64 = i64::try_from(intent.stake_cents).map_err(|_| {
        StoreError::ConstraintViolation("stake_cents excede i64::MAX".to_string())
    })?;
    pool.with_conn(|conn| {
        conn.execute(
            "INSERT INTO intents (spin_id, strategy_id, ladder_step, stake_cents, ev_cents, confidence, decision, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                intent.spin_id,
                intent.strategy_id,
                i64::from(intent.ladder_step),
                stake_i64,
                intent.ev_cents,
                intent.confidence,
                intent.decision.as_str(),
                intent.reason,
                intent.created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .map_err(StoreError::from_sqlx)
}

/// Insere um fill de cobertura em `fills` retornando o `id` gerado.
pub fn insert_fill(pool: &SqlitePool, fill: &NewFill) -> Result<i64, StoreError> {
    let stake_i64 = i64::try_from(fill.stake_cents).map_err(|_| {
        StoreError::ConstraintViolation("stake_cents excede i64::MAX".to_string())
    })?;
    let acked_int: i64 = if fill.acked { 1 } else { 0 };
    pool.with_conn(|conn| {
        conn.execute(
            "INSERT INTO fills (intent_id, cover, stake_cents, acked, confirmed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                fill.intent_id,
                fill.cover,
                stake_i64,
                acked_int,
                fill.confirmed_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .map_err(StoreError::from_sqlx)
}

/// Varre `intents` com decisão `placed` que não possuem fill confirmado (`acked = 1`)
/// e marca-as como `orphan` para reconciliação no boot.
pub fn orphan_sweep(pool: &SqlitePool) -> Result<u64, StoreError> {
    pool.with_conn(|conn| {
        let updated = conn.execute(
            "UPDATE intents
             SET decision = 'orphan',
                 reason = COALESCE(reason, 'orphan_sweep_no_confirmed_fill')
             WHERE decision = 'placed'
               AND NOT EXISTS (
                   SELECT 1 FROM fills f
                   WHERE f.intent_id = intents.id
                     AND f.acked = 1
                     AND f.confirmed_at IS NOT NULL
               )",
            [],
        )?;
        if updated > 0 {
            warn!(orphans = updated, "intents sem fill confirmado marcadas como orphan");
        }
        Ok(updated as u64)
    })
    .map_err(StoreError::from_sqlx)
}

/// Executa o job diário de retenção removendo `spins` > 180 dias e `errors` > 30 dias.
pub fn retention_job(pool: &SqlitePool, now_ms: i64) -> Result<RetentionReport, StoreError> {
    let min_spin_ts = now_ms.saturating_sub(SPINS_RETENTION_MS);
    let min_err_ts = now_ms.saturating_sub(ERRORS_RETENTION_MS);
    pool.with_conn(|conn| {
        let deleted_spins = conn.execute("DELETE FROM spins WHERE ts < ?1", params![min_spin_ts])?;
        let deleted_errors =
            conn.execute("DELETE FROM errors WHERE ts < ?1", params![min_err_ts])?;
        info!(
            deleted_spins,
            deleted_errors, "retention_job concluído com sucesso"
        );
        Ok(RetentionReport {
            deleted_spins: deleted_spins as u64,
            deleted_errors: deleted_errors as u64,
        })
    })
    .map_err(StoreError::from_sqlx)
}

/// Atualiza a decisão e justificativa de uma intenção existente (ex.: `aborted` por `preview_mismatch`).
pub fn update_intent_decision(
    pool: &SqlitePool,
    intent_id: i64,
    decision: IntentDecision,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    pool.with_conn(|conn| {
        conn.execute(
            "UPDATE intents SET decision = ?1, reason = ?2 WHERE id = ?3",
            params![decision.as_str(), reason, intent_id],
        )?;
        Ok(())
    })
    .map_err(StoreError::from_sqlx)
}

/// Persiste ou atualiza o estado da escada (`ladder_state`) de uma estratégia.
pub fn upsert_ladder_state(
    pool: &SqlitePool,
    strategy_id: &str,
    step: u8,
    loss_cents: u64,
    reset_at: Option<i64>,
) -> Result<(), StoreError> {
    let loss_i64 = i64::try_from(loss_cents).map_err(|_| {
        StoreError::ConstraintViolation("loss_cents excede i64::MAX".to_string())
    })?;
    pool.with_conn(|conn| {
        conn.execute(
            "INSERT INTO ladder_state (strategy_id, step, loss_cents, reset_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(strategy_id) DO UPDATE SET
               step = excluded.step,
               loss_cents = excluded.loss_cents,
               reset_at = excluded.reset_at",
            params![strategy_id, i64::from(step), loss_i64, reset_at],
        )?;
        Ok(())
    })
    .map_err(StoreError::from_sqlx)
}

/// Lê o degrau de TODAS as estratégias com escada registrada (`ladder_state`).
/// A escada é por estratégia (§5.3): sem isso o processo reiniciava no degrau 0 e a
/// primeira entrada de cada padrão herdava o degrau de outra estratégia.
pub fn all_ladder_steps(pool: &SqlitePool) -> Result<Vec<(String, u8)>, StoreError> {
    pool.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT strategy_id, step FROM ladder_state")?;
        let rows = stmt.query_map([], |r| {
            let step = u8::try_from(r.get::<_, i64>(1)?.max(0)).unwrap_or(0);
            Ok((r.get::<_, String>(0)?, step))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
    .map_err(StoreError::from_sqlx)
}

/// Lê o estado da escada (`ladder_state`) de uma estratégia, retornando degrau 0 caso ainda não exista.
pub fn get_ladder_state(pool: &SqlitePool, strategy_id: &str) -> Result<LadderRow, StoreError> {
    pool.with_conn(|conn| {
        let row: Option<(i64, i64, Option<i64>)> = conn
            .query_row(
                "SELECT step, loss_cents, reset_at FROM ladder_state WHERE strategy_id = ?1",
                params![strategy_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((step, loss, reset_at)) => Ok(LadderRow {
                strategy_id: strategy_id.to_string(),
                step: u8::try_from(step.max(0)).unwrap_or(0),
                loss_cents: u64::try_from(loss.max(0)).unwrap_or(0),
                reset_at,
            }),
            None => Ok(LadderRow {
                strategy_id: strategy_id.to_string(),
                step: 0,
                loss_cents: 0,
                reset_at: None,
            }),
        }
    })
    .map_err(StoreError::from_sqlx)
}

/// Registra um evento na tabela `errors`.
pub fn insert_error(
    pool: &SqlitePool,
    kind: &str,
    detail: &str,
    ts: i64,
) -> Result<i64, StoreError> {
    pool.with_conn(|conn| {
        conn.execute(
            "INSERT INTO errors (kind, detail, ts) VALUES (?1, ?2, ?3)",
            params![kind, detail, ts],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .map_err(StoreError::from_sqlx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_spin_id_triggers_constraint_error() {
        let pool = init_pool(":memory:").expect("init_pool in-memory");
        for idx in 1..=3u8 {
            let spin = NewSpin {
                spin_id: format!("spin-{idx}"),
                number: idx * 5,
                ts: 1_730_000_000_000 + i64::from(idx) * 1000,
                source: SpinSource::Ws,
                received_at: 1_730_000_000_050 + i64::from(idx) * 1000,
                latency_ms: Some(50),
            };
            let id = insert_spin(&pool, &spin).expect("insert valid spin");
            assert_eq!(id, i64::from(idx));
        }

        let dup = NewSpin {
            spin_id: "spin-2".to_string(),
            number: 17,
            ts: 1_730_000_010_000,
            source: SpinSource::Mo,
            received_at: 1_730_000_010_080,
            latency_ms: Some(80),
        };
        let err = insert_spin(&pool, &dup).expect_err("duplicate spin_id must fail");
        assert!(matches!(err, StoreError::ConstraintViolation(_)));
    }

    #[test]
    fn orphan_sweep_marks_unconfirmed_intents_only() {
        let pool = init_pool(":memory:").expect("init_pool in-memory");
        let orphan_intent_id = insert_intent(
            &pool,
            &NewIntent {
                spin_id: "s-100".to_string(),
                strategy_id: "quebra_permanencia_coluna".to_string(),
                ladder_step: 1,
                stake_cents: 20,
                ev_cents: -1,
                confidence: 0.75,
                decision: IntentDecision::Placed,
                reason: None,
                created_at: 1_730_000_001_000,
            },
        )
        .expect("insert orphan candidate");

        let confirmed_intent_id = insert_intent(
            &pool,
            &NewIntent {
                spin_id: "s-101".to_string(),
                strategy_id: "quebra_permanencia_coluna".to_string(),
                ladder_step: 2,
                stake_cents: 60,
                ev_cents: -2,
                confidence: 0.82,
                decision: IntentDecision::Placed,
                reason: None,
                created_at: 1_730_000_002_000,
            },
        )
        .expect("insert confirmed candidate");

        insert_fill(
            &pool,
            &NewFill {
                intent_id: orphan_intent_id,
                cover: "col:2".to_string(),
                stake_cents: 10,
                acked: false,
                confirmed_at: None,
            },
        )
        .expect("insert unacked fill");

        insert_fill(
            &pool,
            &NewFill {
                intent_id: confirmed_intent_id,
                cover: "col:2".to_string(),
                stake_cents: 30,
                acked: true,
                confirmed_at: Some(1_730_000_002_200),
            },
        )
        .expect("insert acked fill");

        let swept = orphan_sweep(&pool).expect("run orphan_sweep");
        assert_eq!(swept, 1);

        let decision_orphan: String = pool
            .with_conn(|c| {
                c.query_row(
                    "SELECT decision FROM intents WHERE id = ?1",
                    params![orphan_intent_id],
                    |r| r.get(0),
                )
            })
            .expect("query orphan decision");
        assert_eq!(decision_orphan, "orphan");

        let decision_confirmed: String = pool
            .with_conn(|c| {
                c.query_row(
                    "SELECT decision FROM intents WHERE id = ?1",
                    params![confirmed_intent_id],
                    |r| r.get(0),
                )
            })
            .expect("query confirmed decision");
        assert_eq!(decision_confirmed, "placed");
    }

    #[test]
    fn retention_job_purges_expired_spins_and_errors() {
        let pool = init_pool(":memory:").expect("init_pool in-memory");
        let now_ms: i64 = 2_000_000_000_000;

        insert_spin(
            &pool,
            &NewSpin {
                spin_id: "old-spin".to_string(),
                number: 0,
                ts: now_ms - SPINS_RETENTION_MS - 1_000,
                source: SpinSource::Ws,
                received_at: now_ms - SPINS_RETENTION_MS,
                latency_ms: Some(10),
            },
        )
        .expect("insert old spin");

        insert_spin(
            &pool,
            &NewSpin {
                spin_id: "fresh-spin".to_string(),
                number: 36,
                ts: now_ms - 60_000,
                source: SpinSource::Ws,
                received_at: now_ms - 59_950,
                latency_ms: Some(50),
            },
        )
        .expect("insert fresh spin");

        pool.with_conn(|c| {
            c.execute(
                "INSERT INTO errors (kind, detail, ts) VALUES ('cdp', 'old timeout', ?1)",
                params![now_ms - ERRORS_RETENTION_MS - 500],
            )
        })
        .expect("insert old error");

        let report = retention_job(&pool, now_ms).expect("run retention_job");
        assert_eq!(report.deleted_spins, 1);
        assert_eq!(report.deleted_errors, 1);
    }
}
