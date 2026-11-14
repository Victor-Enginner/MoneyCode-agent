// SPDX-License-Identifier: Apache-2.0
//! Núcleo de orquestração do projeto MONSTER: event bus, DAG, classificador de regime e executor.
//! Opera em modo `--dry-run` por padrão e coordena reconciliação pós-spin e kill switch.
//! Orçamento estrito de latência (<250 ms no DAG) sem chamadas de LLM no caminho crítico.

pub mod analytics;
pub mod backtest;
pub mod balance_audit;
pub mod bocpd;
pub mod brain;
pub mod caution;
pub mod dag;
pub mod executor;
pub mod lab;
pub mod reconciler;
pub mod shadow;
pub mod table_stats;
pub mod terminals;
pub mod timeline_sync;

pub use analytics::{
    analyze_table_systemic, compute_chi_square_37, compute_chi_square_cols,
    compute_shannon_entropy_norm, compute_wald_wolfowitz_z, predict_ncm_break_probability,
    SystemicAnalysisReport,
};
pub use backtest::{
    evaluate_all_matchers_history, evaluate_matcher_walk_forward, HistoryBuffer,
    PatternHistoryStats, HISTORY_CAPACITY, REAL_TABLE_479_RECENT_FIRST,
};
pub use brain::{
    build_brain_snapshot, render_gbrain_markdown, write_gbrain_file, BrainSnapshot, HarnessTier,
    StrategyEvidence,
};
pub use bocpd::{
    analyze_chronological, family_of_number, BocpdCfg, BocpdReport, VetoReason,
};
pub use dag::{
    cfg_for_strategy, classify, compute_ev_cents_two_covers, evaluate_dag, evaluate_dag_with_history, fan_in,
    Aggregate, DagOutput, GateCtx, LadderSteps, Plan, Regime, Window30, WINDOW_CAPACITY,
};
pub use executor::{
    place, Calibration, CdpActuator, CoverAck, DryRunActuator, ExecError, ExecutionMode, Fills,
    KillSwitch, LiveHttpActuator, LiveTableTelemetry,
};
pub use reconciler::{
    boot_reconcile, reconcile_spin, BootReconcileReport, PlayedEntry, ReconcileReport,
};
pub use terminals::{family_radars, real_tail_start, terminal_radar, OPERATOR_TERMINALS};
pub use timeline_sync::{
    count_new_spins, ensure_session, has_recent_number, load_chronological_history,
    load_session_chronological, read_newest_numbers, read_session_newest_numbers, SessionDecision,
    read_session_newest_until, sanitize_strip, session_prev_spin_ts, spins_after_trigger, sync_strip_to_db, StripSyncReport, MAX_STRIP,
    MISSED_SPIN_GAP_MS,
};

/// Versão do módulo core.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Janela de aposta (§2) que o gate enxerga para o giro que acabou de chegar.
pub const BET_WINDOW_MS: u32 = 9_000;

/// Orçamento de janela para o gate de risco.
///
/// O texto "PLACE YOUR BETS" é lido por polling e só vira DEPOIS do resultado, mas o DAG roda no
/// instante do giro. Usar só esse texto como fonte fazia todo giro cair em `Skip(WindowTooShort)`
/// e nenhuma entrada acontecer — nem no dry-run. O evento de spin é a autoridade: o resultado é
/// o que abre a janela. `round_open` volta a decidir quando o giro é antigo (replay, reconexão,
/// fita DOM chegando com atraso), aí não há janela nova para apostar.
pub fn window_budget_ms(round_open: bool, spin_age_ms: i64) -> u32 {
    const FRESH_SPIN_MS: i64 = 5_000;
    if round_open {
        return BET_WINDOW_MS;
    }
    if spin_age_ms <= FRESH_SPIN_MS {
        // Giro recém-chegado: desconta o que já passou desde o resultado (idade negativa é
        // relógio do sidecar à frente, não janela gasta).
        return BET_WINDOW_MS.saturating_sub(spin_age_ms.max(0) as u32);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use monster_risk::{Cfg, State, Verdict};
    use monster_store::{init_pool, orphan_sweep};
    use monster_strategy_native::{default_matchers, Cover};

    #[test]
    fn giro_recem_chegado_abre_janela_mesmo_com_texto_da_ui_atrasado() {
        // Texto da UI ainda diz "aguarde" (round_open=false) porque o polling não alcançou o
        // resultado — o giro recém-chegado manda, senão toda entrada vira Skip(WindowTooShort).
        assert_eq!(window_budget_ms(false, 40), BET_WINDOW_MS - 40);
        assert_eq!(window_budget_ms(false, 0), BET_WINDOW_MS);
        // Mesa confirmadamente aberta: janela cheia.
        assert_eq!(window_budget_ms(true, 9_000), BET_WINDOW_MS);
        // Giro antigo (replay/reconexão): não existe janela nova para apostar.
        assert_eq!(window_budget_ms(false, 20_000), 0);
        // Idade negativa (relógio do sidecar à frente) não pode virar janela gigante.
        assert_eq!(window_budget_ms(false, -500), BET_WINDOW_MS);
    }

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
            // Degrau 3+ leva a ficha de proteção no 0 como terceira cobertura.
            let expected_items = if plan.ladder_step >= crate::dag::ZERO_INSURANCE_FROM_STEP { 3 } else { 2 };
            assert_eq!(fills.items.len(), expected_items);

            let rec = reconcile_spin(
                &pool,
                PlayedEntry {
                    strategy_id: &plan.strategy_id,
                    ladder_step: plan.ladder_step,
                    acks: &fills.items,
                },
                19, // Coluna 2 vence (Col(2) e Col(3) cobertas)
                &cfg,
                // Extrato real do acerto: +1 ficha da coluna menos a ficha de protecao do 0 (degrau 3+).
                Some(plan.stake_per_cover_cents as i64
                    - (plan.total_cents - 2 * plan.stake_per_cover_cents) as i64),
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

    /// Espião que conta se o executor pediu a cobertura em lote (`click_cover_repeat`) ou
    /// ficha por ficha (`click_cover`). Resolver o alvo por ficha custava varredura de DOM +
    /// espera de janela + 280 ms cada e estourava a janela de aposta no meio da cobertura.
    struct RepeatSpy {
        inner: DryRunActuator,
        repeats: std::sync::Mutex<Vec<u32>>,
        singles: std::sync::Mutex<u32>,
    }

    impl RepeatSpy {
        fn new() -> Self {
            Self {
                inner: DryRunActuator::new(),
                repeats: std::sync::Mutex::new(Vec::new()),
                singles: std::sync::Mutex::new(0),
            }
        }
    }

    impl CdpActuator for RepeatSpy {
        fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
            self.inner.click(x, y)
        }
        fn click_cover(&self, cover: &Cover, cal: &Calibration) -> Result<(), ExecError> {
            if let Ok(mut n) = self.singles.lock() {
                *n += 1;
            }
            self.inner.click_cover(cover, cal)
        }
        fn click_cover_repeat(
            &self,
            cover: &Cover,
            cal: &Calibration,
            n: u32,
        ) -> Result<(), ExecError> {
            if let Ok(mut v) = self.repeats.lock() {
                v.push(n);
            }
            self.inner.click_cover_repeat(cover, cal, n)
        }
        fn click_chip(&self, denom_cents: u64, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.click_chip(denom_cents, cal)
        }
        fn read_bet_total(&self) -> Result<u64, ExecError> {
            self.inner.read_bet_total()
        }
        fn clear_bets(&self, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.clear_bets(cal)
        }
        fn confirm(&self, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.confirm(cal)
        }
        fn collect_acks(&self, plan: &Plan) -> Result<Vec<CoverAck>, ExecError> {
            self.inner.collect_acks(plan)
        }
    }

    #[test]
    fn cobertura_com_varias_fichas_pede_o_lote_uma_vez_so() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let spy = RepeatSpy::new();
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();

        // 40¢ numa dúzia = duas fichas de 20¢ depois de um único clique de ficha.
        let plan = Plan {
            spin_id: "spin-lote-1".to_string(),
            strategy_id: "cluster_terminal".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 40,
            total_cents: 40,
            ev_cents: -1,
            confidence: 0.75,
            covers: vec![(Cover::Dozen(1), 40)],
            chips_per_cover: vec![(20, 2)],
        };

        let fills = place(&pool, &spy, &plan, &cal, &kill_switch, 1_730_000_020_000)
            .expect("entrada de uma cobertura deve fechar com preview correto");
        assert_eq!(fills.preview_cents, 40);

        let repeats = spy.repeats.lock().map(|v| v.clone()).unwrap_or_default();
        assert_eq!(
            repeats,
            vec![2],
            "a cobertura deve resolver o alvo UMA vez e despachar 2 fichas nele"
        );
        let singles = spy.singles.lock().map(|n| *n).unwrap_or(u32::MAX);
        assert_eq!(singles, 0, "nenhuma ficha pode ser clicada com resolução própria");
    }

    /// Ao vivo o pano engoliu a terceira ficha de um lote de 3 (`lido=20 esperado=30`).
    /// A segunda passada repõe SÓ a diferença e a entrada fecha correta.
    #[test]
    fn ficha_engolida_pela_animacao_e_reposta_na_mesma_cobertura() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let actuator = DryRunActuator::new();
        actuator.set_swallow_chips(1);
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();

        let plan = Plan {
            spin_id: "spin-engole-1".to_string(),
            strategy_id: "quebra_alto_baixo_6x".to_string(),
            ladder_step: 2,
            stake_per_cover_cents: 30,
            total_cents: 30,
            ev_cents: -1,
            confidence: 0.75,
            covers: vec![(Cover::High, 30)],
            chips_per_cover: vec![(10, 3)],
        };

        let fills = place(&pool, &actuator, &plan, &cal, &kill_switch, 1_730_000_030_000)
            .expect("ficha engolida deve ser reposta, nao abortada");
        assert_eq!(fills.preview_cents, 30);
        assert_eq!(actuator.cleared_count(), 0, "nao houve divergencia, nada a limpar");
    }

    /// A reposicao e uma so: se ela tambem perder fichas, o total segue errado e o certo e
    /// abortar (clear + abort), nunca ficar insistindo — insistir cegamente vira aposta dupla.
    #[test]
    fn reposicao_insuficiente_aborta_em_vez_de_insistir() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        let actuator = DryRunActuator::new();
        // 3 do lote + 1 da reposicao engolidas: sobram 20 de 30 esperados.
        actuator.set_swallow_chips(4);
        let cal = Calibration::default();
        let kill_switch = KillSwitch::new();

        let plan = Plan {
            spin_id: "spin-engole-2".to_string(),
            strategy_id: "quebra_alto_baixo_6x".to_string(),
            ladder_step: 2,
            stake_per_cover_cents: 30,
            total_cents: 30,
            ev_cents: -1,
            confidence: 0.75,
            covers: vec![(Cover::High, 30)],
            chips_per_cover: vec![(10, 3)],
        };

        let err = place(&pool, &actuator, &plan, &cal, &kill_switch, 1_730_000_040_000)
            .expect_err("duas fichas perdidas devem abortar");
        assert!(
            matches!(err, ExecError::PreviewMismatch { .. }),
            "esperava divergencia, veio {err:?}"
        );
        assert_eq!(actuator.cleared_count(), 1, "divergencia limpa a mesa");
        assert_eq!(actuator.confirmed_count(), 0, "nunca confirma com total errado");
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
        // A divergência é detectada já na primeira cobertura (10¢ esperados, 20¢ na mesa),
        // antes de empilhar a segunda cobertura.
        assert!(matches!(
            err,
            ExecError::PreviewMismatch {
                preview: 20,
                expected: 10
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

#[cfg(test)]
mod ladder_isolation_tests {
    use super::*;
    use monster_risk::{stake_cents, Cfg, State};
    use monster_strategy_native::default_matchers;

    fn estado_neutro() -> State {
        State {
            bankroll_cents: 45_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 9000,
            spins_since_blow: 10,
            step: 0,
        }
    }

    /// O degrau pertence à estratégia: a perda de um padrão não pode aumentar a primeira
    /// entrada de outro (foi assim que a escada entrou em degrau 2 sem nenhuma perda dele).
    #[test]
    fn gale_do_script_sai_no_giro_seguinte_sem_gatilho_novo() {
        // 13/11: Espelhos perderam no 19 e o gale não saiu porque 19 não é espelho.
        let cfg = Cfg::default();
        let state = estado_neutro();
        let espelhos = monster_strategy_native::operator_script("script_espelhos_gemeos").expect("script");
        let window = [19u8, 31, 7, 4, 1, 28, 5, 14, 9, 36, 17, 0];
        assert!(monster_strategy_native::Matcher::evaluate(&espelhos, &window).is_none(), "19 não é gatilho");
        let matchers: Vec<std::sync::Arc<dyn monster_strategy_native::Matcher>> =
            vec![std::sync::Arc::new(monster_strategy_native::ScriptGale(espelhos))];
        let mut steps = LadderSteps::new();
        steps.insert("script_espelhos_gemeos".to_string(), 1);
        let (out, _) = evaluate_dag_with_history(
            "gale-script",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx { state: &state, ladder_steps: &steps },
            0.55,
        );
        let plan = out.plan.expect("gale sai");
        assert_eq!(plan.strategy_id, "script_espelhos_gemeos");
        assert_eq!(plan.ladder_step, 2);
        assert_eq!(plan.covers.len(), 22);
        assert_eq!(plan.stake_per_cover_cents, 20);

        // No degrau 3 perdido, o script estoura: não existe degrau 4.
        steps.insert("script_espelhos_gemeos".to_string(), 3);
        let (out4, _) = evaluate_dag_with_history(
            "gale-script-4",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx { state: &state, ladder_steps: &steps },
            0.55,
        );
        assert!(out4.plan.is_none(), "script para no degrau 3");
    }

    #[test]
    fn degrau_da_escada_pertence_a_estrategia_vencedora() {
        let matchers = default_matchers();
        let cfg = Cfg::default();
        let state = estado_neutro();
        let window = [2u8, 5, 8, 11, 14, 19, 25, 30, 1, 4, 7, 10, 15, 22, 31, 33, 0, 12, 18, 27];

        let (out, _) = evaluate_dag_with_history(
            "spin-isolado-1",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx {
                state: &state,
                ladder_steps: &LadderSteps::new(),
            },
            0.50,
        );
        let base = out.plan.expect("a janela de referência dispara entrada");
        assert_eq!(base.ladder_step, 1, "primeira entrada do padrão é degrau 1");
        assert_eq!(base.stake_per_cover_cents, stake_cents(1, cfg.base_cents));

        // Outra estratégia caiu 2 degraus: não pode contaminar a entrada da vencedora.
        let mut steps = LadderSteps::new();
        steps.insert("quebra_permanencia_duzia".to_string(), 2);
        steps.insert("setor_vies".to_string(), 4);
        let (out2, _) = evaluate_dag_with_history(
            "spin-isolado-2",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx {
                state: &state,
                ladder_steps: &steps,
            },
            0.50,
        );
        let outra = out2.plan.expect("mesma janela, mesma entrada");
        assert_eq!(
            outra.ladder_step, 1,
            "degrau de outra estratégia nunca é aplicado nesta entrada"
        );

        // A própria estratégia no degrau 2 entra no 3, com a ficha ×3 da escada (§6).
        let mut propria = LadderSteps::new();
        propria.insert(base.strategy_id.clone(), 2);
        let (out3, _) = evaluate_dag_with_history(
            "spin-isolado-3",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx {
                state: &state,
                ladder_steps: &propria,
            },
            0.50,
        );
        let terceiro = out3.plan.expect("entrada da própria estratégia");
        assert_eq!(terceiro.ladder_step, 3);
        // Degrau 3 da propria escada: 2 x 90c (base 10c) + 10c de protecao no 0.
        assert_eq!(terceiro.total_cents, 190);

        // Com a escada carregada, um `step` global alto (resíduo de outra estratégia) é ignorado.
        let carregado = State { step: 3, ..state };
        let mut apenas_outra = LadderSteps::new();
        apenas_outra.insert("quebra_permanencia_duzia".to_string(), 2);
        let (out4, _) = evaluate_dag_with_history(
            "spin-isolado-4",
            &window,
            &[],
            &matchers,
            &cfg,
            &GateCtx {
                state: &carregado,
                ladder_steps: &apenas_outra,
            },
            0.50,
        );
        assert_eq!(
            out4.plan.expect("entrada").ladder_step,
            1,
            "degrau global residual não pode empurrar a entrada para degrau 4"
        );
        assert_eq!(
            terceiro.stake_per_cover_cents,
            stake_cents(3, cfg.base_cents),
            "degrau 3 = 90¢ por cobertura"
        );
        assert_eq!(terceiro.total_cents, 190, "180 + 10 de protecao no 0");
    }
}
