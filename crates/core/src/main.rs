// SPDX-License-Identifier: Apache-2.0
//! Binário principal do orquestrador `monster-core` (DAG + Risco + Executor + Reconciliador).
//! Opera em `--dry-run` por padrão (§1.5), valida Monte Carlo no boot (§13) e consome o sidecar em `127.0.0.1:8787`.
//! Nunca para a captura por erro de execução (§1.7).

use monster_capture::{CaptureSource, SpinBus, StreamClient, DEFAULT_STREAM_ADDR};
use monster_core::{
    boot_reconcile, evaluate_dag, place, reconcile_spin, Calibration, CoverAck, DryRunActuator,
    ExecutionMode, KillSwitch, Plan, Window30,
};
use monster_risk::{run_monte_carlo, Cfg, McParams, State, Verdict};
use monster_store::{init_pool, insert_spin, NewSpin, SpinSource};
use monster_strategy_native::default_matchers;
use std::env;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut mode = ExecutionMode::DryRun;
    let mut stream_addr = DEFAULT_STREAM_ADDR.to_string();
    let mut db_uri = "O:/Archives/Eistein/FUI AGENT ROULLET/monster.db".to_string();
    let mut bankroll_cents: i64 = 45_000;
    let mut cfg = Cfg::default();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--live" => mode = ExecutionMode::Live,
            "--dry-run" => mode = ExecutionMode::DryRun,
            "--stream" if i + 1 < args.len() => {
                stream_addr = args[i + 1]
                    .trim_start_matches("http://")
                    .trim_end_matches("/stream")
                    .to_string();
                i += 1;
            }
            "--db" if i + 1 < args.len() => {
                db_uri.clone_from(&args[i + 1]);
                i += 1;
            }
            "--bank-cents" if i + 1 < args.len() => {
                if let Ok(v) = args[i + 1].parse::<i64>() {
                    bankroll_cents = v;
                }
                i += 1;
            }
            "--base-cents" if i + 1 < args.len() => {
                if let Ok(v) = args[i + 1].parse::<u64>() {
                    cfg.base_cents = v;
                }
                i += 1;
            }
            "--steps" if i + 1 < args.len() => {
                if let Ok(v) = args[i + 1].parse::<u8>() {
                    cfg.max_steps = v;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    // 1. Validação obrigatória de Monte Carlo antes de iniciar (§13 e §15.4).
    let mc_report = run_monte_carlo(&McParams {
        initial_bankroll_cents: bankroll_cents,
        cfg: cfg.clone(),
        ..McParams::default()
    });
    println!(
        "[MONSTER-CORE] Monte Carlo (10.000 sessões) | P(ruína)={:.2}% | DD p95={}¢ | EV/h={:.1}¢",
        mc_report.p_ruin * 100.0,
        mc_report.drawdown_p95_cents,
        mc_report.ev_per_hour_cents
    );

    // 2. Inicializa SQLite WAL e reconcilia órfãos no boot (§9).
    let pool = init_pool(&db_uri)?;
    let boot = boot_reconcile(
        &pool,
        Some(Path::new("O:/Archives/Eistein/roulette-profile")),
    )?;
    println!(
        "[MONSTER-CORE] Boot reconciliado | modo={:?} | orphans_swept={} | singleton_lock_removed={}",
        mode, boot.orphans_swept, boot.singleton_lock_removed
    );

    let matchers = default_matchers();
    let cal = Calibration::default();
    let actuator = DryRunActuator::new();
    let kill_switch = KillSwitch::new();
    let mut window = Window30::new();
    let mut session_pnl_cents = 0i64;
    let mut spins_since_blow = cfg.cooldown_spins;
    let mut current_step = 0u8;
    let mut pending_entry: Option<(Plan, Vec<CoverAck>)> = None;

    let bus = SpinBus::new();
    let rx = bus.subscribe();
    let addr_for_worker = stream_addr.clone();

    thread::spawn(move || {
        let mut client = StreamClient::new(addr_for_worker, bus);
        loop {
            if let Err(err) = client.connect_and_consume(None) {
                eprintln!("[MONSTER-CORE] stream desconectado ({err}), reconectando em 2s...");
                thread::sleep(Duration::from_secs(2));
            }
        }
    });

    println!("[MONSTER-CORE] Conectado ao sidecar em {stream_addr} — aguardando giros...");

    while let Ok(ev) = rx.recv() {
        let now_ms = now_unix_ms();
        let src = match ev.source {
            CaptureSource::Ws => SpinSource::Ws,
            CaptureSource::Mo => SpinSource::Mo,
            CaptureSource::Ext => SpinSource::Ext,
            CaptureSource::Ocr => SpinSource::Ocr,
        };
        let _ = insert_spin(
            &pool,
            &NewSpin {
                spin_id: ev.spin_id.clone(),
                number: ev.number,
                ts: ev.ts,
                source: src,
                received_at: now_ms,
                latency_ms: Some(ev.latency_ms),
            },
        );

        // Reconcilia entrada pendente do spin anterior, se houver (§9).
        if let Some((prev_plan, prev_acks)) = pending_entry.take() {
            match reconcile_spin(
                &pool,
                &prev_plan.strategy_id,
                ev.number,
                &prev_acks,
                &cfg,
                None,
                now_ms,
            ) {
                Ok(rep) => {
                    bankroll_cents += rep.expected_delta_cents;
                    session_pnl_cents += rep.expected_delta_cents;
                    current_step = rep.next_step;
                    if rep.ladder_blown {
                        spins_since_blow = 0;
                    } else {
                        spins_since_blow = spins_since_blow.saturating_add(1);
                    }
                    println!(
                        "[RECONCILE] spin={} num={} | won={} delta={}¢ | pnl={}¢ | prox_degrau={}",
                        ev.spin_id,
                        ev.number,
                        rep.won,
                        rep.expected_delta_cents,
                        session_pnl_cents,
                        current_step
                    );
                }
                Err(err) => {
                    eprintln!("[RECONCILE-ERR] {err}");
                }
            }
        } else {
            spins_since_blow = spins_since_blow.saturating_add(1);
        }

        window.push(ev.number);
        let recent = window.to_recent_vec();
        let st = State {
            bankroll_cents,
            exposure_cents: 0,
            session_pnl_cents,
            remaining_window_ms: 9000,
            spins_since_blow,
            step: current_step,
        };

        let dag_out = evaluate_dag(&ev.spin_id, &recent, &matchers, &cfg, &st, 0.55);
        println!(
            "[DAG] spin={} num={} | regime={:?} | sinais={} | verdict={:?} | {}µs",
            ev.spin_id,
            ev.number,
            dag_out.regime,
            dag_out.aggregates.len(),
            dag_out.verdict,
            dag_out.elapsed_us
        );

        if let (Some(Verdict::Place), Some(plan)) = (dag_out.verdict, dag_out.plan) {
            match place(&pool, &actuator, &plan, &cal, &kill_switch, now_ms) {
                Ok(fills) => {
                    println!(
                        "[EXECUTOR {:?}] ENTRADA CONFIRMADA | strategy={} | degrau={} | total={}¢ | preview={}¢ | covers={:?}",
                        mode,
                        plan.strategy_id,
                        plan.ladder_step,
                        plan.total_cents,
                        fills.preview_cents,
                        plan.covers
                    );
                    pending_entry = Some((plan, fills.items));
                }
                Err(err) => {
                    eprintln!("[EXECUTOR-ABORT] {err}");
                }
            }
        }
    }

    Ok(())
}
