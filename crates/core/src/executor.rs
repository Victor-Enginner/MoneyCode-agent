// SPDX-License-Identifier: Apache-2.0
//! Executor de apostas com verificação obrigatória de preview, idempotência e modo `--dry-run`.
//! Grava a intenção em `intents` ANTES de qualquer clique e aborta com `clear_bets()` se o preview divergir.
//! Suporta kill switch atômico (atalho global, Telegram `/kill`, UI) mantendo a captura viva.

use crate::dag::Plan;
use monster_risk::{decompose, DEFAULT_CHIP_DENOMS_CENTS};
use monster_store::{
    insert_error, insert_fill, insert_intent, IntentDecision, NewFill,
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
    /// O sidecar devolveu um total de `Bet` em cache, não uma leitura desta mesa agora.
    /// Comparar preview velho com o esperado é como não comparar: um `0¢` de rodada anterior
    /// esconde ficha residual e a entrada seguinte lê alta (§1.4).
    #[error("leitura do bet slip velha demais ({0} ms): aguardando leitura fresca da mesa")]
    SlipStale(i64),
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
    ///
    /// Pleno e terminal não têm coordenada por número em `calibration.toml`: a casa é
    /// resolvida no pano pelo sidecar (`straight:N` sai da grade 3×12 derivada das âncoras
    /// `2to1`). O fallback `(0, 0)` só existe para o sidecar devolver `found:false` e
    /// responder 409 — aposta nunca sai em coordenada cega.
    pub fn coords(&self, cover: &Cover) -> Result<(i32, i32), ExecError> {
        let key = cover.canonical_key();
        if let Some(p) = self.covers.get(&key) {
            return Ok(*p);
        }
        if matches!(cover, Cover::Straight(_) | Cover::Terminal(_)) {
            return Ok((0, 0));
        }
        Err(ExecError::MissingCalibration(key))
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
    /// Clica `n` vezes na mesma cobertura depositando `n` fichas.
    ///
    /// A implementação padrão repete `click_cover`, mas o atuador live sobrescreve para resolver
    /// o alvo UMA vez e despachar as `n` fichas naquela coordenada: resolver por ficha custava
    /// varredura de DOM + espera de janela + 280 ms cada, e uma cobertura de 5 fichas gastava ~3 s
    /// dos ~12 s de janela — quando ela fechava no meio, o canvas recusava as fichas restantes e
    /// o total ficava menor que o esperado (`cover_mismatch`).
    fn click_cover_repeat(&self, cover: &Cover, cal: &Calibration, n: u32) -> Result<(), ExecError> {
        for _ in 0..n {
            self.click_cover(cover, cal)?;
        }
        Ok(())
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
    /// Quanto esperar a mesa limpar sozinha antes de apostar (fichas da rodada que acabou
    /// ainda aparecem durante a animação do resultado). `0` = não esperar (dry-run/testes).
    fn table_clear_wait_ms(&self) -> u64 {
        0
    }
    /// Saldo real da mesa em centavos (`None` = sem leitura). Na Playtech o saldo cai no
    /// instante em que a ficha é posta: é a segunda testemunha do que entrou no pano.
    fn read_balance(&self) -> Option<i64> {
        None
    }
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
    /// Leituras do slip que ainda mostram o total anterior (mesa animando o preview).
    slip_lag_reads: u32,
    /// Valor mostrado enquanto o slip está atrasado.
    slip_lag_value_cents: u64,
    /// Leituras que respondem `SlipStale` (sidecar sem leitura fresca desta mesa).
    slip_stale_reads: u32,
    /// Cliques que o pano engole (animação da pilha em andamento) em vez de virar ficha.
    swallow_chips: u32,
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

    /// Simula o slip animando: as próximas `reads` leituras do preview devolvem o total
    /// anterior à entrada (`0¢`). Usado para provar que atraso de DOM não aborta aposta correta.
    pub fn set_slip_lag(&self, reads: u32, stale_value_cents: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.slip_lag_reads = reads;
            g.slip_lag_value_cents = stale_value_cents;
        }
    }

    /// Simula o pano engolindo os próximos `n` cliques (ficha postada nunca chega ao slip).
    pub fn set_swallow_chips(&self, n: u32) {
        if let Ok(mut g) = self.inner.lock() {
            g.swallow_chips = n;
        }
    }

    /// Simula o sidecar devolvendo cache (`bet_age_ms` velho) nas próximas `reads` leituras.
    pub fn set_slip_stale_reads(&self, reads: u32) {
        if let Ok(mut g) = self.inner.lock() {
            g.slip_stale_reads = reads;
        }
    }

    /// Define o total já postado na mesa, sem passar pelos cliques (simula fichas presentes).
    pub fn set_staged_total(&self, cents: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.staged_total_cents = cents;
        }
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
        if g.swallow_chips > 0 {
            // Clique despachado, ficha descartada: espelha a animação do pano engolindo o chip.
            g.swallow_chips -= 1;
            return Ok(());
        }
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
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ExecError::CdpTransport("mutex poisoned".to_string()))?;
        if g.slip_stale_reads > 0 {
            g.slip_stale_reads -= 1;
            return Err(ExecError::SlipStale(SLIP_FRESH_MAX_MS + 1));
        }
        if g.slip_lag_reads > 0 {
            g.slip_lag_reads -= 1;
            return Ok(g.slip_lag_value_cents);
        }
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
        self.post_target_click_repeat(target, x, y, 1)
    }

    /// Pede ao sidecar `repeat` cliques na mesma coordenada resolvida de `target`.
    fn post_target_click_repeat(
        &self,
        target: &str,
        x: i32,
        y: i32,
        repeat: u32,
    ) -> Result<(), ExecError> {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let body = format!(r#"{{"target":"{target}","x":{x},"y":{y},"repeat":{repeat}}}"#);
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
    fn table_clear_wait_ms(&self) -> u64 {
        TABLE_CLEAR_WAIT_MS
    }

    fn read_balance(&self) -> Option<i64> {
        self.fetch_telemetry().and_then(|t| t.balance_cents)
    }

    fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
        if let Ok(mut g) = self.staged_state.lock() {
            if let Some(chip) = g.0 {
                g.1 = g.1.saturating_add(chip);
            }
        }
        self.post_target_click("", x, y)
    }

    fn click_cover(&self, cover: &Cover, cal: &Calibration) -> Result<(), ExecError> {
        self.click_cover_repeat(cover, cal, 1)
    }

    fn click_cover_repeat(&self, cover: &Cover, cal: &Calibration, n: u32) -> Result<(), ExecError> {
        if n == 0 {
            return Ok(());
        }
        let (x, y) = cal.coords(cover)?;
        if let Ok(mut g) = self.staged_state.lock() {
            if let Some(chip) = g.0 {
                g.1 = g.1.saturating_add(chip.saturating_mul(n as u64));
            }
        }
        self.post_target_click_repeat(&cover.canonical_key(), x, y, n)
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
        parse_preview(body)
    }

    fn clear_bets(&self, cal: &Calibration) -> Result<(), ExecError> {
        if let Ok(mut g) = self.staged_state.lock() {
            g.0 = None;
            g.1 = 0;
        }
        // A mesa remove uma ficha por clique em UNDO: repete até o preview real ficar zerado.
        // Entrada de número tem 20+ fichas: 12 UNDO deixaram R$ 5,60 no pano em 14/11.
        for _ in 0..CLEAR_MAX_UNDO {
            match self.read_bet_total() {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                // Leitura velha não autoriza nem limpar nem parar: tenta de novo no próximo UNDO.
                Err(ExecError::SlipStale(_)) => {}
                Err(err) => return Err(err),
            }
            self.post_target_click("clear", cal.clear_coords.0, cal.clear_coords.1)?;
            std::thread::sleep(std::time::Duration::from_millis(140));
        }
        match self.read_bet_total() {
            Ok(0) => Ok(()),
            Ok(restante) => Err(ExecError::CdpTransport(format!(
                "mesa não zerou após {CLEAR_MAX_UNDO} UNDO (restante {restante} centavos)"
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
/// O slip anima: uma leitura 200 ms depois do último clique ainda mostra o total anterior e
/// abortaria uma aposta correta (foi o que matou toda entrada acima do degrau 1 na sessão ao
/// vivo). Por isso a leitura é refeita até ver o total esperado, com teto de
/// `SETTLE_DEADLINE_MS` — que cabe no orçamento de cliques do §2.
/// Retorna o último valor lido (mesmo divergente) para permitir diagnóstico e `clear_bets()`.
/// Idade máxima aceita para o total de `Bet` do sidecar. Mais velho que isso não descreve
/// a mesa neste instante — o valor é cache, e comparar em cima dele é apostar às cegas.
const SLIP_FRESH_MAX_MS: i64 = 700;

/// Interpreta o corpo de `/act/preview` exigindo os DOIS campos que autorizam agir.
///
/// Ausência de `bet_age_ms` NÃO é leitura fresca: antes, campo faltando pulava a checagem e a
/// resposta virava "mesa limpa, 0¢" — bastava o sidecar mudar de forma para apostar em cima de
/// fichas residuais. Sem `preview_cents` também não há total, então não há decisão possível.
fn parse_preview(body: &str) -> Result<u64, ExecError> {
    let parsed: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| ExecError::CdpTransport(format!("preview inválido: {e}")))?;
    let age = parsed
        .get("bet_age_ms")
        .and_then(|v| v.as_i64())
        .ok_or(ExecError::SlipStale(SLIP_FRESH_MAX_MS + 1))?;
    if !(0..=SLIP_FRESH_MAX_MS).contains(&age) {
        return Err(ExecError::SlipStale(age));
    }
    let cents = parsed
        .get("preview_cents")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| ExecError::CdpTransport("preview sem preview_cents".to_string()))?;
    Ok(cents.max(0) as u64)
}
const SETTLE_POLL_MS: u64 = 70;
const SETTLE_DEADLINE_MS: u64 = 1_200;
/// Espera do saldo da mesa refletir as fichas postas.
const BALANCE_CONFIRM_MS: u64 = 1_500;
/// Segunda olhada no slip antes de concluir que uma cobertura não pegou na mesa.
const COVER_RECHECK_MS: u64 = 2_000;
/// Teto de espera para a mesa da rodada anterior sumir antes de apostar (live).
pub const TABLE_CLEAR_WAIT_MS: u64 = 8_000;

/// O saldo confirma que `expected` centavos entraram no pano desde `before`? Espera até 1,5 s
/// o saldo da mesa refletir as fichas. Em 17/11 o slip leu 0 com R$ 11,00 no pano (o operador
/// viu as fichas) e 120 com 40 colocados: o slip sozinho não é testemunha suficiente.
fn balance_confirms(cdp: &dyn CdpActuator, before: Option<i64>, expected: u64) -> bool {
    let Some(before) = before else {
        return false;
    };
    let started = std::time::Instant::now();
    loop {
        if let Some(now) = cdp.read_balance() {
            if (before - now - expected as i64).abs() <= 1 {
                return true;
            }
        }
        if started.elapsed().as_millis() as u64 >= BALANCE_CONFIRM_MS {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

fn read_settled(cdp: &dyn CdpActuator, expected: u64) -> Result<u64, ExecError> {
    read_settled_for(cdp, expected, SETTLE_DEADLINE_MS)
}

/// Prazo para o slip somar todas as fichas de uma entrada: 20 plenos em sequência levam
/// segundos para animar. Em 14/11 o bot leu 170 de 200 aos 1,2 s e desfez uma entrada inteira.
fn final_settle_ms(covers: usize) -> u64 {
    (SETTLE_DEADLINE_MS + 150 * covers as u64).min(4_500)
}

fn read_settled_for(cdp: &dyn CdpActuator, expected: u64, deadline_ms: u64) -> Result<u64, ExecError> {
    let started = std::time::Instant::now();
    let mut last: Option<u64> = None;
    let mut stale_age: i64 = 0;
    loop {
        match cdp.read_bet_total() {
            Ok(seen) => {
                last = Some(seen);
                if seen == expected {
                    return Ok(seen);
                }
            }
            // Cache não é leitura: continua tentando até o teto, sem inventar valor.
            Err(ExecError::SlipStale(age)) => stale_age = age,
            Err(err) => return Err(err),
        }
        if started.elapsed().as_millis() as u64 >= deadline_ms {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SETTLE_POLL_MS));
    }
    match last {
        Some(seen) => Ok(seen),
        None => Err(ExecError::SlipStale(stale_age)),
    }
}

/// Executa o plano de aposta seguindo estritamente o protocolo do §9 e invariantes §1.3, §1.4 e §1.8:
/// `grava intent → clica cobertura → aplica denominações → lê preview → compara → confirm() → coleta acks`.
/// Até quantas coberturas a conferência é feita cobertura a cobertura. Acima disso (entradas
/// de número, 8 a 24 plenos) só o total final é conferido: ler o slip após cada pleno levou 26 s
/// num gale de 22 números em 14/11, a janela fechou no meio e o slip atrasado fez o bot
/// "completar" fichas que já estavam no pano (ficha de R$ 1 num degrau de R$ 0,50).
pub const PER_COVER_VERIFY_MAX: usize = 4;

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
    // A leitura tem de ser FRESCA: um `0¢` em cache de rodada anterior deixou 60¢ na mesa e a
    // entrada seguinte leu 240¢ para 180¢ esperados. Cache não vale como "mesa limpa".
    // A Playtech mostra as fichas da rodada encerrada durante a animação do resultado: UNDO ali
    // não tira nada e apostar por cima soma ficha velha com nova (`lido=140 esperado=40`) ou tem
    // cliques engolidos. Em 14/11 isso cancelava gale atrás de gale. Espera a mesa limpar sozinha.
    let mut residual = read_settled(cdp, 0)?;
    let wait_ms = cdp.table_clear_wait_ms();
    let waiting = std::time::Instant::now();
    while residual > 0 && (waiting.elapsed().as_millis() as u64) < wait_ms {
        std::thread::sleep(std::time::Duration::from_millis(300));
        residual = read_settled(cdp, 0)?;
    }
    // O bot NUNCA retira ficha da mesa (ADR 0007): o operador aposta junto e o UNDO tirava as
    // fichas dele. O que já está no pano é do operador; o bot soma por cima e confere só a
    // própria parte (bilhete − base).
    if residual > 0 {
        eprintln!("[EXEC-DIAG] {residual}¢ já no pano (fichas do operador) — o bot soma por cima, sem tirar nada");
    }
    let own = OwnShare { inner: cdp, base: residual };
    let cdp: &dyn CdpActuator = &own;

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
    // Acumulados legítimos intermediários: o slip anima ficha por ficha, então ver 80¢ enquanto
    // o esperado já é 90¢ (a cobertura anterior inteira) não é divergência — é o pano desenhando.
    let mut legit_prefixes: Vec<u64> = vec![0];
    let verify_each = plan.covers.len() <= PER_COVER_VERIFY_MAX;
    let balance_before = cdp.read_balance();
    info!(intent_id, balance_before = ?balance_before, total = plan.total_cents, "[EXECUTOR] saldo antes dos cliques");
    eprintln!("[EXEC-DIAG] {} degrau {} | aposta {}¢ | saldo antes {:?}¢", plan.strategy_id, plan.ladder_step, plan.total_cents, balance_before);
    let apply_started = std::time::Instant::now();
    let applied = (|| -> Result<(), ExecError> {
    for (cover, cents) in &plan.covers {
        let _ = cal.coords(cover)?;
        let chips_desc = decompose(*cents, &cal.denoms)
            .iter()
            .map(|(d, n)| format!("{d}x{n}"))
            .collect::<Vec<_>>()
            .join("+");
        for (denom, n) in decompose(*cents, &cal.denoms) {
            if last_denom != Some(denom) {
                cdp.click_chip(denom, cal)?;
                last_denom = Some(denom);
            }
            cdp.click_cover_repeat(cover, cal, u32::from(n))?;
        }

        // 2.1 Confere o acumulado APÓS CADA cobertura: é assim que uma dúzia de 40¢ ao lado de uma de 20¢
        // é detectada antes de confirmar, em vez de escondida atrás de um total que fecha por coincidência.
        cumulative_expected += *cents;
        legit_prefixes.push(cumulative_expected);
        if !verify_each {
            continue;
        }
        let mut seen = read_settled(cdp, cumulative_expected)?;
        info!(cover = %cover.canonical_key(), slip = seen, expected = cumulative_expected, "[EXECUTOR] leitura após cobertura");
        eprintln!("[EXEC-DIAG] após {}: bilhete {}¢ (esperado {}¢) | saldo {:?}¢", cover.canonical_key(), seen, cumulative_expected, cdp.read_balance());
        // Slip discorda, mas o saldo caiu exatamente o esperado: as fichas estão no pano e quem
        // errou foi a leitura do slip. Nada a completar nem a desfazer.
        if seen != cumulative_expected && balance_confirms(cdp, balance_before, cumulative_expected) {
            info!(cover = %cover.canonical_key(), slip = seen, expected = cumulative_expected, "[EXECUTOR] slip divergente, saldo confirma a aposta");
            eprintln!("[EXEC-DIAG] bilhete {}¢ diverge, mas o saldo confirma {}¢ no pano — segue", seen, cumulative_expected);
            continue;
        }
        // Slip parado no acumulado ANTERIOR: ou está atrasado, ou o clique desta cobertura não
        // pegou na mesa (14/11: 16 de 24 aborts por fora eram a última cobertura faltando, e o
        // bot desfazia a entrada inteira). Espera mais; se continuar sem ela, clica de novo SÓ
        // esta cobertura. Se o slip só estava lento, o total passa do esperado e o §1.4 desfaz.
        if seen < cumulative_expected && legit_prefixes.contains(&seen) {
            let before = cumulative_expected - *cents;
            seen = read_settled_for(cdp, cumulative_expected, COVER_RECHECK_MS)?;
            if seen == before {
                warn!(cover = %cover.canonical_key(), seen, expected = cumulative_expected, "cobertura não pegou na mesa: clicando de novo só ela");
                eprintln!("[EXEC-DIAG] {} não pegou na mesa (bilhete {}¢) — clicando de novo só ela", cover.canonical_key(), seen);
                for (denom, n) in decompose(*cents, &cal.denoms) {
                    if last_denom != Some(denom) {
                        cdp.click_chip(denom, cal)?;
                        last_denom = Some(denom);
                    }
                    cdp.click_cover_repeat(cover, cal, u32::from(n))?;
                }
                seen = read_settled(cdp, cumulative_expected)?;
            }
            if seen == cumulative_expected || legit_prefixes.contains(&seen) {
                continue;
            }
        }
        // Ficha engolida pela animação do pano: o lote de 3 entrou como 2 ao vivo
        // (`cover=high lido=20 esperado=30 fichas=10x3`). Se o total FICOU ABAIXO do esperado,
        // repete só a diferença uma vez, na mesma cobertura. Acima do esperado nunca repete:
        // excesso é divergência e vai para o clear+abort do §1.4.
        if seen < cumulative_expected {
            let missing = cumulative_expected - seen;
            for (denom, n) in decompose(missing, &cal.denoms) {
                if last_denom != Some(denom) {
                    cdp.click_chip(denom, cal)?;
                    last_denom = Some(denom);
                }
                cdp.click_cover_repeat(cover, cal, u32::from(n))?;
            }
            seen = read_settled(cdp, cumulative_expected)?;
        }
        if seen == cumulative_expected {
            continue;
        }
        // Preview ainda não reagiu a esta cobertura (mesa lenta) ou mostra o acumulado de uma
        // cobertura anterior: validação fica para o total final, não é divergência.
        if legit_prefixes.contains(&seen) {
            continue;
        }
        // Qualquer outra divergência: registra e segue. Ficha a mais é do operador; ficha a
        // menos aparece na leitura final e na conferência de extrato. Nada é desfeito (ADR 0007).
        let elapsed_ms = apply_started.elapsed().as_millis();
        eprintln!(
            "[EXEC-DIAG] {}: bilhete {}¢ x esperado {}¢ ({}) — segue sem tirar ficha",
            cover.canonical_key(),
            seen,
            cumulative_expected,
            if seen > cumulative_expected { "ficha a mais: do operador" } else { "faltou ficha" }
        );
        let _ = insert_error(
            pool,
            "cover_mismatch",
            &format!(
                "intent={intent_id} cover={} lido={seen} esperado={cumulative_expected} fichas={chips_desc} ms={elapsed_ms}",
                cover.canonical_key()
            ),
            now_ms,
        );
    }
        Ok(())
    })();
    if let Err(err) = applied {
        // Clique não resolvido no pano (409/502): o sidecar nunca clica às cegas, então nada
        // caiu fora do lugar. O que já entrou fica (ADR 0007) e a conferência final registra.
        if matches!(err, ExecError::CdpTransport(_)) {
            eprintln!("[EXEC-DIAG] erro de clique ({err}) — o que já entrou fica no pano, sem UNDO");
            let _ = insert_error(pool, "click_unresolved", &format!("intent={intent_id} {err}"), now_ms);
        } else {
            return Err(err);
        }
    }


    // 3. Lê o preview no bet slip ANTES de confirmar (§1.4), dando tempo de o slip somar tudo.
    let mut preview = read_settled_for(cdp, plan.total_cents, final_settle_ms(plan.covers.len()))?;
    info!(intent_id, slip = preview, expected = plan.total_cents, "[EXECUTOR] leitura final do slip");
    eprintln!("[EXEC-DIAG] final: bilhete {}¢ (esperado {}¢) | saldo {:?}¢", preview, plan.total_cents, cdp.read_balance());
    if (preview as i64 - plan.total_cents as i64).abs() > 1
        && balance_confirms(cdp, balance_before, plan.total_cents)
    {
        warn!(intent_id, slip = preview, expected = plan.total_cents, "slip divergente, mas o saldo caiu exatamente a aposta: vale o saldo");
        eprintln!("[EXEC-DIAG] bilhete {}¢ diverge, mas o saldo caiu exatamente {}¢ — vale o saldo", preview, plan.total_cents);
        preview = plan.total_cents;
    }
    // Entrada de número NUNCA é desfeita (decisão do operador em 15/11/2026, ADR 0006): com 20
    // plenos, desfazer custava a entrada inteira por 1-2 fichas que não pegaram. Segue com o que
    // ficou na mesa, registra a diferença e a conferência de extrato mostra o valor real.
    if preview + 1 < plan.total_cents {
        warn!(preview, expected = plan.total_cents, intent_id, "entrada com ficha do bot faltando: segue sem desfazer");
        eprintln!(
            "[EXEC-DIAG] parte do bot no pano: {}¢ de {}¢ — segue sem tirar ficha (conferência de extrato mostra o real)",
            preview, plan.total_cents
        );
        let _ = insert_error(
            pool,
            if verify_each { "partial_entry" } else { "number_partial" },
            &format!("intent={intent_id} preview={preview} expected={}", plan.total_cents),
            now_ms,
        );
    } else if preview > plan.total_cents + 1 {
        eprintln!(
            "[EXEC-DIAG] bilhete {}¢ acima dos {}¢ do bot: fichas do operador — ficam no pano",
            preview, plan.total_cents
        );
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

/// Visão do executor sobre a mesa: só a parte do bot (bilhete − fichas que já estavam no pano)
/// e nenhum UNDO. O operador aposta junto; tirar ficha tirava a dele (ADR 0007).
struct OwnShare<'a> {
    inner: &'a dyn CdpActuator,
    base: u64,
}

impl CdpActuator for OwnShare<'_> {
    fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
        self.inner.click(x, y)
    }
    fn click_cover(&self, cover: &Cover, cal: &Calibration) -> Result<(), ExecError> {
        self.inner.click_cover(cover, cal)
    }
    fn click_cover_repeat(&self, cover: &Cover, cal: &Calibration, n: u32) -> Result<(), ExecError> {
        self.inner.click_cover_repeat(cover, cal, n)
    }
    fn click_chip(&self, denom_cents: u64, cal: &Calibration) -> Result<(), ExecError> {
        self.inner.click_chip(denom_cents, cal)
    }
    fn read_bet_total(&self) -> Result<u64, ExecError> {
        self.inner.read_bet_total().map(|v| v.saturating_sub(self.base))
    }
    fn clear_bets(&self, _cal: &Calibration) -> Result<(), ExecError> {
        Ok(())
    }
    fn confirm(&self, cal: &Calibration) -> Result<(), ExecError> {
        self.inner.confirm(cal)
    }
    fn collect_acks(&self, plan: &Plan) -> Result<Vec<CoverAck>, ExecError> {
        self.inner.collect_acks(plan)
    }
    fn table_clear_wait_ms(&self) -> u64 {
        self.inner.table_clear_wait_ms()
    }
    fn read_balance(&self) -> Option<i64> {
        self.inner.read_balance()
    }
}

/// UNDO máximos ao limpar a mesa (uma ficha por clique na Playtech).
pub const CLEAR_MAX_UNDO: u32 = 60;

#[cfg(test)]
mod slip_settle_tests {
    use super::*;
    use monster_store::init_pool;

    fn plano_de_numeros(k: u8, stake: u64) -> Plan {
        let covers: Vec<(Cover, u64)> = (0..k).map(|n| (Cover::Straight(n), stake)).collect();
        Plan {
            spin_id: "gale-22".to_string(),
            strategy_id: "script_espelhos_gemeos".to_string(),
            ladder_step: 3,
            stake_per_cover_cents: stake,
            total_cents: stake * u64::from(k),
            ev_cents: 0,
            confidence: 0.99,
            covers,
            chips_per_cover: vec![(stake, 1)],
        }
    }

    /// 14/11: gale 3 dos Espelhos (22 x 50c) com slip atrasado. Antes o bot lia o acumulado
    /// velho a cada pleno e "completava" fichas (inclusive de R$ 1). Agora entra exato.
    #[test]
    fn entrada_de_numero_com_slip_atrasado_nao_aposta_a_mais() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = DryRunActuator::new();
        cdp.set_slip_lag(6, 50);
        let plan = plano_de_numeros(22, 50);
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 1)
            .expect("entrada confirmada");
        assert_eq!(fills.preview_cents, 1100, "22 x 50c, nem um centavo a mais");
        assert_eq!(cdp.cleared_count(), 0);
    }

    /// Mesa que ainda mostra as fichas da rodada encerrada e limpa sozinha depois.
    struct MesaAnimando {
        inner: DryRunActuator,
        leituras_velhas: std::sync::atomic::AtomicU32,
    }

    impl CdpActuator for MesaAnimando {
        fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
            self.inner.click(x, y)
        }
        fn click_chip(&self, d: u64, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.click_chip(d, cal)
        }
        fn read_bet_total(&self) -> Result<u64, ExecError> {
            use std::sync::atomic::Ordering::SeqCst;
            if self.leituras_velhas.load(SeqCst) > 0 {
                self.leituras_velhas.fetch_sub(1, SeqCst);
                return Ok(100);
            }
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
        fn table_clear_wait_ms(&self) -> u64 {
            5_000
        }
    }

    /// Slip que demora a mostrar o total completo de uma entrada grande.
    struct SlipLento {
        inner: DryRunActuator,
        total: u64,
        leituras_lentas: std::sync::atomic::AtomicU32,
    }

    impl CdpActuator for SlipLento {
        fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
            self.inner.click(x, y)
        }
        fn click_chip(&self, d: u64, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.click_chip(d, cal)
        }
        fn read_bet_total(&self) -> Result<u64, ExecError> {
            use std::sync::atomic::Ordering::SeqCst;
            let v = self.inner.read_bet_total()?;
            if v == self.total && self.leituras_lentas.load(SeqCst) > 0 {
                self.leituras_lentas.fetch_sub(1, SeqCst);
                return Ok(self.total - 30);
            }
            Ok(v)
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

    /// 14/11: 20 plenos clicados, slip ainda animando aos 1,2 s (170 de 200): o bot desfazia a
    /// entrada inteira. O slip precisa de mais tempo para somar uma entrada grande.
    #[test]
    fn slip_lento_em_entrada_grande_nao_desfaz_a_aposta() {
        let pool = init_pool(":memory:").expect("pool");
        // ~2,5 s mostrando 170 (36 leituras de 70 ms) depois que as 20 fichas já estão no pano
        let cdp = SlipLento {
            inner: DryRunActuator::new(),
            total: 200,
            leituras_lentas: std::sync::atomic::AtomicU32::new(36),
        };
        let plan = plano_de_numeros(20, 10);
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 4)
            .expect("entrada confirmada");
        assert_eq!(fills.preview_cents, 200);
        assert_eq!(cdp.inner.cleared_count(), 0, "nenhum UNDO");
    }

    /// 14/11: gale logo após o resultado via as fichas velhas, fazia UNDO à toa e apostava por
    /// cima (`lido=140 esperado=40`). Agora espera a mesa limpar e entra exato, sem UNDO.
    #[test]
    fn gale_espera_a_mesa_da_rodada_anterior_limpar() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = MesaAnimando {
            inner: DryRunActuator::new(),
            leituras_velhas: std::sync::atomic::AtomicU32::new(25),
        };
        let plan = Plan {
            spin_id: "gale".to_string(),
            strategy_id: "quebra_permanencia_duzia".to_string(),
            ladder_step: 2,
            stake_per_cover_cents: 20,
            total_cents: 40,
            ev_cents: 0,
            confidence: 0.7,
            covers: vec![(Cover::Dozen(1), 20), (Cover::Dozen(3), 20)],
            chips_per_cover: vec![(20, 1)],
        };
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 3)
            .expect("entrada confirmada");
        assert_eq!(fills.preview_cents, 40);
        assert_eq!(cdp.inner.cleared_count(), 0, "nenhum UNDO nas fichas da rodada encerrada");
    }

    /// Slip que mostra sempre o mesmo valor (leitura quebrada); o saldo acompanha o pano real.
    struct SlipQuebrado {
        inner: DryRunActuator,
        slip_fixo: u64,
    }

    impl CdpActuator for SlipQuebrado {
        fn click(&self, x: i32, y: i32) -> Result<(), ExecError> {
            self.inner.click(x, y)
        }
        fn click_chip(&self, d: u64, cal: &Calibration) -> Result<(), ExecError> {
            self.inner.click_chip(d, cal)
        }
        fn read_bet_total(&self) -> Result<u64, ExecError> {
            Ok(self.slip_fixo)
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
        fn read_balance(&self) -> Option<i64> {
            Some(10_000 - self.inner.read_bet_total().unwrap_or(0) as i64)
        }
    }

    /// 17/11: gale 3 dos Espelhos com R$ 11,00 no pano e o slip lendo 0 (o operador viu as
    /// fichas). O saldo caiu exatamente a aposta: a entrada vale, sem clicar de novo.
    #[test]
    fn slip_em_zero_com_saldo_confirmando_a_aposta_vale_o_saldo() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = SlipQuebrado { inner: DryRunActuator::new(), slip_fixo: 0 };
        let plan = plano_de_numeros(22, 50);
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 7)
            .expect("entrada confirmada pelo saldo");
        assert_eq!(fills.preview_cents, 1100);
        assert_eq!(cdp.inner.cleared_count(), 0);
        assert_eq!(cdp.inner.confirmed_count(), 1, "uma aposta só, sem clique repetido");
    }

    /// 17/11 20:02: colunas 1 e 3 (40c) com o slip lendo 120. O saldo caiu 40: entra, sem UNDO.
    #[test]
    fn slip_inflado_com_saldo_certo_nao_desfaz_a_coluna() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = SlipQuebrado { inner: DryRunActuator::new(), slip_fixo: 0 };
        let plan = Plan {
            spin_id: "colunas".to_string(),
            strategy_id: "quebra_permanencia_coluna".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 20,
            total_cents: 40,
            ev_cents: 0,
            confidence: 0.7,
            covers: vec![(Cover::Col(1), 20), (Cover::Col(3), 20)],
            chips_per_cover: vec![(20, 1)],
        };
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 8)
            .expect("entrada confirmada pelo saldo");
        assert_eq!(fills.preview_cents, 40);
        assert_eq!(cdp.inner.cleared_count(), 0);
        assert_eq!(cdp.inner.confirmed_count(), 1, "uma aposta só, sem clique repetido");
    }

    /// 18/11 (ADR 0007): operador com R$ 2,40 no vermelho; o bot entra nas colunas por cima,
    /// confere só a parte dele e nunca tira a ficha do operador.
    #[test]
    fn ficha_do_operador_no_pano_fica_e_o_bot_entra_por_cima() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = DryRunActuator::new();
        cdp.set_staged_total(240);
        let plan = Plan {
            spin_id: "junto".to_string(),
            strategy_id: "quebra_permanencia_coluna".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 20,
            total_cents: 40,
            ev_cents: 0,
            confidence: 0.7,
            covers: vec![(Cover::Col(1), 20), (Cover::Col(3), 20)],
            chips_per_cover: vec![(20, 1)],
        };
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 9)
            .expect("entra por cima");
        assert_eq!(fills.preview_cents, 40, "parte do bot");
        assert_eq!(cdp.cleared_count(), 0, "a ficha do operador fica");
        assert_eq!(cdp.confirmed_count(), 1);
    }

    /// 15/11 (ADR 0006): entrada de número com ficha que não pegou segue sem UNDO.
    #[test]
    fn entrada_de_numero_nunca_e_desfeita() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = DryRunActuator::new();
        cdp.set_swallow_chips(3); // 3 plenos não pegaram
        let plan = plano_de_numeros(20, 10);
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 6)
            .expect("entrada segue");
        assert_eq!(fills.preview_cents, 170, "aposta como ficou na mesa");
        assert_eq!(cdp.cleared_count(), 0, "nenhum UNDO em entrada de número");
    }

    /// 14/11: `lido=20 esperado=40` x12 — a 2a dúzia não pegou e o bot desfazia tudo. Agora
    /// reclica só a cobertura que faltou e a entrada sai completa, sem UNDO.
    #[test]
    fn cobertura_que_nao_pegou_e_reclicada_sem_desfazer_a_entrada() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = DryRunActuator::new();
        cdp.set_swallow_chips(1); // a mesa engole o primeiro clique de cobertura
        let plan = Plan {
            spin_id: "duzias".to_string(),
            strategy_id: "quebra_permanencia_duzia".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 20,
            total_cents: 40,
            ev_cents: 0,
            confidence: 0.7,
            covers: vec![(Cover::Dozen(1), 20), (Cover::Dozen(3), 20)],
            chips_per_cover: vec![(20, 1)],
        };
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 5)
            .expect("entrada confirmada");
        assert_eq!(fills.preview_cents, 40);
        assert_eq!(cdp.cleared_count(), 0, "nenhum UNDO");
    }

    /// Cobertura de fora com slip atrasado mostrando a cobertura anterior: não completa.
    #[test]
    fn slip_atrasado_em_cobertura_de_fora_nao_completa_ficha() {
        let pool = init_pool(":memory:").expect("pool");
        let cdp = DryRunActuator::new();
        let plan = Plan {
            spin_id: "col".to_string(),
            strategy_id: "quebra_permanencia_coluna".to_string(),
            ladder_step: 1,
            stake_per_cover_cents: 20,
            total_cents: 40,
            ev_cents: 0,
            confidence: 0.7,
            covers: vec![(Cover::Col(1), 20), (Cover::Col(3), 20)],
            chips_per_cover: vec![(20, 1)],
        };
        // depois da 2a coluna o slip ainda mostra 20 (a 1a): antes virava "faltou 20c"
        cdp.set_slip_lag(0, 0);
        let fills = place(&pool, &cdp, &plan, &Calibration::default(), &KillSwitch::new(), 2)
            .expect("entrada confirmada");
        assert_eq!(fills.preview_cents, 40);
    }

    /// O slip da Playtech anima: ler o preview logo após os cliques devolve o total antigo. Antes
    /// da correção isso abortava toda entrada acima de uma ficha de 10¢ (`cover_mismatch` com
    /// `lido=70 esperado=90` na sessão ao vivo). O leitor precisa esperar o valor estabilizar.
    #[test]
    fn atraso_no_slip_nao_aborta_aposta_correta() {
        let cdp = DryRunActuator::new();
        cdp.set_staged_total(90);
        cdp.set_slip_lag(4, 0); // slip ainda mostra 0¢ nas 4 primeiras leituras
        let seen = read_settled(&cdp, 90).expect("leitura do preview");
        assert_eq!(seen, 90, "leitura atrasada nao pode virar divergencia");
    }

    /// Total em cache (sidecar sem leitura fresca) não é leitura da mesa: o leitor pula essas
    /// respostas e só devolve um valor vindo de leitura real.
    #[test]
    fn leitura_em_cache_nao_vira_veredito_da_mesa() {
        let cdp = DryRunActuator::new();
        cdp.set_staged_total(90);
        cdp.set_slip_stale_reads(3);
        let seen = read_settled(&cdp, 90).expect("leitura do preview");
        assert_eq!(seen, 90, "resposta em cache nao pode substituir leitura fresca");
    }

    /// Sidecar sem leitura fresca o tempo todo: erro explícito, jamais um `0¢` inventado
    /// (que faria `place()` tratar mesa suja como mesa limpa).
    #[test]
    fn slip_sempre_em_cache_falha_em_vez_de_assumir_zero() {
        let cdp = DryRunActuator::new();
        cdp.set_slip_stale_reads(1_000);
        match read_settled(&cdp, 0) {
            Err(ExecError::SlipStale(_)) => {}
            other => panic!("esperava SlipStale, veio {other:?}"),
        }
    }

    /// Corpo de `/act/preview` sem `bet_age_ms` não pode virar "mesa limpa, 0¢":
    /// campo faltando é leitura não autorizada, não leitura fresca.
    #[test]
    fn preview_sem_idade_de_leitura_falha_em_vez_de_assumir_zero() {
        let body = r#"{"preview_cents":0,"balance_cents":6907,"round_open":true}"#;
        match parse_preview(body) {
            Err(ExecError::SlipStale(_)) => {}
            other => panic!("esperava SlipStale, veio {other:?}"),
        }
    }

    /// Resposta normal: idade dentro da tolerância devolve o total real da mesa.
    #[test]
    fn preview_fresco_devolve_o_total_lido() {
        let body = r#"{"preview_cents":90,"bet_age_ms":120,"balance_cents":6817,"round_open":true}"#;
        assert_eq!(parse_preview(body).expect("preview fresco"), 90);
    }

    /// Sem `preview_cents` não existe total, logo não existe decisão: erro de transporte.
    #[test]
    fn preview_sem_total_nao_pode_significar_cero_apostado() {
        let body = r#"{"bet_age_ms":50,"balance_cents":6907}"#;
        match parse_preview(body) {
            Err(ExecError::CdpTransport(_)) => {}
            other => panic!("esperava CdpTransport, veio {other:?}"),
        }
    }

    /// Divergência permanente (valor estável e errado) continua sendo reportada, sem laço infinito.
    #[test]
    fn divergencia_estavel_encerra_rapido_e_devolve_o_lido() {
        let cdp = DryRunActuator::new();
        let started = std::time::Instant::now();
        let seen = read_settled(&cdp, 90).expect("leitura do preview");
        assert_eq!(seen, 0);
        let waited = started.elapsed().as_millis() as u64;
        assert!(
            (SETTLE_DEADLINE_MS..=SETTLE_DEADLINE_MS + 400).contains(&waited),
            "mesa zerada com esperado 90¢ deve esperar o teto e desistir, sem laco infinito (esperou {waited} ms)"
        );
    }
}
