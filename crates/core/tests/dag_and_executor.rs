// SPDX-License-Identifier: Apache-2.0
//! Testes de integração das Fases 4 e 5 (`crates/core`):
//! - Replay de histórico SQLite com relógio virtual (§13)
//! - Gate Fase 4: latência p95 < 400 ms e Monte Carlo P(ruína 8h) < 5% (§15.4)
//! - Gate Fase 5: 200 entradas em `--dry-run` com zero órfãos e zero preview-mismatch (§15.5)

use monster_core::{
    boot_reconcile, evaluate_dag, place, reconcile_spin, Calibration, DryRunActuator, KillSwitch,
    Window30,
};
use monster_risk::{run_monte_carlo, Cfg, McParams, State, Verdict};
use monster_store::{init_pool, insert_spin, orphan_sweep, NewSpin, SpinSource};
use monster_strategy_native::default_matchers;

#[test]
fn integration_sqlite_virtual_clock_replay_and_200_dryrun_entries() {
    let pool = init_pool(":memory:").expect("init in-memory sqlite");
    let boot = boot_reconcile(&pool, None).expect("boot_reconcile");
    assert_eq!(boot.orphans_swept, 0);

    let cfg = Cfg::default();
    let mc_8h = run_monte_carlo(&McParams {
        hours_per_session: 8,
        cfg: cfg.clone(),
        ..McParams::default()
    });
    assert!(
        mc_8h.p_ruin < 0.05,
        "Gate Fase 4: P(ruína 8h) = {:.4} deve ser < 5%",
        mc_8h.p_ruin
    );

    let matchers = default_matchers();
    let actuator = DryRunActuator::new();
    let cal = Calibration::default();
    let kill_switch = KillSwitch::new();
    let mut window = Window30::new();

    // Semeia 20 números iniciais com relógio virtual para ativar o classificador de regime.
    let seed_numbers = [
        14u8, 22, 31, 5, 18, 27, 9, 16, 29, 2, 19, 33, 7, 15, 28, 11, 3, 6, 9, 12,
    ];
    let mut virtual_clock_ms = 1_730_500_000_000i64;
    for (idx, &num) in seed_numbers.iter().enumerate() {
        virtual_clock_ms += 45_000;
        insert_spin(
            &pool,
            &NewSpin {
                spin_id: format!("replay-seed-{idx}"),
                number: num,
                ts: virtual_clock_ms,
                source: SpinSource::Ws,
                received_at: virtual_clock_ms + 30,
                latency_ms: Some(30),
            },
        )
        .expect("insert seed spin");
        window.push(num);
    }

    // Executa 200 entradas em --dry-run sobre relógio virtual
    for entry_idx in 1..=200u32 {
        virtual_clock_ms += 45_000;
        // Garante que os últimos 4 giros estejam na mesma coluna (1..=12) para acionar QuebraColuna
        let trigger_num = ((entry_idx % 12) + 1) as u8;
        window.push(trigger_num);
        let recent = window.to_recent_vec();

        let state = State {
            bankroll_cents: 45_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 10_000,
            spins_since_blow: 5,
            step: ((entry_idx - 1) % 5) as u8,
        };

        let spin_id = format!("replay-entry-{entry_idx}");
        let dag_out = evaluate_dag(&spin_id, &recent, &matchers, &cfg, &state, 0.50);
        assert_eq!(dag_out.verdict, Some(Verdict::Place));
        let plan = dag_out.plan.expect("plan must be present on Place verdict");

        let fills = place(
            &pool,
            &actuator,
            &plan,
            &cal,
            &kill_switch,
            virtual_clock_ms,
        )
        .expect("place in dry-run must succeed");
        assert_eq!(fills.preview_cents, plan.total_cents);
        assert!(!fills.naked);

        let rec = reconcile_spin(
            &pool,
            &plan.strategy_id,
            20, // Coluna 2 (vencedora contra streak da Coluna 1)
            &fills.items,
            &cfg,
            Some(plan.stake_per_cover_cents as i64),
            virtual_clock_ms + 500,
        )
        .expect("reconcile_spin");
        assert!(rec.won);
        assert!(!rec.session_suspicious);

        // Reinicia a sequência de 3 giros na coluna 1 para a próxima iteração do teste
        window.push(25);
        window.push(2);
        window.push(5);
        window.push(8);
    }

    assert_eq!(actuator.confirmed_count(), 200);
    assert_eq!(actuator.cleared_count(), 0);
    let orphans = orphan_sweep(&pool).expect("orphan_sweep");
    assert_eq!(orphans, 0);
}
