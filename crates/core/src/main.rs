// SPDX-License-Identifier: Apache-2.0
//! Binário principal do orquestrador `monster-core` (DAG + Risco + Executor + Reconciliador + FUI Server).
//! Opera em `--dry-run` por padrão (§1.5), valida Monte Carlo no boot (§13), consome o sidecar em `127.0.0.1:8787`
//! e serve o painel FUI em tempo real (`4 Hz`) em `http://127.0.0.1:8791`.

use monster_capture::{CaptureSource, SpinBus, StreamClient, DEFAULT_STREAM_ADDR};
use monster_core::{
    boot_reconcile, evaluate_all_matchers_history, evaluate_dag_with_history, place,
    reconcile_spin, Calibration, CdpActuator, CoverAck, DryRunActuator, ExecutionMode,
    HistoryBuffer, KillSwitch, LiveHttpActuator, Plan, Window30, REAL_TABLE_479_RECENT_FIRST,
};
use monster_risk::{run_monte_carlo, Cfg, McParams, State, Verdict};
use monster_store::{init_pool, insert_spin, NewSpin, SpinSource, SqlitePool};
use monster_strategy_native::default_matchers;
use monster_ui::FuiController;
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LIVE_FUI_HTML: &str = include_str!("../../../ui/live.html");

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn spawn_fui_http_server(
    pool: SqlitePool,
    fui: FuiController,
    kill_switch: KillSwitch,
    history: Arc<Mutex<HistoryBuffer>>,
    cfg: Cfg,
) {
    thread::spawn(move || {
        let matchers = default_matchers();
        let Ok(listener) = TcpListener::bind("127.0.0.1:8791") else {
            eprintln!("[FUI-HTTP] porta 127.0.0.1:8791 já em uso");
            return;
        };
        println!("[FUI-HTTP] Painel de Operação vivo em http://127.0.0.1:8791");

        for stream_res in listener.incoming() {
            let Ok(mut stream) = stream_res else {
                continue;
            };
            let mut buf = [0u8; 2048];
            let Ok(n) = stream.read(&mut buf) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or("");

            if first_line.starts_with("GET /api/tick") {
                if let Ok(tick) = fui.build_bus_tick(&pool, "quebra_permanencia_coluna") {
                    if let Ok(body) = serde_json::to_string(&tick) {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        continue;
                    }
                }
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n");
            } else if first_line.starts_with("GET /api/patterns") {
                let chrono = history
                    .lock()
                    .map(|h| h.to_chronological_vec())
                    .unwrap_or_default();
                let stats = evaluate_all_matchers_history(&matchers, &chrono, &cfg);
                if let Ok(body) = serde_json::to_string(&stats) {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    continue;
                }
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n");
            } else if first_line.starts_with("POST /api/kill") {
                kill_switch.trip();
                let _ = fui.kill();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
            } else if first_line.starts_with("POST /api/pause") {
                let _ = fui.pause();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
            } else if first_line.starts_with("POST /api/resume") {
                kill_switch.reset();
                let _ = fui.resume();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
            } else {
                let body = LIVE_FUI_HTML.as_bytes();
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(hdr.as_bytes());
                let _ = stream.write_all(body);
            }
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut mode = ExecutionMode::DryRun;
    let mut stream_addr = DEFAULT_STREAM_ADDR.to_string();
    let mut db_uri = "O:/Archives/Eistein/FUI AGENT ROULLET/monster.db".to_string();
    let mut bankroll_cents: i64 = 7_058;
    let mut cfg = Cfg {
        base_cents: 10,
        max_steps: 4,
        max_bet_per_cover_cents: 100_000,
        max_exposure_pct: 0.10,
        stop_loss_cents: 1600,
        stop_win_pct: 0.08,
        min_window_ms: 2000,
        cooldown_spins: 2,
        max_entries_per_hour: 40,
        chip_denoms_cents: vec![2500, 1000, 500, 100, 50, 10],
    };

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
    let dry_actuator = DryRunActuator::new();
    let live_actuator = LiveHttpActuator::new(stream_addr.clone());
    let kill_switch = KillSwitch::new();
    let fui = FuiController::new(cfg.clone());
    let boot_instant = std::time::Instant::now();
    fui.sync_live_metrics(0, bankroll_cents, 0, None, None);

    let mut window = Window30::new();
    let mut initial_hist = HistoryBuffer::new();
    initial_hist.seed_from_recent_first(&REAL_TABLE_479_RECENT_FIRST);
    if let Ok(db_spins) = fui.get_spins(&pool) {
        for sp in db_spins.iter().rev() {
            initial_hist.push_live(sp.number);
        }
    }
    for n in initial_hist.to_recent_vec(30).into_iter().rev() {
        window.push(n);
    }

    let init_chrono = initial_hist.to_chronological_vec();
    let init_stats = evaluate_all_matchers_history(&matchers, &init_chrono, &cfg);
    println!(
        "=== [WALK-FORWARD: {} GIROS DA MESA ANALISADOS] ===",
        init_chrono.len()
    );
    for st in &init_stats {
        println!("  -> {:<28} | {}", st.strategy_id, st.summary);
    }
    println!("====================================================");

    let history = Arc::new(Mutex::new(initial_hist));
    spawn_fui_http_server(
        pool.clone(),
        fui.clone(),
        kill_switch.clone(),
        history.clone(),
        cfg.clone(),
    );

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
        let chrono_hist = if let Ok(mut h) = history.lock() {
            h.push_live(ev.number);
            h.to_chronological_vec()
        } else {
            Vec::new()
        };
        let recent = window.to_recent_vec();
        let st = State {
            bankroll_cents,
            exposure_cents: 0,
            session_pnl_cents,
            remaining_window_ms: 9000,
            spins_since_blow,
            step: current_step,
        };

        let (dag_out, best_hist) = evaluate_dag_with_history(
            &ev.spin_id,
            &recent,
            &chrono_hist,
            &matchers,
            &cfg,
            &st,
            0.55,
        );
        println!(
            "[DAG] spin={} num={} | regime={:?} | sinais={} | verdict={:?} | hist={} | {}µs",
            ev.spin_id,
            ev.number,
            dag_out.regime,
            dag_out.aggregates.len(),
            dag_out.verdict,
            best_hist
                .as_ref()
                .map(|h| h.summary.as_str())
                .unwrap_or("sem-sinal"),
            dag_out.elapsed_us
        );

        if let (Some(Verdict::Place), Some(plan)) = (dag_out.verdict, dag_out.plan) {
            let actuator: &dyn CdpActuator = match mode {
                ExecutionMode::DryRun => &dry_actuator,
                ExecutionMode::Live => &live_actuator,
            };
            match place(&pool, actuator, &plan, &cal, &kill_switch, now_ms) {
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
                    let label = format!(
                        "{} · R$ {},{:02}",
                        plan.covers
                            .iter()
                            .map(|(c, _)| c.canonical_key().to_uppercase())
                            .collect::<Vec<_>>()
                            .join(" + "),
                        plan.stake_per_cover_cents / 100,
                        plan.stake_per_cover_cents % 100
                    );
                    fui.sync_live_metrics(
                        boot_instant.elapsed().as_secs(),
                        bankroll_cents,
                        session_pnl_cents,
                        Some(label),
                        Some(plan.total_cents),
                    );
                    pending_entry = Some((plan, fills.items));
                }
                Err(err) => {
                    eprintln!("[EXECUTOR-ABORT] {err}");
                }
            }
        } else {
            fui.sync_live_metrics(
                boot_instant.elapsed().as_secs(),
                bankroll_cents,
                session_pnl_cents,
                None,
                None,
            );
        }
    }

    Ok(())
}
