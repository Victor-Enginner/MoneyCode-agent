// SPDX-License-Identifier: Apache-2.0
//! Reconciliador pós-spin e de inicialização (boot) do projeto MONSTER (§9).
//! Limpa `SingletonLock` órfão no boot, executa `orphan_sweep` antes de novas entradas
//! e reconcilia `fills` contra o resultado do spin e contra o extrato da banca.

use crate::executor::{CoverAck, ExecError};
use monster_risk::Cfg;
use monster_store::{
    get_ladder_state, insert_error, orphan_sweep, upsert_ladder_state, SqlitePool,
};
use monster_strategy_native::Cover;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};

/// Relatório da reconciliação de boot (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootReconcileReport {
    /// Quantidade de intents não confirmadas marcadas como `orphan`.
    pub orphans_swept: u64,
    /// Indica se um arquivo `SingletonLock` órfão do Chrome foi removido.
    pub singleton_lock_removed: bool,
}

/// Relatório da reconciliação pós-spin (`reconciler.confirm` — §8 e §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Estratégia reconciliada.
    pub strategy_id: String,
    /// Número sorteado (`0..=36`).
    pub winning_number: u8,
    /// Indica se pelo menos uma cobertura confirmada venceu.
    pub won: bool,
    /// Resultado líquido esperado em centavos inteiros (`+stake` na vitória, `-2*stake` na perda).
    pub expected_delta_cents: i64,
    /// Novo degrau da escada após o spin (`0` se venceu ou se estourou `max_steps`).
    pub next_step: u8,
    /// Indica se o degrau `max_steps` perdeu neste spin (estouro de escada).
    pub ladder_blown: bool,
    /// `true` se o delta observado no extrato divergiu do esperado (> 1 centavo).
    pub session_suspicious: bool,
}

/// Executa a limpeza de `SingletonLock` órfão do perfil Chrome e o `orphan_sweep` no SQLite (§9).
pub fn boot_reconcile(
    pool: &SqlitePool,
    profile_dir: Option<&Path>,
) -> Result<BootReconcileReport, ExecError> {
    let mut singleton_lock_removed = false;
    if let Some(dir) = profile_dir {
        let lock_path = dir.join("SingletonLock");
        if lock_path.exists() && std::fs::remove_file(&lock_path).is_ok() {
            singleton_lock_removed = true;
            info!(path = %lock_path.display(), "SingletonLock órfão removido no boot");
        }
    }

    let orphans_swept = orphan_sweep(pool)?;
    Ok(BootReconcileReport {
        orphans_swept,
        singleton_lock_removed,
    })
}

const fn payout_multiplier(cover: Cover) -> i64 {
    match cover {
        Cover::Col(_) | Cover::Col2To1(_) | Cover::Dozen(_) => 2,
        Cover::Straight(_) => 35,
        Cover::Terminal(_) => 8,
    }
}

/// Casa os `fills` confirmados com o resultado do spin e com o extrato da casa (§9).
pub fn reconcile_spin(
    pool: &SqlitePool,
    strategy_id: &str,
    winning_number: u8,
    acks: &[CoverAck],
    cfg: &Cfg,
    observed_balance_delta_cents: Option<i64>,
    now_ms: i64,
) -> Result<ReconcileReport, ExecError> {
    let mut expected_delta_cents = 0i64;
    let mut won = false;

    for ack in acks.iter().filter(|a| a.acked) {
        let stake = ack.stake_cents as i64;
        if ack.cover.matches_number(winning_number) {
            won = true;
            expected_delta_cents += stake * payout_multiplier(ack.cover);
        } else {
            expected_delta_cents -= stake;
        }
    }

    let current = get_ladder_state(pool, strategy_id)?;
    let active_step = current.step.saturating_add(1);
    let mut ladder_blown = false;

    let (next_step, next_loss_cents, reset_at) = if won {
        (0u8, 0u64, Some(now_ms))
    } else {
        let spin_loss = expected_delta_cents.unsigned_abs();
        let new_loss = current.loss_cents.saturating_add(spin_loss);
        if active_step >= cfg.max_steps {
            ladder_blown = true;
            (0u8, 0u64, Some(now_ms))
        } else {
            (active_step, new_loss, current.reset_at)
        }
    };

    upsert_ladder_state(pool, strategy_id, next_step, next_loss_cents, reset_at)?;

    let mut session_suspicious = false;
    if let Some(observed) = observed_balance_delta_cents {
        if (observed - expected_delta_cents).abs() > 1 {
            session_suspicious = true;
            warn!(
                strategy_id,
                winning_number,
                expected_delta_cents,
                observed,
                "divergência de extrato detectada: marcando sessão como suspeita"
            );
            let _ = insert_error(
                pool,
                "extrato_divergence",
                &format!(
                    "strategy={strategy_id} number={winning_number} expected={expected_delta_cents} observed={observed}"
                ),
                now_ms,
            );
        }
    }

    Ok(ReconcileReport {
        strategy_id: strategy_id.to_string(),
        winning_number,
        won,
        expected_delta_cents,
        next_step,
        ladder_blown,
        session_suspicious,
    })
}
