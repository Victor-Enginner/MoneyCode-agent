// SPDX-License-Identifier: Apache-2.0
//! Núcleo de orquestração do projeto MONSTER: event bus, DAG, classificador de regime e executor.
//! Opera em modo `--dry-run` por padrão e coordena reconciliação pós-spin e kill switch.
//! Orçamento estrito de latência (<250 ms no DAG) sem chamadas de LLM no caminho crítico.

pub mod dag;
pub mod executor;
pub mod reconciler;

pub use dag::{
    classify, compute_ev_cents_two_covers, evaluate_dag, fan_in, Aggregate, DagOutput, Plan,
    Regime, Window30, WINDOW_CAPACITY,
};
pub use executor::{
    place, Calibration, CdpActuator, CoverAck, DryRunActuator, ExecError, ExecutionMode, Fills,
    KillSwitch, LiveHttpActuator,
};
pub use reconciler::{boot_reconcile, reconcile_spin, BootReconcileReport, ReconcileReport};

/// Versão do módulo core.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use monster_risk::{Cfg, State, Verdict};
    use monster_store::{init_pool, orphan_sweep};
    use monster_strategy_native::{default_matchers, Cover};

    #[test]
    fn window30_maintains_most_recent_first_up_to_30() {
        let mut w = Window30::new();
        for n in 1..=35u8 {
            w.push(n);
        }
        assert_eq!(w.len(), 30);
        let recent = w.to_recent_vec();
        assert_eq!(recent.len(), 30);
        assert_eq!(recent[0], 35);
        assert_eq!(recent[29], 6);
    }

    #[test]
    fn regime_classifier_detects_permanence_momentum_and_neutral() {
        // Permanência: quase todos repetidos -> poucos runs (< 0.75 * 10 = 7.5)
        let perm = [5u8; 20];
        assert_eq!(classify(&perm), Regime::Permanence);

        // Momentum: alternância constante -> 20 runs (> 1.25 * 10 = 12.5)
        let mut mom = [0u8; 20];
        for (i, slot) in mom.iter_mut().enumerate() {
            *slot = if i % 2 == 0 { 1 } else { 20 };
        }
        assert_eq!(classify(&mom), Regime::Momentum);

        // Janela < 20 -> Neutral
        assert_eq!(classify(&[1, 2, 3, 4]), Regime::Neutral);
    }

    #[test]
    fn dag_p95_latency_is_well_below_400ms_gate() {
        let matchers = default_matchers();
        let cfg = Cfg::default();
        let state = State {
            bankroll_cents: 45_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 8000,
            spins_since_blow: 10,
            step: 0,
        };
        let window = [2u8, 5, 8, 11, 14, 19, 25, 30, 1, 4, 7, 10, 15, 22, 31, 33, 0, 12, 18, 27];
        let mut latencies_us = Vec::with_capacity(500);

        for idx in 0..500u32 {
            let out = evaluate_dag(
                &format!("spin-{idx}"),
                &window,
                &matchers,
                &cfg,
                &state,
                0.50,
            );
            assert_eq!(out.verdict, Some(Verdict::Place));
            assert!(out.plan.is_some());
            latencies_us.push(out.elapsed_us);
        }

        latencies_us.sort_unstable();
        let p95_us = latencies_us[(latencies_us.len() * 95) / 100];
        // Gate §15.4: p95 < 400 ms (400_000 µs); nativo em Rust leva poucos microssegundos.
        assert!(
            p95_us < 400_000,
            "latência p95 do DAG ({p95_us} µs) excedeu o gate de 400 ms"
        );
    }

    #[test]
    fn phase5_gate_dry_run_200_entries_zero_orphans_zero_preview_mismatch() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let actuator = DryRunActuator::new();
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();
        let cfg = Cfg::default();
        let matchers = default_matchers();
        let window = [3u8, 6, 9, 12, 20, 25, 30, 14, 18, 22, 1, 5, 8, 11, 17, 29, 31, 34, 2, 7];

        for i in 1..=200u32 {
            let step = ((i - 1) % 5) as u8;
            let state = State {
                bankroll_cents: 45_000,
                exposure_cents: 0,
                session_pnl_cents: 0,
                remaining_window_ms: 9000,
                spins_since_blow: 5,
                step,
            };
            let spin_id = format!("dryrun-spin-{i}");
            let dag_out = evaluate_dag(&spin_id, &window, &matchers, &cfg, &state, 0.50);
            let plan = dag_out.plan.expect("DAG must produce a valid Plan");

            let now_ms = 1_730_000_000_000i64 + i64::from(i) * 1000;
            let fills = place(&pool, &actuator, &plan, &cal, &kill_switch, now_ms)
                .expect("dry-run place must succeed with zero preview mismatch");

            assert_eq!(fills.preview_cents, plan.total_cents);
            assert!(!fills.naked);
            assert_eq!(fills.items.len(), 2);

            let rec = reconcile_spin(
                &pool,
                &plan.strategy_id,
                19, // Coluna 2 vence (Col(2) e Col(3) cobertas)
                &fills.items,
                &cfg,
                Some(plan.stake_per_cover_cents as i64),
                now_ms + 500,
            )
            .expect("reconcile spin");
            assert!(rec.won);
            assert!(!rec.session_suspicious);
        }

        assert_eq!(actuator.confirmed_count(), 200);
        assert_eq!(actuator.cleared_count(), 0);

        let orphans = orphan_sweep(&pool).expect("run orphan_sweep after 200 entries");
        assert_eq!(orphans, 0, "Gate Fase 5 exige zero órfãos após 200 entradas");
    }

    #[test]
    fn preview_mismatch_clears_bets_and_aborts_before_confirm() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let actuator = DryRunActuator::new();
        actuator.set_forced_preview_delta(10); // +10 centavos de divergência (> 1 centavo)
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();

        let plan = Plan {
            spin_id: "spin-mismatch-1".to_string(),
            strategy_id: "quebra_permanencia_coluna".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 10,
            total_cents: 20,
            ev_cents: -1,
            confidence: 0.75,
            covers: vec![(Cover::Col(2), 10), (Cover::Col(3), 10)],
            chips_per_cover: vec![(10, 1)],
        };

        let err = place(&pool, &actuator, &plan, &cal, &kill_switch, 1_730_000_010_000)
            .expect_err("preview mismatch must abort");
        assert!(matches!(
            err,
            ExecError::PreviewMismatch {
                preview: 30,
                expected: 20
            }
        ));
        assert_eq!(actuator.confirmed_count(), 0, "nunca deve chamar confirm() se preview divergir");
        assert_eq!(actuator.cleared_count(), 1, "deve chamar clear_bets() na divergência");

        // E não deve deixar órfão porque a intent foi marcada como aborted
        let orphans = orphan_sweep(&pool).expect("orphan_sweep");
        assert_eq!(orphans, 0);
    }

    #[test]
    fn idempotency_blocks_duplicate_strategy_and_spin_id() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let actuator = DryRunActuator::new();
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();

        let plan = Plan {
            spin_id: "spin-dup-42".to_string(),
            strategy_id: "quebra_permanencia_coluna".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 10,
            total_cents: 20,
            ev_cents: -1,
            confidence: 0.75,
            covers: vec![(Cover::Col(2), 10), (Cover::Col(3), 10)],
            chips_per_cover: vec![(10, 1)],
        };

        place(&pool, &actuator, &plan, &cal, &kill_switch, 1_730_000_020_000)
            .expect("first entry succeeds");
        let dup_err = place(&pool, &actuator, &plan, &cal, &kill_switch, 1_730_000_020_100)
            .expect_err("second entry for same (strategy_id, spin_id) must be blocked");
        assert!(matches!(dup_err, ExecError::AlreadyEntered { .. }));
        assert_eq!(actuator.confirmed_count(), 1);
    }
}
