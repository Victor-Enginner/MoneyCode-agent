// SPDX-License-Identifier: Apache-2.0
//! Executor de apostas com verificação obrigatória de preview, idempotência e modo `--dry-run`.
//! Grava a intenção em `intents` ANTES de qualquer clique e aborta com `clear_bets()` se o preview divergir.
//! Suporta kill switch atômico (atalho global, Telegram `/kill`, UI) mantendo a captura viva.

use crate::dag::Plan;
use monster_risk::{decompose, DEFAULT_CHIP_DENOMS_CENTS};
use monster_store::{
    insert_error, insert_fill, insert_intent, update_intent_decision, IntentDecision, NewFill,
    NewIntent, SqlitePool, StoreError,
};
use monster_strategy_native::Cover;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tracing::{error, info, warn};

/// Modo de operação do executor (§1.5: `--dry-run` é o default obrigatório).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExecutionMode {
    /// Simula cliques e preview em memória sem enviar `confirm()` de dinheiro real.
    #[default]
    DryRun,
    /// Executa cliques reais na mesa após flag `--live` explícito.
    Live,
}

/// Kill switch global thread-safe (§1.6): corta o executor preservando a captura viva.
#[derive(Debug, Clone, Default)]
pub struct KillSwitch {
    tripped: Arc<AtomicBool>,
}

impl KillSwitch {
    /// Cria um novo `KillSwitch` armado (não acionado).
    pub fn new() -> Self {
        Self {
            tripped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Aciona o kill switch imediatamente (`/kill`, botão UI ou atalho global).
    pub fn trip(&self) {
        self.tripped.store(true, Ordering::SeqCst);
        warn!("KILL SWITCH ACIONADO — executor bloqueado, captura permanece ativa");
    }

    /// Rearma o executor (`/resume`).
    pub fn reset(&self) {
        self.tripped.store(false, Ordering::SeqCst);
        info!("kill switch rearmado via /resume");
    }

    /// Verifica se o kill switch está acionado.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

/// Erros determinísticos do executor (`thiserror`, zero `unwrap()`).
#[derive(Debug, Error)]
pub enum ExecError {
    /// Kill switch ativo no momento da tentativa de aposta.
    #[error("kill switch ativo: execução bloqueada")]
    KillSwitchActive,
    /// Spin já possui entrada registrada para esta estratégia (`UNIQUE(strategy_id, spin_id)`).
    #[error("entrada duplicada bloqueada por idempotência (strategy={strategy_id}, spin={spin_id})")]
    AlreadyEntered {
        /// Estratégia que tentou repetir a entrada.
        strategy_id: String,
        /// Spin repetido.
        spin_id: String,
    },
    /// Coordenada de cobertura ou ficha ausente na calibração.
    #[error("coordenada de calibração ausente para `{0}`")]
    MissingCalibration(String),
    /// Divergência entre o valor lido no bet slip e o total esperado (> 1 centavo).
    #[error("preview mismatch no bet slip: lido={preview} centavos, esperado={expected} centavos")]
    PreviewMismatch {
        /// Valor lido no preview da mesa em centavos.
        preview: u64,
        /// Valor total esperado pelo plano em centavos.
        expected: u64,
    },
    /// Falha de comunicação CDP durante clique ou leitura.
    #[error("falha de transporte CDP: {0}")]
    CdpTransport(String),
    /// Erro de persistência no SQLite WAL.
    #[error("erro de banco de dados no executor: {0}")]
    Store(#[from] StoreError),
}

/// Mapa de calibração de coordenadas de canvas (`config/calibration.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    /// Coordenadas `(x, y)` por chave canônica de cobertura (`col:1`, `dozen:2`, etc.).
    pub covers: HashMap<String, (i32, i32)>,
    /// Coordenadas `(x, y)` por denominação de ficha em centavos (`10`, `50`, `100`, `500`, `1000`, `2500`).
    pub chips: HashMap<u64, (i32, i32)>,
    /// Coordenada do botão `Confirm`.
    pub confirm_coords: (i32, i32),
    /// Coordenada do botão `Clear / Undo`.
    pub clear_coords: (i32, i32),
    /// Denominações disponíveis em ordem decrescente.
    pub denoms: Vec<u64>,
}

impl Default for Calibration {
    fn default() -> Self {
        let mut covers = HashMap::new();
        covers.insert("col:1".to_string(), (875, 760));
        covers.insert("col:2".to_string(), (875, 710));
        covers.insert("col:3".to_string(), (875, 660));
        covers.insert("col2to1:1".to_string(), (875, 760));
        covers.insert("col2to1:2".to_string(), (875, 710));
        covers.insert("col2to1:3".to_string(), (875, 660));
        covers.insert("dozen:1".to_string(), (520, 815));
        covers.insert("dozen:2".to_string(), (670, 815));
        covers.insert("dozen:3".to_string(), (815, 815));
        covers.insert("low".to_string(), (480, 855));
        covers.insert("even".to_string(), (555, 855));
        covers.insert("red".to_string(), (630, 855));
        covers.insert("black".to_string(), (705, 855));
        covers.insert("odd".to_string(), (780, 855));
        covers.insert("high".to_string(), (845, 855));

        let mut chips = HashMap::new();
        chips.insert(10, (955, 945));
        chips.insert(20, (1000, 945));
        chips.insert(50, (1045, 945));
        chips.insert(100, (1090, 945));
        chips.insert(500, (1135, 945));
        chips.insert(1000, (1180, 945));
        chips.insert(2000, (1205, 945));
        chips.insert(2500, (1225, 945));
        chips.insert(5000, (1250, 945));

        Self {
            covers,
            chips,
            confirm_coords: (1045, 940),
            clear_coords: (835, 940),
            denoms: DEFAULT_CHIP_DENOMS_CENTS.to_vec(),
        }
    }
}

impl Calibration {
    /// Obtém as coordenadas `(x, y)` de uma cobertura.
    pub fn coords(&self, cover: &Cover) -> Result<(i32, i32), ExecError> {
        let key = cover.canonical_key();
        self.covers
            .get(&key)
            .copied()
            .ok_or(ExecError::MissingCalibration(key))
    }

    /// Obtém as coordenadas `(x, y)` de uma ficha em centavos.
    pub fn chip_coords(&self, denom_cents: u64) -> Result<(i32, i32), ExecError> {
        self.chips
            .get(&denom_cents)
            .copied()
            .ok_or_else(|| ExecError::MissingCalibration(format!("chip:{denom_cents}")))
    }
}

/// Ack individual de uma cobertura após `confirm()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverAck {
    /// Cobertura apostada.
    pub cover: Cover,
    /// Valor apostado na cobertura em centavos inteiros.
    pub stake_cents: u64,
    /// Indica se a cobertura foi confirmada pela mesa.
    pub acked: bool,
}

/// Resultado consolidado de `place()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fills {
    /// ID da intenção gravada em `intents`.
    pub intent_id: i64,
    /// Lista de fills por cobertura.
    pub items: Vec<CoverAck>,
    /// Valor lido no preview antes do `confirm()`.
    pub preview_cents: u64,
    /// `true` quando apenas parte das coberturas recebeu ack (`naked` — §9).
    pub naked: bool,
}

/// Abstração do cliente de atuação na mesa (implementada por `DryRunActuator` e pelo cliente CDP live).
pub trait CdpActuator: Send + Sync {
    /// Despacha clique (`mousePressed` + `mouseReleased`) na coordenada `(x, y)` do canvas.
    fn click(&self, x: i32, y: i32) -> Result<(), ExecError>;
    /// Clica em uma cobertura específica da mesa (usa resolução DOM dinâmica no modo `--live`).
    fn click_cover(&self, cover: &Cover, cal: &Calibration) -> Result<(), ExecError> {
        let (x, y) = cal.coords(cover)?;
        self.click(x, y)
    }
    /// Seleciona a ficha de denominação `denom_cents`.
    fn click_chip(&self, denom_cents: u64, cal: &Calibration) -> Result<(), ExecError>;
    /// Lê o valor total acumulado no preview do bet slip antes de confirmar.
    fn read_bet_total(&self) -> Result<u64, ExecError>;
    /// Limpa todas as fichas da mesa caso o preview divirja do esperado.
    fn clear_bets(&self, cal: &Calibration) -> Result<(), ExecError>;
    /// Confirma a aposta no bet slip (chamado SOMENTE após validar o preview).
    fn confirm(&self, cal: &Calibration) -> Result<(), ExecError>;
    /// Coleta os acks por cobertura após a confirmação.
    fn collect_acks(&self, plan: &Plan) -> Result<Vec<CoverAck>, ExecError>;
}

#[derive(Debug, Default)]
struct DryRunState {
    selected_chip_cents: Option<u64>,
    staged_total_cents: u64,
    confirmed_count: u64,
    cleared_count: u64,
    clicks: Vec<(i32, i32)>,
    forced_preview_delta_cents: i64,
    partial_ack_index: Option<usize>,
}

/// Atuador determinístico para modo `--dry-run` (§1.5 e §15.5).
#[derive(Debug, Default, Clone)]
pub struct DryRunActuator {
    inner: Arc<Mutex<DryRunState>>,
}

impl DryRunActuator {
    /// Cria um novo atuador `--dry-run`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Injeta um desvio artificial no preview (em centavos) para testar aborto por `PreviewMismatch`.
    pub fn set_forced_preview_delta(&self, delta_cents: i64) {
        if let Ok(mut g) = self.inner.lock() {
            g.forced_preview_delta_cents = delta_cents;
        }
    }

    /// Simula falha de ack em um índice específico para testar detecção de `naked` fill (§9).
    pub fn set_partial_ack_failure(&self, idx: Option<usize>) {
        if let Ok(mut g) = self.inner.lock() {
            g.partial_ack_index = idx;
        }
    }

    /// Quantidade de confirmações executadas até o momento.
    pub fn confirmed_count(&self) -> u64 {
        self.inner.lock().map(|g| g.confirmed_count).unwrap_or(0)
    }

    /// Quantidade de chamadas a `clear_bets()` executadas.
    pub fn cleared_count(&self) -> u64 {
        self.inner.lock().map(|g| g.cleared_count).unwrap_or(0)
    }
}

impl CdpActuator for DryRunActuator {
    fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        g.clicks.push((x, y));
        if let Some(chip) = g.selected_chip_cents {
            g.staged_total_cents = g.staged_total_cents.saturating_add(chip);
        }
        Ok(())
    }

    fn click_chip(&self, denom_cents: u64, cal: &Calibration) -> Result<(), ExecError> {
        let (x, y) = cal.chip_coords(denom_cents)?;
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        g.clicks.push((x, y));
        g.selected_chip_cents = Some(denom_cents);
        Ok(())
    }

    fn read_bet_total(&self) -> Result<u64, ExecError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        // O desvio forçado só se materializa depois do primeiro clique (mesa já suja),
        // simulando a divergência que aparece ao colocar fichas — nunca uma mesa suja antes da entrada.
        let adjusted = if g.staged_total_cents > 0 {
            (g.staged_total_cents as i64) + g.forced_preview_delta_cents
        } else {
            0
        };
        Ok(adjusted.max(0) as u64)
    }

    fn clear_bets(&self, cal: &Calibration) -> Result<(), ExecError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        g.clicks.push(cal.clear_coords);
        g.staged_total_cents = 0;
        g.selected_chip_cents = None;
        g.cleared_count += 1;
        Ok(())
    }

    fn confirm(&self, cal: &Calibration) -> Result<(), ExecError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        g.clicks.push(cal.confirm_coords);
        g.confirmed_count += 1;
        g.staged_total_cents = 0;
        g.selected_chip_cents = None;
        Ok(())
    }

    fn collect_acks(&self, plan: &Plan) -> Result<Vec<CoverAck>, ExecError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        let mut out = Vec::with_capacity(plan.covers.len());
        for (idx, &(cover, stake_cents)) in plan.covers.iter().enumerate() {
            let acked = g.partial_ack_index != Some(idx);
            out.push(CoverAck {
                cover,
                stake_cents,
                acked,
            });
        }
        Ok(out)
    }
}

/// Telemetria ao vivo da mesa Playtech lida via `GET /act/preview` no sidecar Go.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiveTableTelemetry {
    /// Aposta atual acumulada no pano em centavos (`Bet R$...`).
    pub preview_cents: u64,
    /// Saldo real lido na mesa em centavos (`Balance R$...`), `None` se ainda não disponível.
    pub balance_cents: Option<i64>,
    /// Último ganho exibido na mesa em centavos (`Last Win R$...`).
    pub last_win_cents: u64,
    /// `true` quando a janela `PLACE YOUR BETS` está aberta.
    pub round_open: bool,
    /// Código da rodada da Playtech (`Game Code: ...`).
    pub game_code: String,
    /// Fita ordenada de até 12 números exibidos na barra inferior direita da Playtech (`strip[0]` = mais recente).
    pub strip: Vec<u8>,
}

/// Atuador HTTP/CDP real para o modo `--live` (envia `Input.dispatchMouseEvent` + resolução DOM via sidecar Go).
#[derive(Debug, Clone)]
pub struct LiveHttpActuator {
    sidecar_addr: String,
    staged_state: Arc<Mutex<(Option<u64>, u64)>>,
}

impl LiveHttpActuator {
    /// Cria um atuador `--live` apontando para o sidecar (`127.0.0.1:8787`).
    pub fn new(sidecar_addr: impl Into<String>) -> Self {
        Self {
            sidecar_addr: sidecar_addr.into(),
            staged_state: Arc::new(Mutex::new((None, 0))),
        }
    }

    /// Consulta a telemetria completa em tempo real da mesa Playtech (`Balance`, `Bet`, `Last Win`, `Game Code` e fita de 12 números).
    pub fn fetch_telemetry(&self) -> Option<LiveTableTelemetry> {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let req = format!(
            "GET /act/preview HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.sidecar_addr
        );
        let mut stream = TcpStream::connect(&self.sidecar_addr).ok()?;
        let _ = stream.set_read_timeout(Some( std::time::Duration::from_millis(800)));
        stream.write_all(req.as_bytes()).ok()?;
        let mut resp = String::new();
        stream.read_to_string(&mut resp).ok()?;
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(body.trim()).ok()?;

        let preview_cents = parsed
            .get("preview_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0) as u64;
        let bal_raw = parsed
            .get("balance_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let balance_cents = if bal_raw > 0 { Some(bal_raw) } else { None };
        let last_win_cents = parsed
            .get("last_win_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0) as u64;
        let round_open = parsed
            .get("round_open")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let game_code = parsed
            .get("game_code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let strip = parsed
            .get("strip")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_u64())
                    .filter(|&n| n <= 36)
                    .map(|n| n as u8)
                    .collect::<Vec<u8>>()
            })
            .unwrap_or_default();

        Some(LiveTableTelemetry {
            preview_cents,
            balance_cents,
            last_win_cents,
            round_open,
            game_code,
            strip,
        })
    }

    fn post_target_click(&self, target: &str, x: i32, y: i32) -> Result<(), ExecError> {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let body = format!(r#"{{"target":"{target}","x":{x},"y":{y}}}"#);
        let req = format!(
            "POST /act/click HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.sidecar_addr,
            body.len(),
            body
        );
        let mut stream = TcpStream::connect(&self.sidecar_addr)
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
        stream
            .write_all(req.as_bytes())
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        let mut resp = String::new();
        stream
            .read_to_string(&mut resp)
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        if !resp.contains("200 OK") {
            return Err(ExecError::CdpTransport(resp));
        }
        Ok(())
    }
}

impl CdpActuator for LiveHttpActuator {
    fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
        if let Ok(mut g) = self.staged_state.lock() {
            if let Some(chip) = g.0 {
                g.1 = g.1.saturating_add(chip);
            }
        }
        self.post_target_click("", x, y)
    }

    fn click_cover(&self, cover: &Cover, cal: &Calibration) -> Result<(), ExecError> {
        let (x, y) = cal.coords(cover)?;
        if let Ok(mut g) = self.staged_state.lock() {
            if let Some(chip) = g.0 {
                g.1 = g.1.saturating_add(chip);
            }
        }
        self.post_target_click(&cover.canonical_key(), x, y)
    }

    fn click_chip(&self, denom_cents: u64, cal: &Calibration) -> Result<(), ExecError> {
        let (x, y) = cal.chip_coords(denom_cents)?;
        if let Ok(mut g) = self.staged_state.lock() {
            g.0 = Some(denom_cents);
        }
        self.post_target_click(&format!("chip:{denom_cents}"), x, y)
    }

    fn read_bet_total(&self) -> Result<u64, ExecError> {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let req = format!(
            "GET /act/preview HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.sidecar_addr
        );
        let mut stream = TcpStream::connect(&self.sidecar_addr)
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
        stream
            .write_all(req.as_bytes())
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        let mut resp = String::new();
        stream
            .read_to_string(&mut resp)
            .map_err(|e| ExecError::CdpTransport(e.to_string()))?;
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(body.trim())
            .map_err(|e| ExecError::CdpTransport(format!("preview inválido: {e}")))?;
        // Sempre devolve o total REAL da mesa: nunca mascara divergência com o valor que achamos ter apostado.
        Ok(parsed
            .get("preview_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0) as u64)
    }

    fn clear_bets(&self, cal: &Calibration) -> Result<(), ExecError> {
        if let Ok(mut g) = self.staged_state.lock() {
            g.0 = None;
            g.1 = 0;
        }
        // A mesa remove uma ficha por clique em UNDO: repete até o preview real ficar zerado.
        for _ in 0..12 {
            match self.read_bet_total() {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(err) => return Err(err),
            }
            self.post_target_click("clear", cal.clear_coords.0, cal.clear_coords.1)?;
            std::thread::sleep(std::time::Duration::from_millis(140));
        }
        match self.read_bet_total() {
            Ok(0) => Ok(()),
            Ok(restante) => Err(ExecError::CdpTransport(format!(
                "mesa não zerou após 12 UNDO (restante {restante} centavos)"
            ))),
            Err(err) => Err(err),
        }
    }

    fn confirm(&self, _cal: &Calibration) -> Result<(), ExecError> {
        // Na Playtech Roleta Brasileira, clicar na ficha + clicar no pano já confirma a aposta na mesa
        // (não existe botão "Confirm" separado; o giro confirma automaticamente no encerramento da janela).
        if let Ok(mut g) = self.staged_state.lock() {
            g.0 = None;
            g.1 = 0;
        }
        Ok(())
    }

    fn collect_acks(&self, plan: &Plan) -> Result<Vec<CoverAck>, ExecError> {
        Ok(plan
            .covers
            .iter()
            .map(|&(cover, stake_cents)| CoverAck {
                cover,
                stake_cents,
                acked: true,
            })
            .collect())
    }
}

/// Lê o total real da mesa aguardando o DOM da Playtech refletir os cliques recém-enviados.
/// Retorna o último valor lido (mesmo divergente) para permitir diagnóstico e `clear_bets()`.
fn read_settled(cdp: &dyn CdpActuator, expected: u64) -> Result<u64, ExecError> {
    let mut last = cdp.read_bet_total()?;
    for _ in 0..3 {
        if last == expected {
            return Ok(last);
        }
        std::thread::sleep(std::time::Duration::from_millis(90));
        last = cdp.read_bet_total()?;
    }
    Ok(last)
}

/// Executa o plano de aposta seguindo estritamente o protocolo do §9 e invariantes §1.3, §1.4 e §1.8:
/// `grava intent → clica cobertura → aplica denominações → lê preview → compara → confirm() → coleta acks`.
pub fn place(
    pool: &SqlitePool,
    cdp: &dyn CdpActuator,
    plan: &Plan,
    cal: &Calibration,
    kill_switch: &KillSwitch,
    now_ms: i64,
) -> Result<Fills, ExecError> {
    if kill_switch.is_tripped() {
        return Err(ExecError::KillSwitchActive);
    }

    // 0. A mesa precisa estar zerada: aposta residual jamais é empilhada com a entrada nova.
    let residual = cdp.read_bet_total()?;
    if residual > 0 {
        warn!(residual, "aposta residual na mesa: limpando antes da entrada");
        cdp.clear_bets(cal)?;
        let after = cdp.read_bet_total()?;
        if after > 0 {
            let _ = insert_error(
                pool,
                "dirty_table",
                &format!("preview={after} centavos antes da entrada"),
                now_ms,
            );
            return Err(ExecError::PreviewMismatch {
                preview: after,
                expected: 0,
            });
        }
    }

    // 1. Grava a intent ANTES de qualquer clique (§1.8 e §1.3 UNIQUE(strategy_id, spin_id)).
    let covers_desc: Vec<String> = plan.covers.iter().map(|(c, _)| c.canonical_key()).collect();
    let auto_reason = format!(
        "degrau={} cobrindo={} total={}c",
        plan.ladder_step,
        covers_desc.join("+"),
        plan.total_cents
    );
    let intent_id = match insert_intent(
        pool,
        &NewIntent {
            spin_id: plan.spin_id.clone(),
            strategy_id: plan.strategy_id.clone(),
            ladder_step: plan.ladder_step,
            stake_cents: plan.total_cents,
            ev_cents: plan.ev_cents,
            confidence: plan.confidence,
            decision: IntentDecision::Placed,
            reason: Some(auto_reason),
            created_at: now_ms,
        },
    ) {
        Ok(id) => id,
        Err(StoreError::ConstraintViolation(_)) => {
            return Err(ExecError::AlreadyEntered {
                strategy_id: plan.strategy_id.clone(),
                spin_id: plan.spin_id.clone(),
            });
        }
        Err(e) => return Err(ExecError::Store(e)),
    };

    // 2. Aplica as denominações de fichas em cada cobertura via coordenadas calibradas / resolução DOM.
    // Seleciona a ficha no chip-rack apenas quando a denominação muda, evitando cliques redundantes entre as coberturas.
    let mut last_denom: Option<u64> = None;
    let mut cumulative_expected: u64 = 0;
    let mut verified_so_far: u64 = 0;
    let applied = (|| -> Result<(), ExecError> {
    for (cover, cents) in &plan.covers {
        let _ = cal.coords(cover)?;
        for (denom, n) in decompose(*cents, &cal.denoms) {
            if last_denom != Some(denom) {
                cdp.click_chip(denom, cal)?;
                last_denom = Some(denom);
            }
            for _ in 0..n {
                cdp.click_cover(cover, cal)?;
            }
        }

        // 2.1 Confere o acumulado APÓS CADA cobertura: é assim que uma dúzia de 40¢ ao lado de uma de 20¢
        // é detectada antes de confirmar, em vez de escondida atrás de um total que fecha por coincidência.
        cumulative_expected += *cents;
        let seen = read_settled(cdp, cumulative_expected)?;
        if seen == cumulative_expected {
            verified_so_far = seen;
            continue;
        }
        // Preview ainda não reagiu a esta cobertura (mesa lenta): validação fica para o total final.
        if seen == verified_so_far {
            continue;
        }
        error!(
            cover = %cover.canonical_key(),
            seen,
            expected = cumulative_expected,
            intent_id,
            "cobertura com valor divergente: limpando a mesa e abortando"
        );
        let _ = cdp.clear_bets(cal);
        update_intent_decision(pool, intent_id, IntentDecision::Aborted, Some("cover_mismatch"))?;
        let _ = insert_error(
            pool,
            "cover_mismatch",
            &format!(
                "intent={intent_id} cover={} lido={seen} esperado={cumulative_expected}",
                cover.canonical_key()
            ),
            now_ms,
        );
        return Err(ExecError::PreviewMismatch {
            preview: seen,
            expected: cumulative_expected,
        });
    }
        Ok(())
    })();
    if let Err(err) = applied {
        // Cobertura não resolvida no pano (sidecar respondeu 409): nunca confirmar aposta em
        // coordenada cega — limpa a mesa e aborta. Os desvios de preview já se limpam internamente.
        if matches!(err, ExecError::CdpTransport(_)) {
            let _ = cdp.clear_bets(cal);
            let _ = update_intent_decision(
                pool,
                intent_id,
                IntentDecision::Aborted,
                Some("click_unresolved"),
            );
            let _ = insert_error(
                pool,
                "click_unresolved",
                &format!("intent={intent_id} {err}"),
                now_ms,
            );
        }
        return Err(err);
    }


    // 3. Lê o preview no bet slip ANTES de confirmar (§1.4).
    let preview = read_settled(cdp, plan.total_cents)?;
    if (preview as i64 - plan.total_cents as i64).abs() > 1 {
        error!(
            preview,
            expected = plan.total_cents,
            intent_id,
            "preview divergente > 1 centavo: executando clear_bets() e abortando"
        );
        let _ = cdp.clear_bets(cal);
        update_intent_decision(
            pool,
            intent_id,
            IntentDecision::Aborted,
            Some("preview_mismatch"),
        )?;
        let _ = insert_error(
            pool,
            "preview_mismatch",
            &format!("intent={intent_id} preview={preview} expected={}", plan.total_cents),
            now_ms,
        );
        return Err(ExecError::PreviewMismatch {
            preview,
            expected: plan.total_cents,
        });
    }

    // 4. Confirma a aposta somente após preview validado.
    cdp.confirm(cal)?;

    // 5. Coleta acks por cobertura e persiste em `fills` (§9).
    let acks = cdp.collect_acks(plan)?;
    let mut acked_count = 0usize;
    for item in &acks {
        if item.acked {
            acked_count += 1;
        }
        insert_fill(
            pool,
            &NewFill {
                intent_id,
                cover: item.cover.canonical_key(),
                stake_cents: item.stake_cents,
                acked: item.acked,
                confirmed_at: if item.acked { Some(now_ms) } else { None },
            },
        )?;
    }

    let naked = acked_count > 0 && acked_count < acks.len();
    if naked {
        warn!(
            intent_id,
            acked_count,
            total_covers = acks.len(),
            "fill parcial (naked) detectado: registrado sem tentar completar após fechamento"
        );
        let _ = insert_error(
            pool,
            "naked_fill",
            &format!("intent={intent_id} acked={acked_count}/{}", acks.len()),
            now_ms,
        );
    }

    Ok(Fills {
        intent_id,
        items: acks,
        preview_cents: preview,
        naked,
    })
}
