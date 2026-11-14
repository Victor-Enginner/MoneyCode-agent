// SPDX-License-Identifier: Apache-2.0
//! Binário principal do orquestrador `monster-core` (DAG + Risco + Executor + Reconciliador + FUI Server).
//! Sincroniza 100% ao vivo o Saldo real (`Balance`), P&L real, Aposta na mesa (`Bet`) e a fita de 12 números da Playtech,
//! garantindo coerência total entre Colunas (`2to1`) e Dúzias (`1st/2nd/3rd 12`) na FUI (`http://127.0.0.1:8791`).

use monster_capture::{CaptureSource, SpinBus, StreamClient, DEFAULT_STREAM_ADDR};
use monster_core::balance_audit::{BalanceWatch, WatchEvent};
use monster_core::caution::{Caution, CautionChange};
use monster_core::shadow::{ShadowRunner, ShadowSettle, ShadowStats, SHADOW_CARDS};
use monster_core::{
    boot_reconcile, build_brain_snapshot, evaluate_all_matchers_history,
    cfg_for_strategy, ensure_session, evaluate_dag_with_history, has_recent_number, load_chronological_history,
    load_session_chronological, place, read_session_newest_until, reconcile_spin,
    session_prev_spin_ts,
    spins_after_trigger, SessionDecision, MISSED_SPIN_GAP_MS,
    family_radars, sync_strip_to_db, terminal_radar, write_gbrain_file, Calibration, CdpActuator,
    CoverAck, DryRunActuator, OPERATOR_TERMINALS,
    window_budget_ms, ExecutionMode, GateCtx, HistoryBuffer, KillSwitch, LadderSteps,
    LiveHttpActuator, Plan,
    PlayedEntry, Window30, HISTORY_CAPACITY, WINDOW_CAPACITY,
};
use monster_risk::{
    cum_loss_cents, run_monte_carlo, run_monte_carlo_numbers, stake_cents, Cfg, McParams,
    NumbersMcParams, State, Verdict, DEFAULT_CHIP_DENOMS_CENTS,
};
use monster_store::{
    all_ladder_steps, init_pool, insert_error, upsert_ladder_state, insert_intent, insert_spin, IntentDecision, NewIntent, NewSpin,
    SpinSource, SqlitePool,
};
use monster_strategy_native::{Matcher, MatcherTuning, MATCHER_IDS};
use monster_ui::{DivergenceDto, FuiController, SettlementDto, ShadowDto};
use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LIVE_FUI_HTML: &str = include_str!("../../../ui/live.html");

/// Ajuste das sequências editado na dashboard: vive em disco para sobreviver ao restart.
/// Raiz do projeto: `MONSTER_ROOT` se definido, senão a pasta atual (os scripts entram nela).
/// Nada de caminho fixo de máquina: o projeto roda de qualquer pasta, em qualquer PC.
fn monster_root() -> std::path::PathBuf {
    env::var("MONSTER_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

/// Caminho de um arquivo do projeto como `String` (relativo à raiz).
fn project_path(rel: &str) -> String {
    monster_root().join(rel).to_string_lossy().into_owned()
}

fn tuning_path() -> String {
    project_path("config/tuning.json")
}

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

/// Ficha de entrada escolhida na dashboard: o valor sobrevive ao restart.
fn stake_path() -> String {
    project_path("config/stake.json")
}

/// Fichas que o operador pode escolher na FUI (centavos). Fora desta lista é recusado:
/// cada valor muda a escada e precisa passar pelo Monte Carlo do §13.
const ALLOWED_BASES: [u64; 5] = [10, 20, 40, 50, 100];

/// Janela mínima entre entradas, mesmo sem Game Code (WS + fita duplicando o mesmo giro).
const MIN_ENTRY_GAP_MS: i64 = 15_000;
/// Uma rodada da Playtech dura ~35 s: Game Code igual depois disso é texto velho no DOM,
/// não a mesma rodada. Sem esse teto o bot ignorou sinais por 33 min (13/11, código preso).
const ROUND_KEY_TTL_MS: i64 = 45_000;

/// Escadas com degrau pendente que NÃO acabaram de perder neste giro: devem ser zeradas.
/// Histórico das estratégias em observação (uma linha JSON por ciclo encerrado).
fn shadow_log_path() -> String {
    project_path("logs/shadow.jsonl")
}

/// Loga e grava um ciclo simulado encerrado. Nunca toca na mesa.
fn record_shadow(s: &ShadowSettle, now_ms: i64) {
    println!(
        "[SOMBRA] {} | {} no degrau {} (saiu {}){} | {}{:.2} (simulado, sem aposta)",
        s.nome,
        if s.won { "GANHOU" } else { "ESTOUROU" },
        s.step,
        s.number,
        if s.blocked_by_exposure { " | parou na trava de exposição" } else { "" },
        if s.net_cents >= 0 { "+R$ " } else { "-R$ " },
        s.net_cents.unsigned_abs() as f64 / 100.0
    );
    let line = serde_json::json!({
        "ts": now_ms, "id": s.id, "won": s.won, "step": s.step,
        "net_cents": s.net_cents, "number": s.number, "blocked": s.blocked_by_exposure,
    });
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(shadow_log_path())
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(err) = res {
        eprintln!("[SOMBRA-ERR] falha ao gravar logs/shadow.jsonl: {err}");
    }
}

/// Recarrega o placar acumulado das observações anteriores.
fn restore_shadow_stats(shadows: &mut [ShadowRunner]) {
    let Ok(text) = std::fs::read_to_string(shadow_log_path()) else {
        return;
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default();
        let Some(r) = shadows.iter_mut().find(|r| r.card.id == id) else {
            continue;
        };
        r.stats.add(&ShadowSettle {
            id: r.card.id,
            nome: r.card.nome,
            won: v.get("won").and_then(|x| x.as_bool()).unwrap_or(false),
            step: v.get("step").and_then(|x| x.as_u64()).and_then(|x| u8::try_from(x).ok()).unwrap_or(1),
            net_cents: v.get("net_cents").and_then(|x| x.as_i64()).unwrap_or(0),
            number: v.get("number").and_then(|x| x.as_u64()).and_then(|x| u8::try_from(x).ok()).unwrap_or(0),
            blocked_by_exposure: v.get("blocked").and_then(|x| x.as_bool()).unwrap_or(false),
        });
    }
    for r in shadows.iter() {
        let ShadowStats { cycles, wins, blows, net_cents, .. } = r.stats.clone();
        println!(
            "[SOMBRA] {} em observação | histórico: {cycles} ciclos, {wins} ganhos, {blows} estouros, saldo simulado {:.2}",
            r.card.nome,
            net_cents as f64 / 100.0
        );
    }
}

fn shadow_dtos(shadows: &[ShadowRunner]) -> Vec<ShadowDto> {
    shadows
        .iter()
        .map(|r| {
            let (step, chip) = r.open();
            ShadowDto {
                id: r.card.id.to_string(),
                nome: r.card.nome.to_string(),
                step,
                chip_cents: chip,
                cycles: r.stats.cycles,
                wins: r.stats.wins,
                blows: r.stats.blows,
                net_cents: r.stats.net_cents,
                worst_cents: r.stats.worst_cents,
            }
        })
        .collect()
}

/// Publica mudança do modo cautela no log e no painel (o Telegram lê do painel).
fn report_caution(fui: &FuiController, caution: &Caution, change: CautionChange) {
    match change {
        CautionChange::None => {}
        CautionChange::On(reason) => {
            println!("[CAUTELA] LIGADO — {reason} | só coluna e dúzia até 2 acertos de primeira ou 20 giros calmos");
            fui.sync_caution(Some(caution.reason().to_string()));
        }
        CautionChange::Off(why) => {
            println!("[CAUTELA] DESLIGADO — {why} | todas as estratégias liberadas");
            fui.sync_caution(None);
        }
    }
}

/// Como a entrada pendente foi resolvida na mesa.
enum EntryOutcome {
    /// Giro seguinte chegou no tempo normal: o número do evento é o resultado.
    Normal,
    /// Giro(s) perdido(s): `result` veio da fita; `missed` (mais recente primeiro) faltava no banco.
    Missed { result: u8, missed: Vec<u8> },
    /// Giro perdido e a fita não permite saber o resultado.
    Unknown,
}

/// Decide o resultado real da entrada. Com intervalo normal vale o evento; acima de
/// `MISSED_SPIN_GAP_MS` a fita da mesa diz o que saiu logo depois do giro da entrada.
fn resolve_entry_outcome(
    pool: &SqlitePool,
    latest_strip: &Mutex<Vec<u8>>,
    ev_number: u8,
    ev_ts: i64,
    trigger_ts: i64,
) -> EntryOutcome {
    if trigger_ts <= 0 || ev_ts - trigger_ts <= MISSED_SPIN_GAP_MS {
        return EntryOutcome::Normal;
    }
    let strip = latest_strip.lock().map(|s| s.clone()).unwrap_or_default();
    let before = read_session_newest_until(pool, trigger_ts, 12).unwrap_or_default();
    let Some(after) = spins_after_trigger(&strip, &before) else {
        return EntryOutcome::Unknown;
    };
    let Some(&result) = after.last() else {
        return EntryOutcome::Unknown;
    };
    // Fita já atualizada mostra o giro do evento no topo: ele não é giro perdido.
    let missed: Vec<u8> = if after.first() == Some(&ev_number) {
        after[1..].to_vec()
    } else {
        after
    };
    if missed.is_empty() {
        // Rodada só demorou (troca de dealer): fita confirma que nada se perdeu.
        return EntryOutcome::Normal;
    }
    EntryOutcome::Missed { result, missed }
}

/// Grava na TIMELINE os giros que o observer perdeu, entre o giro da entrada e o atual.
fn record_missed_spins(pool: &SqlitePool, missed_newest_first: &[u8], trigger_ts: i64, ev_ts: i64) {
    let step = (ev_ts - trigger_ts) / (missed_newest_first.len() as i64 + 1);
    for (j, &number) in missed_newest_first.iter().rev().enumerate() {
        let ts = trigger_ts + step * (j as i64 + 1);
        let _ = insert_spin(
            pool,
            &NewSpin {
                spin_id: format!("gap-{ev_ts}-{j}-{number}"),
                number,
                ts,
                source: SpinSource::Mo,
                received_at: now_unix_ms(),
                latency_ms: Some(0),
            },
        );
    }
}

/// Aplica o resultado da conferência de extrato e devolve a base de P&L (talvez ajustada).
/// Divergência avisa (log, `errors`, painel, Telegram) e NÃO pausa: o operador decide.
/// Movimento externo (saque, depósito) sai da base para não virar lucro nem prejuízo do bot.
fn handle_watch_event(
    event: WatchEvent,
    base_bal: i64,
    init_bal_atom: &AtomicI64,
    pool: &SqlitePool,
    fui: &FuiController,
) -> i64 {
    match event {
        WatchEvent::None => base_bal,
        WatchEvent::AuditOk { audit, observed_total_cents } => {
            println!(
                "[EXTRATO-OK] {} num={} | delta={}¢ | saldo esperado={}¢ lido={}¢",
                audit.strategy_id, audit.number, audit.delta_cents, audit.expected_total_cents, observed_total_cents
            );
            base_bal
        }
        WatchEvent::Divergent { audit, observed_total_cents } => {
            let diff = observed_total_cents - audit.expected_total_cents;
            let now = now_unix_ms();
            eprintln!(
                "[EXTRATO-DIVERGENTE] {} num={} | delta calculado={}¢ | saldo esperado={}¢ lido={}¢ | diferença={}¢",
                audit.strategy_id, audit.number, audit.delta_cents, audit.expected_total_cents, observed_total_cents, diff
            );
            let _ = insert_error(
                pool,
                "extrato_divergence",
                &format!(
                    "strategy={} number={} expected={} observed={} diff={}",
                    audit.strategy_id, audit.number, audit.expected_total_cents, observed_total_cents, diff
                ),
                now,
            );
            fui.sync_balance_divergence(DivergenceDto {
                strategy_id: audit.strategy_id,
                number: audit.number,
                expected_cents: audit.expected_total_cents,
                observed_cents: observed_total_cents,
                diff_cents: diff,
                ts: now,
            });
            base_bal
        }
        WatchEvent::External { delta_cents } => {
            println!(
                "[MOVIMENTO-EXTERNO] saldo mudou {}¢ sem entrada do bot (saque, depósito ou ficha manual) — fora do P&L",
                delta_cents
            );
            let new_base = base_bal + delta_cents;
            init_bal_atom.store(new_base, Ordering::Relaxed);
            new_base
        }
    }
}

fn stale_ladders(steps: &LadderSteps, gale_alive: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = steps
        .iter()
        .filter(|(sid, &step)| step > 0 && Some(sid.as_str()) != gale_alive)
        .map(|(sid, _)| sid.clone())
        .collect();
    out.sort();
    out
}

/// Regra "uma entrada por rodada" tolerante a Game Code congelado no DOM.
fn is_same_round(round_key: &str, last_round: &str, last_entry_at_ms: i64, now_ms: i64) -> bool {
    if last_entry_at_ms <= 0 {
        return false;
    }
    let since = now_ms - last_entry_at_ms;
    since < MIN_ENTRY_GAP_MS || (!round_key.is_empty() && last_round == round_key && since < ROUND_KEY_TTL_MS)
}


/// Lê a ficha salva; valor inválido ou não permitido volta ao padrão do `Cfg`.
fn load_stake_base(path: &str, fallback: u64) -> u64 {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return fallback;
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => match v.get("base_cents").and_then(|b| b.as_u64()) {
            Some(base) if ALLOWED_BASES.contains(&base) => base,
            _ => fallback,
        },
        Err(_) => fallback,
    }
}

/// Persiste a ficha escolhida para o próximo boot.
fn save_stake_base(path: &str, base_cents: u64) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Preserva a ficha de números: a dashboard só muda a ficha de fora.
    let number_base = load_number_base(path, 10);
    std::fs::write(
        path,
        format!("{{\n  \"base_cents\": {base_cents},\n  \"number_base_cents\": {number_base}\n}}\n"),
    )
}

/// Lê o lucro alvo da escada de números (`number_base_cents`); ausente ou inválido = fallback.
fn load_number_base(path: &str, fallback: u64) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("number_base_cents").and_then(serde_json::Value::as_u64))
        .filter(|b| ALLOWED_BASES.contains(b))
        .unwrap_or(fallback)
}

/// Copia a config vigente: a ficha pode mudar em runtime, então nada guarda `Cfg` fixo.
fn cfg_snapshot(shared: &Arc<Mutex<Cfg>>) -> Cfg {
    match shared.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Escada de uma ficha, para a dashboard mostrar o custo antes de aplicar.
fn ladder_rows(base_cents: u64, max_steps: u8) -> Vec<serde_json::Value> {
    (1..=max_steps)
        .map(|step| {
            let stake = stake_cents(step, base_cents);
            serde_json::json!({
                "step": step,
                "stake_per_cover_cents": stake,
                "exposure_cents": stake * 2,
                "cum_loss_cents": cum_loss_cents(step, base_cents),
            })
        })
        .collect()
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
    // Walk-forward e regime: histórico longo da mesa (estatística vale entre sessões).
    let chrono = load_chronological_history(pool, HISTORY_CAPACITY);
    if let Ok(mut h) = history.lock() {
        let mut fresh = HistoryBuffer::new();
        for &n in &chrono {
            fresh.push_live(n);
        }
        *h = fresh;
    }
    // Padrões de entrada: só a sessão atual. Número de outra sessão nunca fecha streak.
    let session = load_session_chronological(pool, WINDOW_CAPACITY);
    if let Ok(mut w) = window.lock() {
        let mut fresh = Window30::new();
        for &n in &session {
            fresh.push(n);
        }
        *w = fresh;
    }
}

/// Dependências do servidor HTTP da FUI, agrupadas: são oito e a ordem posicional
/// não diz nada na chamada.
struct FuiServerDeps {
    pool: SqlitePool,
    fui: FuiController,
    kill_switch: KillSwitch,
    history: Arc<Mutex<HistoryBuffer>>,
    live_cfg: Arc<Mutex<Cfg>>,
    bankroll: Arc<AtomicI64>,
    tuning: Arc<Mutex<MatcherTuning>>,
    tuning_path: String,
}

fn spawn_fui_http_server(deps: FuiServerDeps) {
    thread::spawn(move || {
        let FuiServerDeps {
            pool,
            fui,
            kill_switch,
            history,
            live_cfg,
            bankroll,
            tuning,
            tuning_path,
        } = deps;
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
            } else if first_line.starts_with("GET /api/lab") {
                let matchers = matchers_now(&tuning);
                let body = match monster_core::lab::load_clean_spins(&pool) {
                    Ok((raw, spins)) => serde_json::to_string(&monster_core::lab::run_lab(
                        raw,
                        &spins,
                        &monster_core::lab::default_hypotheses(&matchers),
                    ))
                    .unwrap_or_else(|_| "{}".to_string()),
                    Err(err) => format!("{{\"error\":\"{err}\"}}"),
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                continue;
            } else if first_line.starts_with("GET /api/patterns") {
                let chrono = history
                    .lock()
                    .map(|h| h.to_chronological_vec())
                    .unwrap_or_default();
                let matchers = matchers_now(&tuning);
                let cfg = cfg_snapshot(&live_cfg);
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
                let cfg = cfg_snapshot(&live_cfg);
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
                    // Allowlist do interruptor: o Telegram e a dashboard editam
                    // pelos mesmos ids, então o nome mora só no strategy-native.
                    "matchers": MATCHER_IDS,
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
            } else if first_line.starts_with("GET /api/base") {
                let cfg = cfg_snapshot(&live_cfg);
                let payload = serde_json::json!({
                    "base_cents": cfg.base_cents,
                    "max_steps": cfg.max_steps,
                    "allowed": ALLOWED_BASES,
                    "bank_cents": bankroll.load(Ordering::Relaxed),
                    "ladder": ladder_rows(cfg.base_cents, cfg.max_steps),
                });
                let body = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                let out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(out.as_bytes());
            } else if first_line.starts_with("POST /api/base") {
                let pedido = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("base_cents").and_then(|b| b.as_u64()));
                let (status, payload) = match pedido {
                    Some(base) if ALLOWED_BASES.contains(&base) => {
                        let current = cfg_snapshot(&live_cfg);
                        let bank = bankroll.load(Ordering::Relaxed).max(1);
                        let mut candidate = current.clone();
                        candidate.base_cents = base;
                        // §13: ficha nova só entra depois do Monte Carlo com a banca de agora.
                        let mc = run_monte_carlo(&McParams {
                            initial_bankroll_cents: bank,
                            cfg: candidate,
                            ..McParams::default()
                        });
                        if mc.p_ruin >= 0.05 {
                            (
                                "409 Conflict",
                                serde_json::json!({
                                    "ok": false,
                                    "p_ruin": mc.p_ruin,
                                    "error": format!(
                                        "Monte Carlo reprovou {}¢ com banca de {}¢: P(ruína)={:.2}% (limite 5%)",
                                        base, bank, mc.p_ruin * 100.0
                                    ),
                                }),
                            )
                        } else {
                            if let Ok(mut g) = live_cfg.lock() {
                                g.base_cents = base;
                            }
                            let _ = fui.set_base(base);
                            if let Err(err) = save_stake_base(&stake_path(), base) {
                                eprintln!("[STAKE] aplicada em memória, não salva: {err}");
                            }
                            println!(
                                "[STAKE] ficha {}¢ -> {}¢ | P(ruína)={:.2}% | DD p95={}¢",
                                current.base_cents,
                                base,
                                mc.p_ruin * 100.0,
                                mc.drawdown_p95_cents
                            );
                            (
                                "200 OK",
                                serde_json::json!({
                                    "ok": true,
                                    "base_cents": base,
                                    "p_ruin": mc.p_ruin,
                                    "drawdown_p95_cents": mc.drawdown_p95_cents,
                                    "max_steps": current.max_steps,
                                    "ladder": ladder_rows(base, current.max_steps),
                                }),
                            )
                        }
                    }
                    _ => (
                        "400 Bad Request",
                        serde_json::json!({
                            "ok": false,
                            "error": format!("ficha inválida; escolha entre {:?}", ALLOWED_BASES),
                        }),
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
                let disk_html = std::fs::read(project_path("ui/live.html"))
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
    let mut db_uri = project_path("monster.db");
    let mut bankroll_cents: i64 = 4_000;
    // Inicio padrao R$ 40: coluna 20¢ e 5 degraus ×3 (20/60/180/540/1620 por coluna); numeros com lucro alvo 10¢.
    // Perda máxima de um estouro = 2420¢. Exposição do degrau 5 = 1620¢, que é ~51% da banca
    // depois de 4 perdas (3200¢) — por isso o teto de exposição sobe para 0,55.
    // Stop loss desligado a pedido do operador: o limite é a própria banca (4000¢).
    let mut cfg = Cfg {
        base_cents: 20,
        number_base_cents: 10,
        max_steps: 5,
        max_bet_per_cover_cents: 100_000,
        max_exposure_pct: 0.55,
        stop_loss_cents: 4000,
        stop_win_pct: 0.08,
        min_window_ms: 2000,
        cooldown_spins: 2,
        max_entries_per_hour: 40,
        chip_denoms_cents: DEFAULT_CHIP_DENOMS_CENTS.to_vec(),
    };

    // `--mc-only` imprime os dois Monte Carlo e sai: é como se testa uma mudança de
    // `base_cents`/`max_steps`/banca sem abrir sessão (gate do §13).
    let mut mc_only = false;
    // `--lab` roda o laboratório de hipóteses sobre o banco e sai (não toca na mesa nem reconcilia).
    let mut lab_only = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mc-only" => mc_only = true,
            "--lab" => lab_only = true,
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

    // Ficha salva pela dashboard vence o padrão do binário; o MC abaixo já roda com ela.
    cfg.base_cents = load_stake_base(&stake_path(), cfg.base_cents);
    cfg.number_base_cents = load_number_base(&stake_path(), cfg.number_base_cents);
    println!(
        "[STAKE] ficha de fora: {}¢ por cobertura · números: lucro alvo {}¢/ciclo, máx {} casas",
        cfg.base_cents,
        cfg.number_base_cents,
        monster_risk::MAX_NUMBER_COVERS
    );

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
    if lab_only {
        let pool = init_pool(&db_uri)?;
        let tuning = Arc::new(Mutex::new(load_sequence_tuning(&tuning_path())));
        let matchers = matchers_now(&tuning);
        let (raw, spins) = monster_core::lab::load_clean_spins(&pool)?;
        let r = monster_core::lab::run_lab(raw, &spins, &monster_core::lab::default_hypotheses(&matchers));
        println!(
            "[LAB] giros brutos={} limpos={} | qui2 37 casas={:.1} (df=36, desvio a 5% acima de 51,0) | recomendado {}",
            r.giros_brutos, r.giros_limpos, r.qui2_casas, r.giros_recomendados
        );
        println!("{:<30} {:>6} {:>7} {:>7} {:>6} {:>8} {:>7}  VEREDITO", "HIPOTESE", "APOST", "ACERTO", "ACASO", "Z", "EV", "P");
        for l in &r.linhas {
            println!(
                "{:<30} {:>6} {:>6.1}% {:>6.1}% {:>6.2} {:>7.1}% {:>7.3}  {}",
                l.id, l.teste.bets, l.teste.hit_pct, l.teste.chance_pct, l.teste.z, l.teste.ev_pct, l.p_teste, l.veredito
            );
        }
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

    let tuning = Arc::new(Mutex::new(load_sequence_tuning(&tuning_path())));
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
    // A ficha pode ser trocada pela dashboard em runtime: o DAG e o executor leem daqui
    // a cada giro, nunca de uma cópia presa no boot.
    let live_cfg = Arc::new(Mutex::new(cfg.clone()));
    let boot_instant = std::time::Instant::now();

    // Histórico inicial exclusivamente do banco (sincronizado com a fita da mesa): nada de números inventados.
    let initial_chrono = load_chronological_history(&pool, HISTORY_CAPACITY);
    let mut initial_hist = HistoryBuffer::new();
    for &n in &initial_chrono {
        initial_hist.push_live(n);
    }
    // Janela de padrões começa vazia: a sessão (continua ou nova) é decidida na primeira
    // fita lida da mesa, e só então a janela é carregada.
    let window = Arc::new(Mutex::new(Window30::new()));
    let boot_ms = now_unix_ms();
    let session_ready = Arc::new(AtomicBool::new(false));
    // Última fita lida da mesa: autoridade para achar o resultado real quando um giro se perde.
    let latest_strip: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let init_chrono = initial_hist.to_chronological_vec();
    let init_snap = build_brain_snapshot(&matchers, &init_chrono, &cfg);
    let _ = write_gbrain_file(
        std::path::Path::new(&project_path("docs/memory/BRAIN_STATE.md")),
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
    // Conferência de extrato pós-resultado (§9): saldo real contra o delta do reconciliador.
    let balance_watch = Arc::new(Mutex::new(BalanceWatch::default()));
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
        let watch_poll = balance_watch.clone();
        let session_ready_poll = session_ready.clone();
        let strip_poll = latest_strip.clone();

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
                    let total = real_bal + i64::try_from(tel.preview_cents).unwrap_or(0);
                    let event = watch_poll
                        .lock()
                        .map(|mut w| w.observe(total, now_unix_ms()))
                        .unwrap_or(WatchEvent::None);
                    let base_bal = handle_watch_event(event, base_bal, &init_bal_atom, &pool_poll, &fui_poll);
                    bank_atom.store(real_bal, Ordering::Relaxed);
                    pnl_atom.store(real_bal - base_bal, Ordering::Relaxed);
                }

                if tel.strip.len() >= 4 {
                    if let Ok(mut s) = strip_poll.lock() {
                        s.clone_from(&tel.strip);
                    }
                }

                // Primeira fita após o boot decide a sessão: continua (troca rápida) ou abre outra
                // semeada com a fita da mesa. Até decidir, o executor não entra.
                if !session_ready_poll.load(Ordering::Relaxed) {
                    match ensure_session(&pool_poll, &tel.strip, boot_ms, now_unix_ms()) {
                        Ok(Some(SessionDecision::Continue)) => {
                            println!("[SESSAO] fita da mesa continua o histórico — sessão anterior mantida");
                            reload_live_history(&pool_poll, &win_poll, &hist_poll);
                            session_ready_poll.store(true, Ordering::Relaxed);
                        }
                        Ok(Some(SessionDecision::Open { started_at, seeded, reason })) => {
                            println!(
                                "[SESSAO] nova sessão ({reason}) às {started_at} | fita da mesa {:?} vira a base ({seeded} giro(s)) | histórico anterior fora da TIMELINE e dos padrões",
                                tel.strip
                            );
                            reload_live_history(&pool_poll, &win_poll, &hist_poll);
                            session_ready_poll.store(true, Ordering::Relaxed);
                        }
                        Ok(None) => {}
                        Err(err) => eprintln!("[SESSAO-ERR] falha ao decidir sessão: {err}"),
                    }
                }

                // A fita da Playtech é a autoridade da TIMELINE: grava só os giros ausentes e recarrega os buffers.
                if session_ready_poll.load(Ordering::Relaxed) && tel.strip.len() >= 4 {
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

    spawn_fui_http_server(FuiServerDeps {
        pool: pool.clone(),
        fui: fui.clone(),
        kill_switch: kill_switch.clone(),
        history: history.clone(),
        live_cfg: live_cfg.clone(),
        bankroll: shared_bankroll_cents.clone(),
        tuning: tuning.clone(),
        tuning_path: tuning_path(),
    });

    let mut spins_since_blow = cfg.cooldown_spins;
    // Freio de risco: com gale recorrente, degrau 4 ou sequência longa, só coluna e dúzia.
    let mut caution = Caution::default();
    // Scripts do operador em observação: simulam a escada sem apostar (logs/shadow.jsonl).
    let mut shadows: Vec<ShadowRunner> = SHADOW_CARDS.iter().map(|c| ShadowRunner::new(*c)).collect();
    restore_shadow_stats(&mut shadows);
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
    // `ts` do giro em que a entrada pendente foi feita: mede se algum giro se perdeu até o resultado.
    let mut pending_trigger_ts: i64 = 0;
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
        // Config vigente do giro: ficha e degraus podem ter mudado pela dashboard.
        let cfg_now = cfg_snapshot(&live_cfg);

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

        // Buraco na sequência (observer reconectou): antes de qualquer análise, a fita da mesa
        // completa os giros perdidos. Sem isso, 35-14-[12 perdido]-8 virou "3x coluna 2" em
        // 14/11 e o bot entrou duas rodadas cedo. Sem confirmação da fita, este giro não entra.
        let mut window_uncertain = false;
        if !already_recorded && session_ready.load(Ordering::Relaxed) {
            if let Ok(Some(prev_ts)) = session_prev_spin_ts(&pool, ev.ts) {
                match resolve_entry_outcome(&pool, &latest_strip, ev.number, ev.ts, prev_ts) {
                    EntryOutcome::Normal => {}
                    EntryOutcome::Missed { missed, .. } => {
                        record_missed_spins(&pool, &missed, prev_ts, ev.ts);
                        println!(
                            "[GIRO-PERDIDO] fita da mesa mostra {:?} antes do {} — gravado(s) antes de analisar padrões",
                            missed, ev.number
                        );
                    }
                    EntryOutcome::Unknown => {
                        window_uncertain = true;
                        eprintln!(
                            "[GIRO-PERDIDO] {} s sem giro antes do {} e a fita não confirma a sequência — sem entrada neste giro",
                            (ev.ts - prev_ts) / 1000,
                            ev.number
                        );
                    }
                }
            }
        }

        // Reconcilia entrada pendente do spin anterior, se houver (§9).
        // Um evento duplicado da MESMA rodada (WS + fita) não pode mexer na escada: sem esta
        // guarda a perda era contada duas vezes e a entrada seguinte pulava um degrau.
        // Estratégia que acabou de perder NESTE giro: é a única com direito a gale agora.
        let mut gale_alive: Option<String> = None;
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
            let outcome = resolve_entry_outcome(&pool, &latest_strip, ev.number, ev.ts, pending_trigger_ts);
            let result_number = match &outcome {
                EntryOutcome::Normal => Some(ev.number),
                EntryOutcome::Missed { result, missed } => {
                    record_missed_spins(&pool, missed, pending_trigger_ts, ev.ts);
                    println!(
                        "[GIRO-PERDIDO] {} giro(s) perdido(s) entre a entrada e o {} — resultado real da entrada={} (fita da mesa) | gale interrompido",
                        missed.len(),
                        ev.number,
                        result
                    );
                    Some(*result)
                }
                EntryOutcome::Unknown => {
                    eprintln!(
                        "[GIRO-PERDIDO] {} s sem giro e a fita não mostra o resultado de {} — entrada não liquidada por número; escada zerada, saldo real segue no P&L",
                        (ev.ts - pending_trigger_ts) / 1000,
                        prev_plan.strategy_id
                    );
                    let _ = insert_error(
                        &pool,
                        "missed_spin_unknown",
                        &format!("strategy={} trigger_ts={} next={}", prev_plan.strategy_id, pending_trigger_ts, ev.number),
                        now_ms,
                    );
                    None
                }
            };
            let gale_ok = matches!(outcome, EntryOutcome::Normal);
            if let Some(result_number) = result_number {
            match reconcile_spin(
                &pool,
                PlayedEntry {
                    strategy_id: &prev_plan.strategy_id,
                    ladder_step: prev_plan.ladder_step,
                    acks: &prev_acks,
                },
                result_number,
                &cfg_for_strategy(&cfg_now, &prev_plan.strategy_id),
                None,
                now_ms,
            ) {
                Ok(rep) => {
                    if initial_table_balance.load(Ordering::Relaxed) <= 0 {
                        shared_bankroll_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                        shared_pnl_cents.fetch_add(rep.expected_delta_cents, Ordering::Relaxed);
                    }
                    ladder_steps.insert(rep.strategy_id.clone(), rep.next_step);
                    let audit_open = balance_watch
                        .lock()
                        .map(|mut w| w.settle(rep.expected_delta_cents, &rep.strategy_id, result_number, now_ms))
                        .unwrap_or(false);
                    if !audit_open && mode == ExecutionMode::Live {
                        println!("[EXTRATO] sem saldo lido antes da entrada — conferência desta rodada não aberta");
                    }
                    // Gale só no giro seguinte à perda: com giro perdido esse giro já passou.
                    if rep.next_step > 0 && gale_ok {
                        gale_alive = Some(rep.strategy_id.clone());
                    }
                    let change = if rep.won || rep.ladder_blown {
                        caution.on_cycle_end(prev_plan.ladder_step, rep.won)
                    } else {
                        caution.on_ladder_step(rep.next_step.saturating_add(1))
                    };
                    report_caution(&fui, &caution, change);
                    if rep.ladder_blown {
                        spins_since_blow = 0;
                    } else {
                        spins_since_blow = spins_since_blow.saturating_add(1);
                    }
                    println!(
                        "[RECONCILE] spin={} num={} | estrategia={} | degrau_jogado={} | won={} delta={}¢ | pnl={}¢ | proximo_degrau={}",
                        ev.spin_id,
                        result_number,
                        rep.strategy_id,
                        prev_plan.ladder_step,
                        rep.won,
                        rep.expected_delta_cents,
                        shared_pnl_cents.load(Ordering::Relaxed),
                        rep.next_step
                    );
                    // Resultado vai no próximo tick: o Telegram confirma a operação com o
                    // delta do reconciliador, não com dedução de saldo.
                    let _ = fui.sync_settlement(SettlementDto {
                        strategy_id: rep.strategy_id.clone(),
                        won: rep.won,
                        delta_cents: rep.expected_delta_cents,
                        step: prev_plan.ladder_step,
                        number: result_number,
                        max_steps: cfg_now.max_steps,
                        ts: now_ms,
                    });
                }
                Err(err) => {
                    eprintln!("[RECONCILE-ERR] {err}");
                }
            }
            }
        } else {
            spins_since_blow = spins_since_blow.saturating_add(1);
        }

        // Gale só existe no giro seguinte à perda. Escada parada (padrão sumiu, entrada
        // bloqueada, kill switch) é abandonada: padrão novo sempre começa no degrau 1.
        // Sem isso, em 13/11 a perda de um padrão de 10 giros antes entrou como "4º gale"
        // (R$ 22,20) no primeiro giro de um padrão novo.
        if !already_recorded {
            for sid in stale_ladders(&ladder_steps, gale_alive.as_deref()) {
                // Gale interrompido = ciclo encerrado sem acerto.
                let lost_step = ladder_steps.get(&sid).copied().unwrap_or(1);
                let change = caution.on_cycle_end(lost_step.max(1), false);
                report_caution(&fui, &caution, change);
                ladder_steps.insert(sid.clone(), 0);
                match upsert_ladder_state(&pool, &sid, 0, 0, Some(now_ms)) {
                    Ok(()) => println!("[LADDER] escada de {sid} zerada: gale interrompido não passa para outro padrão"),
                    Err(err) => eprintln!("[LADDER-ERR] falha ao zerar {sid}: {err}"),
                }
            }
        }

        if window_uncertain {
            continue;
        }

        // Sessão ainda não decidida (fita da mesa não lida): o giro fica gravado, mas nenhum
        // padrão é avaliado sobre uma janela que pode misturar duas sessões.
        if !session_ready.load(Ordering::Relaxed) {
            println!(
                "[SESSAO] spin={} num={} gravado — aguardando a fita da mesa para decidir a sessão; sem entrada",
                ev.spin_id, ev.number
            );
            continue;
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
        let change = caution.on_spin(&recent);
        report_caution(&fui, &caution, change);
        if !already_recorded {
            let bank = u64::try_from(shared_bankroll_cents.load(Ordering::Relaxed)).unwrap_or(0);
            let cap = (bank as f64 * cfg_now.max_exposure_pct) as u64;
            for runner in &mut shadows {
                if let Some(s) = runner.on_spin(&recent, cap) {
                    record_shadow(&s, now_ms);
                }
            }
            fui.sync_shadow(shadow_dtos(&shadows));
        }

        // Placar ao vivo dos terminais 2-3-6-9: leitura pura da fita real (sem replay de
        // fixture). Serve para o operador comparar a percepção com a medição.
        // Radares do painel: só a sessão atual, como a TIMELINE.
        let session_hist = load_session_chronological(&pool, 100);
        fui.sync_terminal_radar(Some(terminal_radar(
            &session_hist,
            100,
            OPERATOR_TERMINALS,
            3,
            0.44,
        )));
        fui.sync_family_radars(family_radars(&session_hist, 100));

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
        // Modo cautela tira números e apostas 1:1 da disputa: coluna/dúzia seguem concorrendo.
        let matchers: Vec<Arc<dyn Matcher>> = matchers_now(&tuning)
            .into_iter()
            .filter(|m| caution.allows(m.id()))
            .collect();
        let (dag_out, best_hist) = evaluate_dag_with_history(
            &ev.spin_id,
            &recent,
            &chrono_hist,
            &matchers,
            &cfg_now,
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
            let same_round = is_same_round(&round_key, &last_entry_round, last_entry_at_ms, now_ms);
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
            // Só dinheiro real mexe no saldo da mesa: dry-run não abre conferência de extrato.
            if mode == ExecutionMode::Live {
                if let Ok(mut w) = balance_watch.lock() {
                    w.begin_entry();
                }
            }
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
                    pending_trigger_ts = ev.ts;
                }
                Err(err) => {
                    if let Ok(mut w) = balance_watch.lock() {
                        w.cancel_entry();
                    }
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

#[cfg(test)]
mod round_tests {
    use super::{is_same_round, stale_ladders, LadderSteps};

    #[test]
    fn gale_so_continua_para_quem_acabou_de_perder() {
        let mut steps = LadderSteps::new();
        steps.insert("quebra_permanencia_coluna".into(), 3);
        steps.insert("quebra_alto_baixo_6x".into(), 3);
        steps.insert("quebra_cor_6x".into(), 0);
        let stale = stale_ladders(&steps, Some("quebra_alto_baixo_6x"));
        assert_eq!(stale, vec!["quebra_permanencia_coluna".to_string()]);
    }

    #[test]
    fn sem_perda_neste_giro_toda_escada_pendente_zera() {
        let mut steps = LadderSteps::new();
        steps.insert("quebra_permanencia_coluna".into(), 3);
        steps.insert("quebra_cor_6x".into(), 4);
        assert_eq!(stale_ladders(&steps, None).len(), 2);
    }

    #[test]
    fn mesmo_codigo_dentro_da_rodada_bloqueia() {
        assert!(is_same_round("A", "A", 1_000, 31_000));
    }

    #[test]
    fn codigo_congelado_no_dom_nao_trava_para_sempre() {
        assert!(!is_same_round("3845027974", "3845027974", 1_000, 1_000 + 484_930));
    }

    #[test]
    fn intervalo_minimo_vale_sem_codigo() {
        assert!(is_same_round("", "", 1_000, 10_000));
        assert!(!is_same_round("", "", 1_000, 20_000));
    }

    #[test]
    fn sem_entrada_anterior_nunca_bloqueia() {
        assert!(!is_same_round("A", "A", 0, 5_000));
    }
}
