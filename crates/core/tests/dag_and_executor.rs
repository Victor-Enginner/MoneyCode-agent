// SPDX-License-Identifier: Apache-2.0
//! Testes de integração das Fases 4 e 5 (`crates/core`):
//! - Replay de histórico SQLite com relógio virtual (§13)
//! - Gate Fase 4: latência p95 < 400 ms e Monte Carlo P(ruína 8h) < 5% (§15.4)
//! - Gate Fase 5: 200 entradas em `--dry-run` com zero órfãos e zero preview-mismatch (§15.5)

use monster_core::{
    boot_reconcile, evaluate_dag, place, reconcile_spin, Calibration, DryRunActuator, KillSwitch,
    PlayedEntry,
    Window30,
};
use monster_risk::{run_monte_carlo, Cfg, McParams, State, Verdict};
use monster_store::{init_pool, insert_spin, orphan_sweep, NewSpin, SpinSource};
use monster_strategy_native::{default_matchers, Cover};

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
        14u8, 22, 31, 5, 18, 27, 9, 16, 29, 2, 19, 33, 7, 15, 28, 11, 25, 1, 4, 7,
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
        // Garante que os últimos 4 giros estejam na Coluna 1 e Dúzia 1 (1, 4, 7, 10) para acionar QuebraColuna/QuebraDuzia
        let trigger_num = 10u8;
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
            PlayedEntry {
                strategy_id: &plan.strategy_id,
                ladder_step: plan.ladder_step,
                acks: &fills.items,
            },
            20, // Coluna 2 e Dúzia 2 (vencedora contra streak da Coluna 1 / Dúzia 1)
            &cfg,
            // Ganho líquido da cobertura dupla menos a ficha de proteção do zero (degrau >= 3).
            Some(plan.stake_per_cover_cents as i64 - zero_chip_cents(&plan)),
            virtual_clock_ms + 500,
        )
        .expect("reconcile_spin");
        assert!(rec.won);
        assert!(!rec.session_suspicious);

        // Reinicia a sequência de 3 giros na Coluna 1 / Dúzia 1 (1, 4, 7) para a próxima iteração do teste
        window.push(25);
        window.push(1);
        window.push(4);
        window.push(7);
    }

    assert_eq!(actuator.confirmed_count(), 200);
    assert_eq!(actuator.cleared_count(), 0);
    let orphans = orphan_sweep(&pool).expect("orphan_sweep");
    assert_eq!(orphans, 0);
}

/// Gate da Fase 5 com a escada de fato andando (§6/§9): 200 entradas em `--dry-run` numa sessão
/// de relógio virtual, alternando vitória e perda, conferindo a cada entrada que o degrau é o da
/// **própria** estratégia, que a ficha segue `stake_cents(degrau)` e que a mesa nunca fica suja.
#[test]
fn fase5_200_entradas_dry_run_com_escada_avancando() {
    use monster_core::backtest::cover_wins_real_roulette;
    use monster_core::{evaluate_dag_with_history, GateCtx, LadderSteps, Plan};
    use monster_risk::stake_cents;

    // Número real que vence ou perde a cobertura do plano — nada de "20 = coluna 2" chutado,
    // a conta sai da própria regra da roleta.
    // Sem o 0: a ficha de proteção do zero (degrau >= 3) faz o 0 "acertar" a cobertura,
    // mas o zero protegido repete o degrau em vez de contar vitória.
    let pick_outcome = |plan: &Plan, want_win: bool| -> u8 {
        (1u8..=36)
            .find(|&n| {
                plan.covers
                    .iter()
                    .any(|(cover, _)| cover_wins_real_roulette(*cover, n))
                    == want_win
            })
            .expect("sempre existe numero que vence e numero que perde a cobertura")
    };

    let pool = init_pool(":memory:").expect("init in-memory sqlite");
    let _ = boot_reconcile(&pool, None).expect("boot_reconcile");

    let cfg = Cfg::default();
    // Só o Fibonacci lag: este gate prova a escada ×3 e o executor, não o agregador.
    // (Ele dispara com a sequência 1-4-7-10 semeada e cobre duas colunas; com a lista completa
    // o `cluster_final` ganharia a disputa e o plano viraria de números, que é o outro teste.)
    let matchers: Vec<std::sync::Arc<dyn monster_strategy_native::Matcher>> =
        vec![std::sync::Arc::new(monster_strategy_native::FibonacciLag)];
    let actuator = DryRunActuator::new();
    let cal = Calibration::default();
    let kill_switch = KillSwitch::new();
    let mut window = Window30::new();
    let mut ladder_steps = LadderSteps::new();

    let mut virtual_clock_ms = 1_730_600_000_000i64;
    let mut seed: u32 = 0;
    let mut push = |n: u8, window: &mut Window30, pool: &monster_store::SqlitePool, clock: &mut i64| {
        *clock += 45_000;
        insert_spin(
            pool,
            &NewSpin {
                spin_id: format!("fase5-{seed}-{n}"),
                number: n,
                ts: *clock,
                source: SpinSource::Ws,
                received_at: *clock + 25,
                latency_ms: Some(25),
            },
        )
        .expect("insert spin");
        window.push(n);
        seed += 1;
    };

    // Semeia Coluna 1 (1, 4, 7, 10, 1) para armar o lag de Fibonacci (cobre Col 2 + Col 3).
    // Cinco casas, e não quatro: o número do resultado empurrado no fim de cada rodada
    // entra em `w[4]` e precisa continuar sendo Coluna 1.
    for n in [1u8, 4, 7, 10, 1] {
        push(n, &mut window, &pool, &mut virtual_clock_ms);
    }

    let mut placed = 0u32;
    let mut blows = 0u32;
    let mut rungs = std::collections::BTreeSet::new();
    let max_steps = cfg.max_steps;

    for entry_idx in 1..=200u32 {
        // Perde 5 seguidas (degraus 1..5, estourando no último) e ganha na 6ª.
        let will_win = entry_idx % 6 == 0;
        // Rearma a Coluna 1 antes de cada entrada: é o que mantém o matcher disparando.
        for n in [1u8, 4, 7, 10, 1] {
            push(n, &mut window, &pool, &mut virtual_clock_ms);
        }

        let state = State {
            bankroll_cents: 500_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 10_000,
            spins_since_blow: 5,
            step: 0, // neutro: o degrau vem do mapa por estratégia
        };
        // Histórico curto de propósito: com >= 25 giros o walk-forward entraria e bloquearia
        // entradas por taxa de acerto, o que não é o gate desta prova (é o da Fase 4).
        let mut chrono: Vec<u8> = window.to_recent_vec();
        chrono.reverse();
        if chrono.len() > 10 {
            chrono = chrono[chrono.len() - 10..].to_vec();
        }
        let (dag_out, _) = evaluate_dag_with_history(
            &format!("fase5-entry-{entry_idx}"),
            &window.to_recent_vec(),
            &chrono,
            &matchers,
            &cfg,
            &GateCtx {
                state: &state,
                ladder_steps: &ladder_steps,
            },
            0.50,
        );
        assert_eq!(
            dag_out.verdict,
            Some(Verdict::Place),
            "entrada {entry_idx} deveria apostar"
        );
        let plan = dag_out.plan.expect("plano no veredito Place");

        // O degrau pertence à estratégia vencedora, nunca à última que perdeu.
        let expected_step = ladder_steps.get(&plan.strategy_id).copied().unwrap_or(0) + 1;
        assert_eq!(
            plan.ladder_step, expected_step,
            "entrada {entry_idx} ({}) saiu no degrau {} com escada em {}",
            plan.strategy_id, plan.ladder_step, expected_step - 1
        );
        assert_eq!(
            plan.stake_per_cover_cents,
            stake_cents(plan.ladder_step, cfg.base_cents),
            "ficha por cobertura deve seguir a escada x3"
        );
        rungs.insert(plan.ladder_step);

        let fills = place(&pool, &actuator, &plan, &cal, &kill_switch, virtual_clock_ms)
            .expect("place em dry-run deve confirmar");
        assert_eq!(fills.preview_cents, plan.total_cents, "preview != plano");
        assert!(!fills.naked, "fill naked nao pode existir em dry-run");
        placed += 1;

        let outcome = pick_outcome(&plan, will_win);
        let rec = reconcile_spin(
            &pool,
            PlayedEntry {
                strategy_id: &plan.strategy_id,
                ladder_step: plan.ladder_step,
                acks: &fills.items,
            },
            outcome,
            &cfg,
            Some(plan.stake_per_cover_cents as i64),
            virtual_clock_ms + 500,
        )
        .expect("reconcile_spin");
        assert_eq!(rec.won, will_win, "entrada {entry_idx} resultado divergente");
        if rec.ladder_blown {
            blows += 1;
            assert_eq!(plan.ladder_step, max_steps, "so estoura no ultimo degrau");
            assert_eq!(rec.next_step, 0, "estouro reinicia a escada");
        } else if will_win {
            assert_eq!(rec.next_step, 0, "vitoria zera o degrau");
        } else {
            assert_eq!(
                rec.next_step,
                plan.ladder_step,
                "perda avanca exatamente um degrau"
            );
        }
        ladder_steps.insert(rec.strategy_id.clone(), rec.next_step);

        // Fecha a rodada com o número que saiu de verdade.
        push(outcome, &mut window, &pool, &mut virtual_clock_ms);
    }

    assert_eq!(placed, 200, "gate Fase 5 exige 200 entradas");
    assert!(blows > 0, "a sessao precisa ter exercitado estouro de escada");
    assert_eq!(
        rungs.iter().copied().collect::<Vec<_>>(),
        (1..=max_steps).collect::<Vec<_>>(),
        "todos os degraus da escada precisam ser exercitados"
    );
    assert_eq!(
        actuator.cleared_count(),
        0,
        "gate Fase 5: zero preview-mismatch (nenhum clear_bets)"
    );
    let orphans = orphan_sweep(&pool).expect("orphan_sweep");
    assert_eq!(orphans, 0, "gate Fase 5: zero orfaos");
}

/// Fase 5 no caminho de NÚMEROS: o `cluster_final` cobre terminais, o `slip.compiler`
/// expande em plenos e a escada é `(D+G)/(36−k)` — nunca a ×3 de duas colunas.
#[test]
fn fase5_cluster_final_aposta_em_numeros_com_escada_propria() {
    use monster_risk::NumberLadder;
    use monster_strategy_native::{ClusterFinal, Matcher};

    let pool = init_pool(":memory:").expect("init in-memory sqlite");
    let cfg = Cfg::default();
    let matchers: Vec<std::sync::Arc<dyn Matcher>> = vec![std::sync::Arc::new(ClusterFinal::default())];
    let actuator = DryRunActuator::new();
    let cal = Calibration::default();
    let kill_switch = KillSwitch::new();
    let mut window = Window30::new();
    let mut ladder_steps = monster_core::LadderSteps::new();
    let mut clock = 1_730_700_000_000i64;
    let mut seed = 0u32;

    let mut push = |n: u8, window: &mut Window30, clock: &mut i64| {
        *clock += 45_000;
        insert_spin(
            &pool,
            &NewSpin {
                spin_id: format!("fase5-num-{seed}-{n}"),
                number: n,
                ts: *clock,
                source: SpinSource::Ws,
                received_at: *clock + 20,
                latency_ms: Some(20),
            },
        )
        .expect("insert spin");
        window.push(n);
        seed += 1;
    };

    // Família 1-4-7 persistente (10 giros da mesma família).
    let family: [u8; 10] = [1, 4, 7, 11, 14, 17, 21, 24, 27, 31];
    for n in family {
        push(n, &mut window, &mut clock);
    }

    let mut losses_cents = 0u64;
    let mut steps_seen = std::collections::BTreeSet::new();
    for entry_idx in 1..=3u32 {
        let will_win = entry_idx == 3;
        for n in family {
            push(n, &mut window, &mut clock);
        }

        let state = State {
            bankroll_cents: 500_000,
            exposure_cents: 0,
            session_pnl_cents: -(losses_cents as i64),
            remaining_window_ms: 10_000,
            spins_since_blow: 5,
            step: 0,
        };
        let mut chrono = window.to_recent_vec();
        chrono.reverse();
        chrono = chrono[chrono.len().saturating_sub(10)..].to_vec();
        let (dag_out, _) = monster_core::evaluate_dag_with_history(
            &format!("fase5-num-entry-{entry_idx}"),
            &window.to_recent_vec(),
            &chrono,
            &matchers,
            &cfg,
            &monster_core::GateCtx {
                state: &state,
                ladder_steps: &ladder_steps,
            },
            0.50,
        );
        assert_eq!(dag_out.verdict, Some(Verdict::Place), "entrada {entry_idx}");
        let plan = dag_out.plan.expect("plano de números");

        // O plano é de PLENOS: 15 números (0, 1, 4, 7 com 4+4+4+3 pockets).
        assert_eq!(plan.covers.len(), 15, "cluster 1-4-7 + terminal 0 = 15 números");
        assert!(
            plan.covers.iter().all(|(c, _)| matches!(c, Cover::Straight(_))),
            "terminal precisa virar pleno antes de chegar ao executor"
        );
        let nums: Vec<u8> = plan.covers.iter().map(|(c, _)| c.numbers()[0]).collect();
        assert!(nums.contains(&0) && nums.contains(&1) && nums.contains(&4) && nums.contains(&7));

        // Ficha por número segue a escada de números, não a ×3.
        let nl = NumberLadder::new(15, cfg.base_cents);
        let expected_stake = nl.stake_cents(plan.ladder_step);
        assert_eq!(plan.stake_per_cover_cents, expected_stake);
        assert_eq!(plan.total_cents, 15 * expected_stake);
        if plan.ladder_step > 1 {
            assert!(
                expected_stake < monster_risk::stake_cents(plan.ladder_step, cfg.base_cents),
                "escada de números não pode crescer ×3 (degrau {})",
                plan.ladder_step
            );
        }
        steps_seen.insert(plan.ladder_step);

        let fills = place(&pool, &actuator, &plan, &cal, &kill_switch, clock)
            .expect("place dry-run de números");
        assert_eq!(fills.preview_cents, plan.total_cents, "preview deve somar os plenos");
        assert!(!fills.naked);

        // Acerta um número coberto na 3ª entrada; nas outras sai um número fora da cobertura (2).
        let outcome = if will_win { 11 } else { 2 };
        let rec = reconcile_spin(
            &pool,
            PlayedEntry {
                strategy_id: &plan.strategy_id,
                ladder_step: plan.ladder_step,
                acks: &fills.items,
            },
            outcome,
            &cfg,
            None,
            clock + 500,
        )
        .expect("reconcile_spin numeros");
        assert_eq!(rec.won, will_win, "entrada {entry_idx}: resultado divergente");

        if will_win {
            assert_eq!(rec.next_step, 0, "acerto zera o degrau");
            // 15 plenos com x cada: acerto paga 35x e os outros 14 custam 14x -> +21x.
            assert_eq!(rec.expected_delta_cents, 21 * expected_stake as i64);
            losses_cents = 0;
        } else {
            assert_eq!(rec.next_step, plan.ladder_step, "perda avança um degrau");
            assert_eq!(rec.expected_delta_cents, -(15 * expected_stake as i64));
            losses_cents = nl.cum_loss_cents(plan.ladder_step);
        }
        ladder_steps.insert(rec.strategy_id.clone(), rec.next_step);
    }

    assert_eq!(
        steps_seen.iter().copied().collect::<Vec<_>>(),
        vec![1, 2, 3],
        "escada de números precisa subir degrau por degrau"
    );
    assert_eq!(actuator.cleared_count(), 0, "zero preview-mismatch em números");
    assert_eq!(orphan_sweep(&pool).expect("orphan_sweep"), 0, "zero órfãos");
}

#[test]
fn dag_expoe_posterior_de_regime_sem_vetar_bloco_real() {
    use monster_core::{evaluate_dag_with_history, GateCtx, LadderSteps};
    use monster_strategy_native::{ClusterFinal, Matcher};

    let cfg = Cfg::default();
    let matchers: Vec<std::sync::Arc<dyn Matcher>> =
        vec![std::sync::Arc::new(ClusterFinal::default())];
    let ladder_steps = LadderSteps::new();
    let state = State {
        bankroll_cents: 500_000,
        exposure_cents: 0,
        session_pnl_cents: 0,
        remaining_window_ms: 10_000,
        spins_since_blow: 5,
        step: 0,
    };
    // Bloco real de 1-4-7: 30 giros da família e a janela atual no mesmo bloco.
    let window: Vec<u8> = vec![1, 4, 7, 11, 14];
    let mut chrono: Vec<u8> = [1u8, 4, 7, 11, 14, 17]
        .iter()
        .copied()
        .cycle()
        .take(30)
        .collect();
    chrono.extend(window.iter().rev().copied());

    let (dag_out, _) = evaluate_dag_with_history(
        "regime-bloco",
        &window,
        &chrono,
        &matchers,
        &cfg,
        &GateCtx {
            state: &state,
            ladder_steps: &ladder_steps,
        },
        0.50,
    );

    // O DAG publica a posterior do BOCPD junto da decisão.
    assert_eq!(dag_out.bocpd.map_family, Some(1), "regime na família 1-4-7");
    assert!(dag_out.bocpd.observations >= 30);
    assert!(dag_out.bocpd.p_change < 0.2, "bloco estável não está trocando");

    // E não veta o sinal legítimo.
    let cluster = dag_out
        .aggregates
        .iter()
        .find(|a| a.strategy_id == "cluster_final")
        .expect("matcher dispara com bloco estável");
    assert!(!cluster.regime_veto);
    assert!(cluster.confidence > 0.0);
    assert_eq!(dag_out.verdict, Some(Verdict::Place));
    assert_eq!(dag_out.plan.expect("plano de plenos").covers.len(), 15);
}

/// Ficha de proteção no 0 incluída no plano (0 quando não há).
fn zero_chip_cents(plan: &monster_core::Plan) -> i64 {
    plan.covers
        .iter()
        .filter(|(c, _)| *c == Cover::Straight(0))
        .map(|(_, cents)| *cents as i64)
        .sum()
}
