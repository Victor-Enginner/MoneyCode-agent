// SPDX-License-Identifier: Apache-2.0
//! Binário principal do orquestrador `monster-core` (DAG + Risco + Executor + Reconciliador + FUI Server).
//! Sincroniza 100% ao vivo o Saldo real (`Balance`), P&L real, Aposta na mesa (`Bet`) e a fita de 12 números da Playtech,
//! garantindo coerência total entre Colunas (`2to1`) e Dúzias (`1st/2nd/3rd 12`) na FUI (`http://127.0.0.1:8791`).

use monster_capture::{CaptureSource, SpinBus, StreamClient, DEFAULT_STREAM_ADDR};
use monster_core::{
    boot_reconcile, build_brain_snapshot, evaluate_all_matchers_history,
    evaluate_dag_with_history, has_recent_number, load_chronological_history, place, reconcile_spin,
    sync_strip_to_db, write_gbrain_file, Calibration, CdpActuator, CoverAck, DryRunActuator,
    window_budget_ms, ExecutionMode, GateCtx, HistoryBuffer, KillSwitch, LadderSteps,
    LiveHttpActuator, Plan,
    PlayedEntry, Window30, HISTORY_CAPACITY, WINDOW_CAPACITY,
};
use monster_risk::{
    run_monte_carlo, run_monte_carlo_numbers, Cfg, McParams, NumbersMcParams, State, Verdict,
};
use monster_store::{
    all_ladder_steps, init_pool, insert_intent, insert_spin, IntentDecision, NewIntent, NewSpin,
    SpinSource, SqlitePool,
};
use monster_strategy_native::{Matcher, MatcherTuning};
use monster_ui::FuiController;
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LIVE_FUI_HTML: &str = include_str!("../../../ui/live.html");

/// Ajuste das sequências editado na dashboard: vive em disco para sobreviver ao restart.
const TUNING_PATH: &str = "O:/Archives/Eistein/FUI AGENT ROULLET/config/tuning.json";

/// Lê o ajuste salvo; qualquer arquivo corrompido ou fora de faixa volta ao padrão do §8
/// em vez de derrubar o boot com matcher inválido.
fn load_sequence_tuning(path: &str) -> MatcherTuning {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return MatcherTuning::default();
    };
    match serde_json::from_str::<MatcherTuning>(&raw) {
        Ok(t) => match t.validate() {
            Ok(()) => {
                println!("[TUNING] ajuste de sequências carregado de {path}");
                t
            }
            Err(err) => {
                eprintln!("[TUNING] {path} rejeitado ({err}) — voltando ao padrão");
                MatcherTuning::default()
            }
        },
        Err(err) => {
            eprintln!("[TUNING] {path} ilegível ({err}) — voltando ao padrão");
            MatcherTuning::default()
        }
    }
}

/// Persiste o ajuste aplicado. Falha de escrita é reportada mas não reverte a sessão:
/// o operador prefere o gatilho novo em memória a voltar ao antigo sem aviso.
fn save_sequence_tuning(path: &str, tuning: &MatcherTuning) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(tuning)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, body)
}

/// Snapshot do ajuste vigente (o guarda é `Copy`, então solta o lock imediatamente).
fn tuning_snapshot(shared: &Arc<Mutex<MatcherTuning>>) -> MatcherTuning {
    match shared.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Reconstrói os matchers a partir do ajuste vigente. Matcher nunca fica indisponível:
/// se o lock estiver envenenado, o guard é recuperado em vez de devolver lista vazia.
fn matchers_now(shared: &Arc<Mutex<MatcherTuning>>) -> Vec<Arc<dyn Matcher>> {
    match shared.lock() {
        Ok(g) => g.matchers(),
        Err(poisoned) => poisoned.into_inner().matchers(),
    }
}

/// Lê a requisição inteira (cabeçalho + corpo) respeitando `Content-Length`, para o
/// `POST /api/tuning` não perder o JSON quando ele não chega no primeiro pacote.
fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<(String, String)> {
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
            let len = head
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if raw.len() >= head_end + 4 + len {
                let body = String::from_utf8_lossy(&raw[head_end + 4..]).to_string();
                return Ok((head, body));
            }
        }
        if raw.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    match text.find("\r\n\r\n") {
        Some(i) => Ok((text[..i].to_string(), text[i + 4..].to_string())),
        None => Ok((text, String::new())),
    }
}

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
    tuning: Arc<Mutex<MatcherTuning>>,
    tuning_path: String,
) {
    thread::spawn(move || {
        let boot_instant = std::time::Instant::now();
        let Ok(listener) = TcpListener::bind("127.0.0.1:8791") else {
            eprintln!("[FUI-HTTP] porta 127.0.0.1:8791 já em uso");
            return;
        };
        println!("[FUI-HTTP] Painel de Operação vivo em http://127.0.0.1:8791");

        for stream_res in listener.incoming() {
            let Ok(mut stream) = stream_res else {
                continue;
            };
            // Timeout curto: um cliente que abre e não fala não pode travar o painel.
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let Ok((head, body)) = read_http_request(&mut stream) else {
                continue;
            };
            if head.is_empty() {
                continue;
            }
            let first_line = head.lines().next().unwrap_or("");

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
                let matchers = matchers_now(&tuning);
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
                let matchers = matchers_now(&tuning);
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
            } else if first_line.starts_with("GET /api/tuning") {
                let resp = serde_json::json!({
                    "current": tuning_snapshot(&tuning),
                    "defaults": MatcherTuning::default(),
                });
                let body = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
                let out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(out.as_bytes());
            } else if first_line.starts_with("POST /api/tuning") {
                let (status, payload) = match serde_json::from_str::<MatcherTuning>(&body) {
                    Ok(next) => match next.validate() {
                        Ok(()) => {
                            let mut guard = match tuning.lock() {
                                Ok(g) => g,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            *guard = next.clone();
                            drop(guard);
                            let persisted = match save_sequence_tuning(&tuning_path, &next) {
                                Ok(()) => serde_json::json!({"ok": true, "tuning": next}),
                                Err(err) => {
                                    eprintln!("[TUNING] falha ao persistir {tuning_path}: {err}");
                                    serde_json::json!({
                                        "ok": true,
                                        "tuning": next,
                                        "warning": format!("aplicado em memória, não salvo: {err}")
                                    })
                                }
                            };
                            println!(
                                "[TUNING] aplicado | coluna {}..={} | dúzia {}..={} | cor {}..={} | alto/baixo {}..={} | par/ímpar {}..={} | desligados={:?}",
                                next.sequences.coluna.min, next.sequences.coluna.max,
                                next.sequences.duzia.min, next.sequences.duzia.max,
                                next.sequences.cor.min, next.sequences.cor.max,
                                next.sequences.alto_baixo.min, next.sequences.alto_baixo.max,
                                next.sequences.par_impar.min, next.sequences.par_impar.max,
                                next.disabled
                            );
                            ("200 OK", persisted)
                        }
                        Err(err) => (
                            "400 Bad Request",
                            serde_json::json!({"ok": false, "error": err}),
                        ),
                    },
                    Err(err) => (
                        "400 Bad Request",
                        serde_json::json!({"ok": false, "error": format!("JSON inválido: {err}")}),
                    ),
                };
                let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                let out = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    json.len(),
                    json
                );
                let _ = stream.write_all(out.as_bytes());
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
    // Padrão definido pelo gate do §13 com banca ~R$ 80: base 20¢ e 3 degraus
    // (20/60/180, perda máxima 520¢) dão P(ruína)=0,00%; com 4 degraus sobe para 8,16% e reprova.
    let mut cfg = Cfg {
        base_cents: 20,
        max_steps: 3,
        max_bet_per_cover_cents: 100_000,
        max_exposure_pct: 0.10,
        stop_loss_cents: 1600,
        stop_win_pct: 0.08,
        min_window_ms: 2000,
        cooldown_spins: 2,
        max_entries_per_hour: 40,
        chip_denoms_cents: vec![2500, 1000, 500, 100, 50, 10],
    };

    // `--mc-only` imprime os dois Monte Carlo e sai: é como se testa uma mudança de
    // `base_cents`/`max_steps`/banca sem abrir sessão (gate do §13).
    let mut mc_only = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mc-only" => mc_only = true,
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

    // 1b. Mesmo gate para cobertura de números (cluster de terminais: k = 15 pockets).
    // A geometria é outra — `k·x` de exposição, escada própria — então o MC de colunas não cobre.
    let mc_numbers = run_monte_carlo_numbers(&NumbersMcParams {
        initial_bankroll_cents: bankroll_cents,
        numbers: 15,
        cfg: cfg.clone(),
        ..NumbersMcParams::default()
    });
    println!(
        "[MONSTER-CORE] MC números (k={} · {} degraus) | estouro/ciclo={:.2}% | P(ruína)={:.2}% | DD p95={}¢ | exp. máx={}¢ | EV/h={:.1}¢ | gate={}",
        mc_numbers.numbers,
        mc_numbers.max_steps,
        mc_numbers.p_blow_per_cycle * 100.0,
        mc_numbers.p_ruin * 100.0,
        mc_numbers.drawdown_p95_cents,
        mc_numbers.max_exposure_cents,
        mc_numbers.ev_per_hour_cents,
        if mc_numbers.gate_passes() { "PASS" } else { "FAIL" }
    );
    if mc_only {
        return Ok(());
    }

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

    let tuning = Arc::new(Mutex::new(load_sequence_tuning(TUNING_PATH)));
    let matchers = matchers_now(&tuning);
    let boot_tuning = tuning_snapshot(&tuning);
    println!(
        "[TUNING] sequências | coluna {}..={} | dúzia {}..={} | cor {}..={} | alto/baixo {}..={} | par/ímpar {}..={} | desligados={:?}",
        boot_tuning.sequences.coluna.min, boot_tuning.sequences.coluna.max,
        boot_tuning.sequences.duzia.min, boot_tuning.sequences.duzia.max,
        boot_tuning.sequences.cor.min, boot_tuning.sequences.cor.max,
        boot_tuning.sequences.alto_baixo.min, boot_tuning.sequences.alto_baixo.max,
        boot_tuning.sequences.par_impar.min, boot_tuning.sequences.par_impar.max,
        boot_tuning.disabled
    );
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
        tuning.clone(),
        TUNING_PATH.to_string(),
    );

    let mut spins_since_blow = cfg.cooldown_spins;
    // A escada vive em `ladder_state`, uma linha por estratégia (§5.3). Carrega o estado persistido
    // no boot para o reinício retomar o degrau real em vez de voltar ao degrau 1.
    let mut ladder_steps: LadderSteps = match all_ladder_steps(&pool) {
        Ok(rows) => rows.into_iter().collect(),
        Err(err) => {
            eprintln!("[BOOT] falha ao ler ladder_state ({err}) — escadas começam no degrau 1");
            LadderSteps::new()
        }
    };
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
        // Um evento duplicado da MESMA rodada (WS + fita) não pode mexer na escada: sem esta
        // guarda a perda era contada duas vezes e a entrada seguinte pulava um degrau.
        if already_recorded {
            if pending_entry.is_some() {
                println!(
                    "[DEDUPE] rodada repetida (num={}) — reconciliação da entrada pendente ignorada",
                    ev.number
                );
            }
        } else if let Some((prev_plan, prev_acks)) = pending_entry.take() {
            // Rodada resolvida: o overlay sai do pano junto com a entrada.
            fui.set_cover_numbers(Vec::new());
            match reconcile_spin(
                &pool,
                PlayedEntry {
                    strategy_id: &prev_plan.strategy_id,
                    ladder_step: prev_plan.ladder_step,
                    acks: &prev_acks,
                },
                ev.number,
                &cfg,
                None,
                now_ms,
            ) {
                Ok(rep) => {
                    if initial_table_balance.load(Ordering::Relaxed) <= 0 {
                        shared_bankroll_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                        shared_pnl_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                    }
                    ladder_steps.insert(rep.strategy_id.clone(), rep.next_step);
                    if rep.ladder_blown {
                        spins_since_blow = 0;
                    } else {
                        spins_since_blow = spins_since_blow.saturating_add(1);
                    }
                    println!(
                        "[RECONCILE] spin={} num={} | estrategia={} | degrau_jogado={} | won={} delta={}¢ | pnl={}¢ | proximo_degrau={}",
                        ev.spin_id,
                        ev.number,
                        rep.strategy_id,
                        prev_plan.ladder_step,
                        rep.won,
                        rep.expected_delta_cents,
                        shared_pnl_cents.load(Ordering::Relaxed),
                        rep.next_step
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
        // Janela: o giro é a autoridade (o texto da UI chega depois por polling). Replay e
        // reconexão entregam sempre giro velho, aí a janela é zero e o gate dá Skip.
        let window_open = table_round_open.load(Ordering::Relaxed);
        let spin_age_ms = now_ms.saturating_sub(ev.ts);
        let st = State {
            bankroll_cents: if cur_bank > 0 { cur_bank } else { bankroll_cents },
            exposure_cents: 0,
            session_pnl_cents: cur_pnl,
            remaining_window_ms: window_budget_ms(window_open, spin_age_ms),
            spins_since_blow,
            // Neutro: o DAG usa o degrau da própria estratégia vencedora (`ladder_steps`).
            step: 0,
        };

        // Ajuste de sequências pode mudar a qualquer momento pela dashboard: os matchers
        // são reconstruídos a cada giro para o gatilho novo valer no próximo sinal.
        let matchers = matchers_now(&tuning);
        let (dag_out, best_hist) = evaluate_dag_with_history(
            &ev.spin_id,
            &recent,
            &chrono_hist,
            &matchers,
            &cfg,
            &GateCtx {
                state: &st,
                ladder_steps: &ladder_steps,
            },
            0.55,
        );
        println!(
            "[DAG] spin={} num={} | regime={:?} | p_troca={:.2} bloco={} | sinais={} | verdict={:?} | hist={} | {}µs",
            ev.spin_id,
            ev.number,
            dag_out.regime,
            dag_out.bocpd.p_change,
            dag_out.bocpd.map_run_length,
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
                    // Overlay do pano por número: o cluster chega aqui já expandido em plenos.
                    let mut covered: Vec<u8> = plan
                        .covers
                        .iter()
                        .flat_map(|(cover, _)| cover.numbers())
                        .collect();
                    covered.sort_unstable();
                    covered.dedup();
                    fui.set_cover_numbers(covered);
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
                        ladder_step: ladder_steps.get(&pre.strategy_id).copied().unwrap_or(0),
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
