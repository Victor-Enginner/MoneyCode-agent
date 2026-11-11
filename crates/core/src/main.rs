// SPDX-License-Identifier: Apache-2.0
//! Binário principal do orquestrador `monster-core` (DAG + Risco + Executor + Reconciliador + FUI Server).
//! Sincroniza 100% ao vivo o Saldo real (`Balance`), P&L real, Aposta na mesa (`Bet`) e a fita de 12 números da Playtech,
//! garantindo coerência total entre Colunas (`2to1`) e Dúzias (`1st/2nd/3rd 12`) na FUI (`http://127.0.0.1:8791`).

use monster_capture::{CaptureSource, SpinBus, StreamClient, DEFAULT_STREAM_ADDR};
use monster_core::{
    boot_reconcile, build_brain_snapshot, evaluate_all_matchers_history,
    evaluate_dag_with_history, has_recent_number, load_chronological_history, place, reconcile_spin,
    sync_strip_to_db, write_gbrain_file, Calibration, CdpActuator, CoverAck, DryRunActuator,
    ExecutionMode, HistoryBuffer, KillSwitch, LiveHttpActuator, Plan, Window30, HISTORY_CAPACITY,
    WINDOW_CAPACITY,
};
use monster_risk::{run_monte_carlo, Cfg, McParams, State, Verdict};
use monster_store::{
    init_pool, insert_intent, insert_spin, IntentDecision, NewIntent, NewSpin, SpinSource,
    SqlitePool,
};
use monster_strategy_native::default_matchers;
use monster_ui::FuiController;
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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

/// Remove apenas os spins legados gravados com `spin_id` do formato antigo `rid-<número>`,
/// que colapsavam giros diferentes no mesmo identificador e corrompiam a TIMELINE.
/// Preserva `intents`/`fills`: registro financeiro nunca é apagado por rotina de higiene.
fn cleanup_legacy_rid_spins(pool: &SqlitePool) -> u64 {
    pool.with_conn(|conn| {
        let deleted = conn.execute(
            "DELETE FROM spins WHERE spin_id GLOB 'rid-[0-9]' OR spin_id GLOB 'rid-[0-9][0-9]'",
            [],
        )?;
        Ok(deleted as u64)
    })
    .unwrap_or(0)
}

/// Recarrega `Window30` e `HistoryBuffer` a partir do banco para que DAG, walk-forward e TIMELINE
/// enxerguem exatamente a mesma sequência de giros que a mesa da Playtech está exibindo.
fn reload_live_history(
    pool: &SqlitePool,
    window: &Arc<Mutex<Window30>>,
    history: &Arc<Mutex<HistoryBuffer>>,
) {
    let chrono = load_chronological_history(pool, HISTORY_CAPACITY);
    if let Ok(mut h) = history.lock() {
        let mut fresh = HistoryBuffer::new();
        for &n in &chrono {
            fresh.push_live(n);
        }
        *h = fresh;
    }
    if let Ok(mut w) = window.lock() {
        let mut fresh = Window30::new();
        let start = chrono.len().saturating_sub(WINDOW_CAPACITY);
        for &n in &chrono[start..] {
            fresh.push(n);
        }
        *w = fresh;
    }
}

fn spawn_fui_http_server(
    pool: SqlitePool,
    fui: FuiController,
    kill_switch: KillSwitch,
    history: Arc<Mutex<HistoryBuffer>>,
    cfg: Cfg,
) {
    thread::spawn(move || {
        let boot_instant = std::time::Instant::now();
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
                fui.touch_uptime(boot_instant.elapsed().as_secs());
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
            } else if first_line.starts_with("GET /api/brain") {
                let chrono = history
                    .lock()
                    .map(|h| h.to_chronological_vec())
                    .unwrap_or_default();
                let snap = build_brain_snapshot(&matchers, &chrono, &cfg);
                if let Ok(body) = serde_json::to_string(&snap) {
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
                let disk_html = std::fs::read("O:/Archives/Eistein/FUI AGENT ROULLET/ui/live.html")
                    .unwrap_or_else(|_| LIVE_FUI_HTML.as_bytes().to_vec());
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    disk_html.len()
                );
                let _ = stream.write_all(hdr.as_bytes());
                let _ = stream.write_all(&disk_html);
            }
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut mode = ExecutionMode::DryRun;
    let mut stream_addr = DEFAULT_STREAM_ADDR.to_string();
    let mut db_uri = "O:/Archives/Eistein/FUI AGENT ROULLET/monster.db".to_string();
    let mut bankroll_cents: i64 = 5_088;
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

    // 2. Inicializa SQLite WAL, remove registros inválidos antigos e reconcilia órfãos no boot (§9).
    let pool = init_pool(&db_uri)?;
    let legacy_removed = cleanup_legacy_rid_spins(&pool);
    if legacy_removed > 0 {
        println!("[MONSTER-CORE] {legacy_removed} spins legados (spin_id rid-<n>) removidos da TIMELINE");
    }
    let boot = boot_reconcile(&pool, None)?;
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

    // Histórico inicial exclusivamente do banco (sincronizado com a fita da mesa): nada de números inventados.
    let initial_chrono = load_chronological_history(&pool, HISTORY_CAPACITY);
    let mut initial_hist = HistoryBuffer::new();
    for &n in &initial_chrono {
        initial_hist.push_live(n);
    }
    let window = Arc::new(Mutex::new({
        let mut w = Window30::new();
        let start = initial_chrono.len().saturating_sub(WINDOW_CAPACITY);
        for &n in &initial_chrono[start..] {
            w.push(n);
        }
        w
    }));

    let init_chrono = initial_hist.to_chronological_vec();
    let init_snap = build_brain_snapshot(&matchers, &init_chrono, &cfg);
    let _ = write_gbrain_file(
        std::path::Path::new("O:/Archives/Eistein/FUI AGENT ROULLET/docs/memory/BRAIN_STATE.md"),
        &init_snap,
    );
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
    let shared_bankroll_cents = Arc::new(AtomicI64::new(0));
    let shared_pnl_cents = Arc::new(AtomicI64::new(0));
    let initial_table_balance = Arc::new(AtomicI64::new(0));
    let table_round_open = Arc::new(AtomicBool::new(false));
    // `Game Code` da rodada exibida pela Playtech: chave de "uma entrada por rodada".
    let table_round = Arc::new(Mutex::new(String::new()));

    // Thread de sincronização contínua (500 ms) com o DOM da Playtech:
    // saldo real, P&L real, aposta na mesa, último ganho e fita de 12 números -> SQLite -> FUI.
    {
        let live_act_poll = live_actuator.clone();
        let fui_poll = fui.clone();
        let pool_poll = pool.clone();
        let win_poll = window.clone();
        let hist_poll = history.clone();
        let bank_atom = shared_bankroll_cents.clone();
        let pnl_atom = shared_pnl_cents.clone();
        let init_bal_atom = initial_table_balance.clone();
        let round_open_atom = table_round_open.clone();
        let round_key_atom = table_round.clone();

        thread::spawn(move || loop {
            if let Some(tel) = live_act_poll.fetch_telemetry() {
                round_open_atom.store(tel.round_open, Ordering::Relaxed);
                if !tel.game_code.is_empty() {
                    if let Ok(mut g) = round_key_atom.lock() {
                        g.clone_from(&tel.game_code);
                    }
                }

                if let Some(real_bal) = tel.balance_cents {
                    let prev_init = init_bal_atom.load(Ordering::Relaxed);
                    let base_bal = if prev_init <= 0 {
                        init_bal_atom.store(real_bal, Ordering::Relaxed);
                        real_bal
                    } else {
                        prev_init
                    };
                    bank_atom.store(real_bal, Ordering::Relaxed);
                    pnl_atom.store(real_bal - base_bal, Ordering::Relaxed);
                }

                // A fita da Playtech é a autoridade da TIMELINE: grava só os giros ausentes e recarrega os buffers.
                if tel.strip.len() >= 4 {
                    match sync_strip_to_db(&pool_poll, &tel.strip, &tel.game_code, now_unix_ms()) {
                        Ok(report) if report.inserted > 0 => {
                            reload_live_history(&pool_poll, &win_poll, &hist_poll);
                            println!(
                                "[TELEMETRIA-LIVE] fita={:?} | {} giro(s) novo(s){} | saldo=R$ {:.2} | aposta=R$ {:.2}",
                                tel.strip,
                                report.inserted,
                                if report.rebuilt { " (baseline regravada)" } else { "" },
                                (bank_atom.load(Ordering::Relaxed) as f64) / 100.0,
                                (tel.preview_cents as f64) / 100.0
                            );
                        }
                        Ok(_) => {}
                        Err(err) => eprintln!("[TELEMETRIA-LIVE] falha ao sincronizar fita: {err}"),
                    }
                }

                let balance = match bank_atom.load(Ordering::Relaxed) {
                    0 => None,
                    v => Some(v),
                };
                let pnl = if balance.is_some() {
                    Some(pnl_atom.load(Ordering::Relaxed))
                } else {
                    None
                };
                fui_poll.sync_table_telemetry(
                    balance,
                    pnl,
                    Some(tel.preview_cents),
                    Some(tel.last_win_cents),
                );
            }
            thread::sleep(Duration::from_millis(500));
        });
    }

    spawn_fui_http_server(
        pool.clone(),
        fui.clone(),
        kill_switch.clone(),
        history.clone(),
        cfg.clone(),
    );

    let mut spins_since_blow = cfg.cooldown_spins;
    let mut current_step = 0u8;
    let mut pending_entry: Option<(Plan, Vec<CoverAck>)> = None;
    let mut last_entry_round = String::new();
    let mut last_entry_at_ms: i64 = 0;

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
        // A mesma rodada pode chegar pelo WS e pela fita DOM: grava uma única vez.
        let already_recorded = has_recent_number(&pool, ev.number, ev.ts, 3_000).unwrap_or(false);
        if already_recorded {
            println!(
                "[DEDUPE] spin={} num={} já registrado nos últimos 3s (WS/fita) — ignorado",
                ev.spin_id, ev.number
            );
        } else {
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
        }

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
                    if initial_table_balance.load(Ordering::Relaxed) <= 0 {
                        shared_bankroll_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                        shared_pnl_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                    }
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
                        shared_pnl_cents.load(Ordering::Relaxed),
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

        // Buffers reconstruídos do banco (já sincronizado com a fita da Playtech):
        // DAG e TIMELINE nunca divergem nem duplicam o mesmo giro.
        reload_live_history(&pool, &window, &history);
        let recent = window
            .lock()
            .map(|w| w.to_recent_vec())
            .unwrap_or_default();
        let chrono_hist = history
            .lock()
            .map(|h| h.to_chronological_vec())
            .unwrap_or_default();

        let cur_bank = shared_bankroll_cents.load(Ordering::Relaxed);
        let cur_pnl = shared_pnl_cents.load(Ordering::Relaxed);
        // Só tenta entrar com a mesa em `PLACE YOUR BETS`; fora disso a janela é zero e o gate dá Skip.
        let window_open = table_round_open.load(Ordering::Relaxed);
        let st = State {
            bankroll_cents: if cur_bank > 0 { cur_bank } else { bankroll_cents },
            exposure_cents: 0,
            session_pnl_cents: cur_pnl,
            remaining_window_ms: if window_open { 9_000 } else { 0 },
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
            // Regra dura: no máximo UMA entrada por rodada da Playtech, mesmo que a rodada chegue
            // por duas fontes (WS + fita DOM) ou o DAG emita o sinal mais de uma vez.
            let round_key = table_round.lock().map(|g| g.clone()).unwrap_or_default();
            let same_round = (!round_key.is_empty() && last_entry_round == round_key)
                || (last_entry_at_ms > 0 && (now_ms - last_entry_at_ms) < 15_000);
            if same_round {
                println!(
                    "[EXECUTOR] rodada {} já possui entrada (última em {}ms) — sinal duplicado ignorado",
                    if round_key.is_empty() { "—" } else { round_key.as_str() },
                    now_ms - last_entry_at_ms
                );
                continue;
            }
            let actuator: &dyn CdpActuator = match mode {
                ExecutionMode::DryRun => &dry_actuator,
                ExecutionMode::Live => &live_actuator,
            };
            match place(&pool, actuator, &plan, &cal, &kill_switch, now_ms) {
                Ok(fills) => {
                    last_entry_at_ms = now_ms;
                    last_entry_round = round_key;
                    let cover_names = plan
                        .covers
                        .iter()
                        .map(|(c, _)| c.display_label())
                        .collect::<Vec<_>>()
                        .join(" + ");
                    println!(
                        "[EXECUTOR {:?}] ENTRADA CONFIRMADA | strategy={} | degrau={} | coberturas={} | total={}¢ | preview={}¢",
                        mode,
                        plan.strategy_id,
                        plan.ladder_step,
                        cover_names,
                        plan.total_cents,
                        fills.preview_cents,
                    );
                    let label = format!(
                        "{} · R$ {},{:02}",
                        cover_names,
                        plan.stake_per_cover_cents / 100,
                        plan.stake_per_cover_cents % 100
                    );
                    let live_bank = shared_bankroll_cents.load(Ordering::Relaxed);
                    fui.sync_live_metrics(
                        boot_instant.elapsed().as_secs(),
                        if live_bank > 0 { Some(live_bank) } else { None },
                        Some(shared_pnl_cents.load(Ordering::Relaxed)),
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
            // Verifica se algum padrão entrou em pré-alerta de observação (3x em Coluna/Dúzia ou 5x em Cor/Alto-Baixo/Par-Ímpar)
            if let Some(pre) = dag_out
                .aggregates
                .iter()
                .find(|a| a.confidence >= 0.40 && a.confidence < 0.55)
            {
                let covers_str = pre
                    .cover
                    .iter()
                    .map(|c| c.display_label())
                    .collect::<Vec<_>>()
                    .join(" + ");
                let pre_reason = format!(
                    "OBSERVANDO (pré-gatilho armado p/ {}) — entra na próxima repetição",
                    covers_str
                );
                println!(
                    "[OBSERVANDO] spin={} | padrao={} | alvo={} (aguardando confirmacao no proximo giro)",
                    ev.spin_id, pre.strategy_id, covers_str
                );
                let _ = insert_intent(
                    &pool,
                    &NewIntent {
                        spin_id: ev.spin_id.clone(),
                        strategy_id: pre.strategy_id.clone(),
                        confidence: pre.confidence,
                        ev_cents: 0,
                        decision: IntentDecision::Skipped,
                        reason: Some(pre_reason),
                        ladder_step: current_step,
                        stake_cents: 0,
                        created_at: now_ms,
                    },
                );
            }
            let live_bank = shared_bankroll_cents.load(Ordering::Relaxed);
            fui.sync_live_metrics(
                boot_instant.elapsed().as_secs(),
                if live_bank > 0 { Some(live_bank) } else { None },
                Some(shared_pnl_cents.load(Ordering::Relaxed)),
                None,
                None,
            );
        }
    }

    Ok(())
}
