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
        Cover::Red | Cover::Black | Cover::Low | Cover::High | Cover::Even | Cover::Odd => 1,
        Cover::Straight(_) => 35,
        Cover::Terminal(_) => 8,
    }
}

/// Entrada que efetivamente rodou no spin anterior — base do avanço da escada (§9).
#[derive(Debug, Clone, Copy)]
pub struct PlayedEntry<'a> {
    /// Estratégia que entrou.
    pub strategy_id: &'a str,
    /// Degrau que a entrada REALMENTE usou (`Plan::ladder_step`).
    pub ladder_step: u8,
    /// Acks por cobertura coletados após a confirmação.
    pub acks: &'a [CoverAck],
}

/// Casa os `fills` confirmados com o resultado do spin e com o extrato da casa (§9).
/// O avanço da escada parte do degrau realmente jogado (`entry.ladder_step`), não de um
/// `ladder_state` defasado: é isso que impede a escada de pular degrau.
pub fn reconcile_spin(
    pool: &SqlitePool,
    entry: PlayedEntry<'_>,
    winning_number: u8,
    cfg: &Cfg,
    observed_balance_delta_cents: Option<i64>,
    now_ms: i64,
) -> Result<ReconcileReport, ExecError> {
    let strategy_id = entry.strategy_id;
    let acks = entry.acks;
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
    let played_step = entry.ladder_step.max(1);
    let mut ladder_blown = false;

    // Zero protegido: entrada por fora com a ficha de proteção no 0, e saiu o 0. A aposta voltou
    // (≈ empate), então a escada REPETE o degrau em vez de zerar: a perda dos degraus anteriores
    // continua para ser recuperada.
    let outside_entry = acks
        .iter()
        .any(|a| !matches!(a.cover, Cover::Straight(_) | Cover::Terminal(_)));
    let zero_protected = winning_number == 0
        && outside_entry
        && acks.iter().any(|a| a.acked && a.cover == Cover::Straight(0));
    let (next_step, next_loss_cents, reset_at) = if zero_protected {
        let leftover = u64::try_from((-expected_delta_cents).max(0)).unwrap_or(0);
        (
            played_step.saturating_sub(1),
            current.loss_cents.saturating_add(leftover),
            current.reset_at,
        )
    } else if won {
        (0u8, 0u64, Some(now_ms))
    } else {
        let spin_loss = expected_delta_cents.unsigned_abs();
        let new_loss = current.loss_cents.saturating_add(spin_loss);
        if played_step >= cfg.max_steps {
            ladder_blown = true;
            warn!(
                strategy_id,
                played_step,
                max_steps = cfg.max_steps,
                "estouro de escada no último degrau: escada reiniciada"
            );
            (0u8, 0u64, Some(now_ms))
        } else {
            // Perdeu no degrau k -> o próximo é k+1, ou seja, `ladder_state.step = k`.
            (played_step, new_loss, current.reset_at)
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

#[cfg(test)]
mod tests {
    use super::*;
    use monster_store::{get_ladder_state, init_pool, upsert_ladder_state};

    fn entrada<'a>(id: &'a str, step: u8, acks: &'a [CoverAck]) -> PlayedEntry<'a> {
        PlayedEntry {
            strategy_id: id,
            ladder_step: step,
            acks,
        }
    }

    fn loss_acks(stake: u64) -> Vec<CoverAck> {
        vec![
            CoverAck {
                cover: Cover::Col(1),
                stake_cents: stake,
                acked: true,
            },
            CoverAck {
                cover: Cover::Col(3),
                stake_cents: stake,
                acked: true,
            },
        ]
    }

    #[test]
    fn escada_avanca_do_degrau_jogado_e_nunca_pula() {
        let pool = init_pool(":memory:").expect("sqlite em memória");
        let cfg = Cfg::default();
        let id = "quebra_permanencia_coluna";

        // Degrau 1 perdido (-20¢) -> a próxima entrada é o degrau 2, então `step` persistido = 1.
        let acks1 = loss_acks(10);
        let r1 = reconcile_spin(&pool, entrada(id, 1, &acks1), 17, &cfg, None, 1_000)
            .expect("reconcile degrau 1");
        assert!(!r1.won);
        assert_eq!(r1.expected_delta_cents, -20);
        assert_eq!(r1.next_step, 1);
        assert!(!r1.ladder_blown);

        // Degrau 2 perdido (-60¢) -> próxima entrada é o degrau 3 (`step` = 2).
        let acks2 = loss_acks(30);
        let r2 = reconcile_spin(&pool, entrada(id, 2, &acks2), 17, &cfg, None, 2_000)
            .expect("reconcile degrau 2");
        assert_eq!(r2.expected_delta_cents, -60);
        assert_eq!(r2.next_step, 2);

        let row = get_ladder_state(&pool, id).expect("ladder_state");
        assert_eq!(row.step, 2);
        assert_eq!(row.loss_cents, 80, "perda acumulada §6: 20¢ + 60¢");
    }

    #[test]
    fn ladder_state_defasado_nao_faz_a_escada_pular_degrau() {
        let pool = init_pool(":memory:").expect("sqlite em memória");
        let cfg = Cfg::default();
        let id = "quebra_permanencia_duzia";
        // Estado persistido diz degrau 2, mas a entrada que rodou foi o degrau 1.
        upsert_ladder_state(&pool, id, 2, 40, None).expect("seed ladder_state");
        let acks = loss_acks(10);
        let r = reconcile_spin(&pool, entrada(id, 1, &acks), 17, &cfg, None, 1_000)
            .expect("reconcile degrau 1");
        assert_eq!(
            r.next_step, 1,
            "o avanço parte do degrau jogado; o banco não pode empurrar para o degrau 3"
        );
    }

    #[test]
    fn ultimo_degrau_perdido_reinicia_a_escada() {
        let pool = init_pool(":memory:").expect("sqlite em memória");
        let cfg = Cfg::default();
        let id = "quebra_alto_baixo_6x";
        let acks = vec![CoverAck {
            cover: Cover::High,
            stake_cents: 810,
            acked: true,
        }];
        // 17 não é alto: o último degrau perdeu e a escada estoura (§6).
        let r = reconcile_spin(&pool, entrada(id, cfg.max_steps, &acks), 17, &cfg, None, 1_000)
            .expect("reconcile degrau final");
        assert!(r.ladder_blown);
        assert_eq!(r.next_step, 0);
    }

    #[test]
    fn zero_protegido_repete_o_degrau_em_vez_de_zerar() {
        let pool = init_pool(":memory:").expect("pool");
        let cfg = Cfg::default();
        // Degrau 4 de coluna (5,40 por coluna) + 0,30 no zero; sai o 0.
        let acks = [
            CoverAck { cover: Cover::Col(1), stake_cents: 540, acked: true },
            CoverAck { cover: Cover::Col(3), stake_cents: 540, acked: true },
            CoverAck { cover: Cover::Straight(0), stake_cents: 30, acked: true },
        ];
        let r = reconcile_spin(&pool, entrada("quebra_permanencia_coluna", 4, &acks), 0, &cfg, None, 1)
            .expect("reconcile");
        assert_eq!(r.next_step, 3, "joga o degrau 4 de novo");
        assert_eq!(r.expected_delta_cents, 30 * 35 - 1080);
        assert!(!r.ladder_blown);
        // Sem o zero (degrau 1/2), o 0 continua sendo perda normal.
        let acks2 = [
            CoverAck { cover: Cover::Col(1), stake_cents: 20, acked: true },
            CoverAck { cover: Cover::Col(3), stake_cents: 20, acked: true },
        ];
        let r2 = reconcile_spin(&pool, entrada("quebra_permanencia_duzia", 1, &acks2), 0, &cfg, None, 1)
            .expect("reconcile");
        assert!(!r2.won);
        assert_eq!(r2.next_step, 1);
    }

    #[test]
    fn vitoria_zera_a_escada_e_o_degrau() {
        let pool = init_pool(":memory:").expect("sqlite em memória");
        let cfg = Cfg::default();
        let id = "quebra_permanencia_coluna";
        upsert_ladder_state(&pool, id, 3, 260, None).expect("seed ladder_state");
        // 13 é coluna 1: acerta a cobertura da coluna 1 e perde a da coluna 3.
        let acks = loss_acks(90);
        let r = reconcile_spin(&pool, entrada(id, 4, &acks), 13, &cfg, None, 1_000)
            .expect("reconcile vencedor");
        assert!(r.won);
        assert_eq!(r.expected_delta_cents, 90, "ganho líquido = uma ficha (§6)");
        assert_eq!(r.next_step, 0);
        assert_eq!(get_ladder_state(&pool, id).expect("ladder_state").loss_cents, 0);
    }
}
