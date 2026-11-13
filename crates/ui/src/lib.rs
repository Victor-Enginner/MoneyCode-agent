// SPDX-License-Identifier: Apache-2.0
//! Camada de comandos IPC Tauri, máquina de 6 estados da FUI, tray e emissor `bus://tick` (4 Hz).
//! Implementa os 9 comandos de leitura somente-leitura no SQLite e os 5 comandos de escrita (§7 de `docs/FUI.md`).
//! Garante que nenhum campo sem fonte retorne `0` (sempre `None` / `"—"`).

use monster_risk::{cum_loss_cents, stake_cents, Cfg};
use monster_store::{get_ladder_state, SqlitePool, StoreError};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tracing::{info, warn};

/// Intervalo de lote do evento `bus://tick` a 4 Hz (250 ms — §7).
pub const BUS_TICK_INTERVAL_MS: u64 = 250;

/// Atalho global imutável para o Kill Switch (§7 e §8).
pub const GLOBAL_KILL_SHORTCUT: &str = "Ctrl+Alt+K";

/// Tópico do evento de lote emitido a 4 Hz para a WebView Tauri (§7).
pub const BUS_TICK_EVENT_TOPIC: &str = "bus://tick";

/// Os 6 estados globais obrigatórios da FUI (`docs/FUI.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GlobalState {
    /// Captura e DAG ativos aguardando gatilho.
    Observando,
    /// Entrada autorizada com countdown da janela ativo.
    Entrada,
    /// Captura viva, apostas suspensas pelo operador.
    Pausado,
    /// Uma das fontes de captura caiu (banner âmbar 28px).
    Degraded,
    /// Erro operacional requerendo atenção (banner vermelho 28px).
    Erro,
    /// Kill switch acionado (só `/resume` ou restart restaura).
    Killed,
}

/// As 4 cores de ícone da bandeja do sistema (tray — §6 e §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayColor {
    /// Normal (`OBSERVANDO`).
    Green,
    /// Atenção (`ENTRADA` ou `DEGRADED`).
    Amber,
    /// Pausado (`PAUSADO`).
    Gray,
    /// Crítico (`ERRO` ou `KILLED`).
    Red,
}

/// Itens do menu de contexto do tray (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayMenuAction {
    /// Exibe e foca a janela principal.
    Mostrar,
    /// Pausa novas entradas mantendo a captura viva.
    Pausar,
    /// Aciona o Kill Switch imediato.
    Kill,
    /// Encerra o processo explicitamente pelo menu.
    Sair,
}

/// Comportamento ao fechar a janela principal no X (§7: `close` esconde, não mata o processo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseDisposition {
    /// Esconde a janela mantendo o processo e a captura vivos em background.
    HideWindowKeepAlive,
    /// Encerra o processo (apenas quando acionado via `TrayMenuAction::Sair`).
    ExitProcess,
}

impl GlobalState {
    /// Cor de estado associada na barra superior (§6).
    pub const fn bar_token(self) -> &'static str {
        match self {
            Self::Observando => "--ok",
            Self::Entrada | Self::Degraded => "--warn",
            Self::Pausado => "--muted",
            Self::Erro | Self::Killed => "--bad",
        }
    }

    /// Cor correspondente do ícone no system tray (§6).
    pub const fn tray_color(self) -> TrayColor {
        match self {
            Self::Observando => TrayColor::Green,
            Self::Entrada | Self::Degraded => TrayColor::Amber,
            Self::Pausado => TrayColor::Gray,
            Self::Erro | Self::Killed => TrayColor::Red,
        }
    }

    /// Glifo obrigatório para nunca depender apenas de cor (§3 e §9).
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Observando => "●",
            Self::Entrada => "◈",
            Self::Pausado => "⏸",
            Self::Degraded => "⚠",
            Self::Erro => "✖",
            Self::Killed => "■",
        }
    }

    /// Indica se a faixa de banner de 28px abaixo da barra A deve ficar visível (§6).
    pub const fn requires_banner(self) -> bool {
        matches!(self, Self::Degraded | Self::Erro | Self::Killed)
    }
}

/// Estado de cada um dos 4 LEDs da barra A (`WS`, `MO`, `EXEC`, `DB` — §5.A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedStatus {
    /// Saudável (verde `--ok`).
    Ok,
    /// Degradado (âmbar `--warn`).
    Warn,
    /// Morto (vermelho piscando `--bad`).
    Bad,
    /// Desligado (cinza `--dim`).
    Off,
}

/// Erros tipados da camada UI/IPC (`thiserror`, zero `unwrap()`).
#[derive(Debug, Error)]
pub enum UiError {
    /// Erro de leitura no SQLite.
    #[error("erro de leitura no SQLite: {0}")]
    Store(#[from] StoreError),
    /// Erro de lock interno.
    #[error("estado da UI indisponível")]
    StateLock,
    /// Parâmetro inválido em comando de escrita.
    #[error("parâmetro inválido: {0}")]
    InvalidParam(String),
}

/// DTO do painel A / C / D (`get_session`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDto {
    /// Estado global atual da máquina.
    pub state: GlobalState,
    /// Segundos de sessão ativa (`None` se não iniciada).
    pub uptime_sec: Option<u64>,
    /// Banca atual em centavos inteiros (`None` -> `"—"`).
    pub bankroll_cents: Option<i64>,
    /// P&L acumulado da sessão em centavos inteiros (`None` -> `"—"`).
    pub pnl_cents: Option<i64>,
    /// Aposta atualmente aceita na mesa em centavos (`Bet R$...` da Playtech; `None` -> `"—"`).
    pub table_bet_cents: Option<u64>,
    /// Último ganho pago pela mesa em centavos (`Last Win R$...`; `None` -> `"—"`).
    pub last_win_cents: Option<u64>,
    /// Pontos da curva P&L da sessão em centavos inteiros.
    pub pnl_history_cents: Vec<i64>,
    /// Rótulo da cobertura ativa (ex.: `"COL 2+3 · R$ 0,20"` ou `None` -> `"—"`).
    pub active_cover_label: Option<String>,
    /// Números cobertos na entrada aberta (`0..=36`). Vem do plano já expandido: uma aposta do
    /// cluster 1-4-7+0 chega aqui como 15 plenos, então o overlay do pano é sempre por número.
    #[serde(default)]
    pub cover_numbers: Vec<u8>,
    /// Exposição total aberta na rodada em centavos inteiros (`None` -> `"—"`).
    pub exposure_cents: Option<u64>,
    /// Janela restante em milissegundos (`None` -> `"—"`).
    pub remaining_window_ms: Option<u32>,
    /// Texto do banner de 28px quando em `DEGRADED` / `ERRO` / `KILLED`.
    pub banner_detail: Option<String>,
}

/// DTO de saúde das 4 fontes (`get_health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthDto {
    /// LED do WebSocket protobuf.
    pub ws: LedStatus,
    /// LED do MutationObserver no OOPIF.
    pub mo: LedStatus,
    /// LED do Executor.
    pub exec: LedStatus,
    /// LED do banco SQLite WAL.
    pub db: LedStatus,
    /// Nome da fonte morta caso em `DEGRADED`.
    pub dead_source: Option<String>,
}

/// Item da timeline de 20 números (`get_spins` — §5.B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpinItemDto {
    /// Identificador único do spin.
    pub spin_id: String,
    /// Número sorteado (`0..=36`).
    pub number: u8,
    /// Coluna real `2 to 1` (`0` para zero, `1..=3` para `(n-1)%3 + 1`).
    pub column: u8,
    /// Dúzia real `1st/2nd/3rd 12` (`0` para zero, `1..=3` para `(n-1)/12 + 1`).
    pub dozen: u8,
    /// Timestamp em milissegundos.
    pub ts: i64,
    /// Fonte (`ws`, `mo`, `ext`, `ocr`).
    pub source: String,
    /// `true` se este spin disparou um matcher no DAG (borda ciano à esquerda).
    pub triggered_matcher: bool,
}

/// Linha da tabela da escada no Painel D (`get_ladder` — §5.D).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderRowDto {
    /// Degrau (`1..=max_steps`).
    pub step: u8,
    /// Valor por cobertura em centavos inteiros.
    pub stake_per_cover_cents: u64,
    /// Exposição em cobertura dupla (`2 * stake_per_cover_cents`).
    pub exposure_cents: u64,
    /// Perda acumulada até este degrau em centavos inteiros.
    pub cum_loss_cents: u64,
    /// `true` se for o degrau ativo no momento (`▸`).
    pub active: bool,
}

/// Estado completo da escada (`get_ladder`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderStateDto {
    /// Degrau atual (`None` se sem fonte).
    pub current_step: Option<u8>,
    /// Total de degraus configurados.
    pub max_steps: Option<u8>,
    /// Custo total do estouro da escada em centavos inteiros.
    pub blow_cost_cents: Option<u64>,
    /// Linhas de cada degrau.
    pub rows: Vec<LadderRowDto>,
}

/// Item da aba DECISÕES (`get_decisions` — §5.E).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionItemDto {
    /// ID da intenção no SQLite.
    pub id: i64,
    /// Spin associado.
    pub spin_id: String,
    /// Timestamp de criação.
    pub ts: i64,
    /// Cartão / estratégia que gerou a decisão.
    pub strategy_id: String,
    /// Confiança estatística (`0.0..=1.0`).
    pub confidence: Option<f64>,
    /// Regime classificado (`Permanence`, `Momentum`, `Neutral`).
    pub regime: Option<String>,
    /// Estatística chi² opcional.
    pub chi_square: Option<f64>,
    /// Decisão tomada (`placed`, `skipped`, `aborted`, `orphan`).
    pub decision: String,
    /// Motivo explícito da decisão.
    pub reason: Option<String>,
}

/// Item da aba FILLS (`get_fills` — §5.E).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillItemDto {
    /// ID do fill.
    pub id: i64,
    /// FK para `intents.id`.
    pub intent_id: i64,
    /// Cobertura (`col:2`, `col:3`, etc.).
    pub cover: String,
    /// Valor apostado em centavos inteiros.
    pub stake_cents: u64,
    /// Indicador de confirmação pela mesa.
    pub acked: bool,
    /// Latência observada em milissegundos (`None` -> `"—"`).
    pub latency_ms: Option<i64>,
}

/// Item da aba ÓRFÃOS (`get_orphans` — §5.E).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanItemDto {
    /// ID da intent órfã.
    pub intent_id: i64,
    /// Spin associado.
    pub spin_id: String,
    /// Estratégia.
    pub strategy_id: String,
    /// Valor em centavos inteiros.
    pub stake_cents: u64,
    /// Estado da reconciliação.
    pub reconcile_status: String,
}

/// Item da aba ERROS (`get_errors` — §5.E).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorItemDto {
    /// ID do erro.
    pub id: i64,
    /// Categoria (`kind`).
    pub kind: String,
    /// Detalhe do erro.
    pub detail: String,
    /// Timestamp em milissegundos.
    pub ts: i64,
}

/// Payload emitido a 4 Hz no tópico `bus://tick` (§7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusTickPayload {
    /// Sequência monotônica do tick.
    pub seq: u64,
    /// Estado de sessão.
    pub session: SessionDto,
    /// Saúde dos 4 LEDs.
    pub health: HealthDto,
    /// Últimos 20 spins.
    pub spins: Vec<SpinItemDto>,
    /// Tabela e degrau da escada.
    pub ladder: LadderStateDto,
    /// Últimas decisões.
    pub decisions: Vec<DecisionItemDto>,
    /// Últimos fills.
    pub fills: Vec<FillItemDto>,
    /// Intents órfãs.
    pub orphans: Vec<OrphanItemDto>,
    /// Últimos 50 erros.
    pub errors: Vec<ErrorItemDto>,
}

/// Formata centavos opcionais garantindo que `None` retorne `"—"` e nunca `"0"` (§2.2 e §9).
pub fn format_optional_cents(cents: Option<i64>) -> String {
    let Some(val) = cents else {
        return "—".to_string();
    };
    let sign = if val < 0 { "-" } else { "" };
    let abs = val.unsigned_abs();
    format!("{sign}R$ {},{:02}", abs / 100, abs % 100)
}

#[derive(Debug)]
struct RuntimeUiState {
    session: SessionDto,
    health: HealthDto,
    cfg: Cfg,
}

/// Controlador de estado e comandos IPC do Tauri (§7).
#[derive(Clone)]
pub struct FuiController {
    inner: Arc<Mutex<RuntimeUiState>>,
    tick_seq: Arc<AtomicU64>,
}

impl Default for FuiController {
    fn default() -> Self {
        Self::new(Cfg {
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
        })
    }
}

impl FuiController {
    /// Cria um novo controlador FUI com a configuração de risco informada.
    pub fn new(cfg: Cfg) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RuntimeUiState {
                session: SessionDto {
                    state: GlobalState::Observando,
                    uptime_sec: None,
                    bankroll_cents: None,
                    pnl_cents: None,
                    table_bet_cents: None,
                    last_win_cents: None,
                    pnl_history_cents: Vec::new(),
                    active_cover_label: None,
                    cover_numbers: Vec::new(),
                    exposure_cents: None,
                    remaining_window_ms: None,
                    banner_detail: None,
                },
                health: HealthDto {
                    ws: LedStatus::Ok,
                    mo: LedStatus::Ok,
                    exec: LedStatus::Ok,
                    db: LedStatus::Ok,
                    dead_source: None,
                },
                cfg,
            })),
            tick_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Trata o evento de fechamento da janela principal (§7: `close` esconde a janela sem matar o processo).
    pub fn on_window_close_requested(&self) -> CloseDisposition {
        CloseDisposition::HideWindowKeepAlive
    }

    /// Atualiza banca, P&L, aposta na mesa e último ganho com os valores lidos do DOM da Playtech.
    pub fn sync_table_telemetry(
        &self,
        bankroll_cents: Option<i64>,
        pnl_cents: Option<i64>,
        table_bet_cents: Option<u64>,
        last_win_cents: Option<u64>,
    ) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(b) = bankroll_cents {
                g.session.bankroll_cents = Some(b);
            }
            if let Some(p) = pnl_cents {
                g.session.pnl_cents = Some(p);
            }
            if let Some(bet) = table_bet_cents {
                g.session.table_bet_cents = Some(bet);
            }
            if let Some(win) = last_win_cents {
                g.session.last_win_cents = Some(win);
            }
        }
    }

    /// Sincroniza métricas vivas do `monster-core` (banca, P&L, relógio de sessão e cobertura ativa) com a FUI.
    pub fn sync_live_metrics(
        &self,
        uptime_sec: u64,
        bankroll_cents: Option<i64>,
        pnl_cents: Option<i64>,
        active_cover_label: Option<String>,
        exposure_cents: Option<u64>,
    ) {
        if let Ok(mut g) = self.inner.lock() {
            g.session.uptime_sec = Some(uptime_sec);
            if bankroll_cents.is_some() {
                g.session.bankroll_cents = bankroll_cents;
            }
            if let Some(pnl) = pnl_cents {
                g.session.pnl_cents = Some(pnl);
                g.session.pnl_history_cents.push(pnl);
                if g.session.pnl_history_cents.len() > 60 {
                    let drop_n = g.session.pnl_history_cents.len() - 60;
                    g.session.pnl_history_cents.drain(0..drop_n);
                }
            }
            g.session.active_cover_label = active_cover_label;
            g.session.exposure_cents = exposure_cents;
            if g.session.state != GlobalState::Killed && g.session.state != GlobalState::Pausado {
                g.session.state = if exposure_cents.unwrap_or(0) > 0 {
                    GlobalState::Entrada
                } else {
                    GlobalState::Observando
                };
            }
        }
    }

    /// Publica os números cobertos pela entrada aberta (overlay do pano por número).
    ///
    /// Lista vazia limpa o overlay — é o que acontece quando a entrada é reconciliada.
    pub fn set_cover_numbers(&self, numbers: Vec<u8>) {
        if let Ok(mut g) = self.inner.lock() {
            g.session.cover_numbers = numbers;
        }
    }

    /// Atualiza o relógio de sessão (`uptime_sec`) a cada tick da FUI.
    pub fn touch_uptime(&self, uptime_sec: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.session.uptime_sec = Some(uptime_sec);
        }
    }

    /// Comando IPC de leitura: `get_session` (§7).
    pub fn get_session(&self) -> Result<SessionDto, UiError> {
        let g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        Ok(g.session.clone())
    }

    /// Comando IPC de leitura: `get_health` (§7).
    pub fn get_health(&self) -> Result<HealthDto, UiError> {
        let g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        Ok(g.health.clone())
    }

    /// Comando IPC de leitura: `get_cfg` (§7).
    pub fn get_cfg(&self) -> Result<Cfg, UiError> {
        let g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        Ok(g.cfg.clone())
    }

    /// Comando IPC de leitura: `get_spins` — lê os 20 últimos spins do SQLite, que é a fonte de verdade
    /// sincronizada com a fita de números da Playtech por `timeline_sync` (§5.B e §7).
    pub fn get_spins(&self, pool: &SqlitePool) -> Result<Vec<SpinItemDto>, UiError> {
        let list = pool
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT s.spin_id, s.number, s.ts, s.source,
                            EXISTS(SELECT 1 FROM intents i WHERE i.spin_id = s.spin_id) AS triggered
                     FROM spins s
                     ORDER BY s.ts DESC, s.id DESC
                     LIMIT 20",
                )?;
                let rows = stmt.query_map([], |r| {
                    let number_i64: i64 = r.get(1)?;
                    let number = u8::try_from(number_i64.clamp(0, 36)).unwrap_or(0);
                    let column = if number == 0 {
                        0
                    } else {
                        (number - 1) % 3 + 1
                    };
                    let dozen = if number == 0 {
                        0
                    } else {
                        (number - 1) / 12 + 1
                    };
                    let triggered_int: i64 = r.get(4)?;
                    Ok(SpinItemDto {
                        spin_id: r.get(0)?,
                        number,
                        column,
                        dozen,
                        ts: r.get(2)?,
                        source: r.get(3)?,
                        triggered_matcher: triggered_int != 0,
                    })
                })?;
                let mut out = Vec::with_capacity(20);
                for item in rows {
                    out.push(item?);
                }
                Ok(out)
            })
            .map_err(|e| UiError::Store(StoreError::Database(e)))?;

        Ok(list)
    }

    /// Comando IPC de leitura: `get_ladder` — monta a tabela da escada ×3 a partir de `ladder_state` e `Cfg` (§5.D e §7).
    pub fn get_ladder(
        &self,
        pool: &SqlitePool,
        strategy_id: &str,
    ) -> Result<LadderStateDto, UiError> {
        let cfg = self.get_cfg()?;
        let row = get_ladder_state(pool, strategy_id)?;
        let active_step = row.step;
        let mut rows = Vec::with_capacity(cfg.max_steps as usize);
        for s in 1..=cfg.max_steps {
            let stake = stake_cents(s, cfg.base_cents);
            rows.push(LadderRowDto {
                step: s,
                stake_per_cover_cents: stake,
                exposure_cents: 2 * stake,
                cum_loss_cents: cum_loss_cents(s, cfg.base_cents),
                active: active_step == s,
            });
        }
        Ok(LadderStateDto {
            current_step: Some(active_step),
            max_steps: Some(cfg.max_steps),
            blow_cost_cents: Some(cum_loss_cents(cfg.max_steps, cfg.base_cents)),
            rows,
        })
    }

    /// Comando IPC de leitura: `get_decisions` (§5.E e §7).
    pub fn get_decisions(&self, pool: &SqlitePool) -> Result<Vec<DecisionItemDto>, UiError> {
        pool.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, spin_id, created_at, strategy_id, confidence, decision, reason
                 FROM intents
                 ORDER BY created_at DESC, id DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DecisionItemDto {
                    id: r.get(0)?,
                    spin_id: r.get(1)?,
                    ts: r.get(2)?,
                    strategy_id: r.get(3)?,
                    confidence: Some(r.get(4)?),
                    regime: None,
                    chi_square: None,
                    decision: r.get(5)?,
                    reason: r.get(6)?,
                })
            })?;
            let mut list = Vec::new();
            for item in rows {
                list.push(item?);
            }
            Ok(list)
        })
        .map_err(|e| UiError::Store(StoreError::Database(e)))
    }

    /// Comando IPC de leitura: `get_fills` (§5.E e §7).
    pub fn get_fills(&self, pool: &SqlitePool) -> Result<Vec<FillItemDto>, UiError> {
        pool.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT f.id, f.intent_id, f.cover, f.stake_cents, f.acked,
                        CASE WHEN f.confirmed_at IS NOT NULL THEN (f.confirmed_at - i.created_at) ELSE NULL END
                 FROM fills f
                 LEFT JOIN intents i ON i.id = f.intent_id
                 ORDER BY f.id DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                let stake_i64: i64 = r.get(3)?;
                let acked_i64: i64 = r.get(4)?;
                Ok(FillItemDto {
                    id: r.get(0)?,
                    intent_id: r.get(1)?,
                    cover: r.get(2)?,
                    stake_cents: u64::try_from(stake_i64.max(0)).unwrap_or(0),
                    acked: acked_i64 != 0,
                    latency_ms: r.get(5)?,
                })
            })?;
            let mut list = Vec::new();
            for item in rows {
                list.push(item?);
            }
            Ok(list)
        })
        .map_err(|e| UiError::Store(StoreError::Database(e)))
    }

    /// Comando IPC de leitura: `get_orphans` (§5.E e §7).
    pub fn get_orphans(&self, pool: &SqlitePool) -> Result<Vec<OrphanItemDto>, UiError> {
        pool.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, spin_id, strategy_id, stake_cents, COALESCE(reason, 'pendente_extrato')
                 FROM intents
                 WHERE decision = 'orphan'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                let stake_i64: i64 = r.get(3)?;
                Ok(OrphanItemDto {
                    intent_id: r.get(0)?,
                    spin_id: r.get(1)?,
                    strategy_id: r.get(2)?,
                    stake_cents: u64::try_from(stake_i64.max(0)).unwrap_or(0),
                    reconcile_status: r.get(4)?,
                })
            })?;
            let mut list = Vec::new();
            for item in rows {
                list.push(item?);
            }
            Ok(list)
        })
        .map_err(|e| UiError::Store(StoreError::Database(e)))
    }

    /// Comando IPC de leitura: `get_errors` — últimos 50 de `errors` (§5.E e §7).
    pub fn get_errors(&self, pool: &SqlitePool) -> Result<Vec<ErrorItemDto>, UiError> {
        pool.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, COALESCE(detail, '—'), ts
                 FROM errors
                 ORDER BY ts DESC, id DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(ErrorItemDto {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    detail: r.get(2)?,
                    ts: r.get(3)?,
                })
            })?;
            let mut list = Vec::new();
            for item in rows {
                list.push(item?);
            }
            Ok(list)
        })
        .map_err(|e| UiError::Store(StoreError::Database(e)))
    }

    /// Comando IPC de escrita: `pause` (§7).
    pub fn pause(&self) -> Result<GlobalState, UiError> {
        let mut g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        if g.session.state != GlobalState::Killed {
            g.session.state = GlobalState::Pausado;
            g.health.exec = LedStatus::Off;
            info!("FUI alterada para PAUSADO");
        }
        Ok(g.session.state)
    }

    /// Comando IPC de escrita: `resume` (§7).
    pub fn resume(&self) -> Result<GlobalState, UiError> {
        let mut g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        g.session.state = GlobalState::Observando;
        g.session.banner_detail = None;
        g.health.exec = LedStatus::Ok;
        info!("FUI restaurada para OBSERVANDO via resume");
        Ok(g.session.state)
    }

    /// Comando IPC de escrita: `kill` (`Ctrl+Alt+K` / botão KILL / menu tray — §7).
    pub fn kill(&self) -> Result<GlobalState, UiError> {
        let mut g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        g.session.state = GlobalState::Killed;
        g.session.banner_detail =
            Some("KILL SWITCH ATIVO — executor desligado, captura viva".to_string());
        g.health.exec = LedStatus::Bad;
        warn!(shortcut = GLOBAL_KILL_SHORTCUT, "FUI entrou em estado KILLED");
        Ok(g.session.state)
    }

    /// Comando IPC de escrita: `set_base` (§7).
    pub fn set_base(&self, base_cents: u64) -> Result<Cfg, UiError> {
        if base_cents < 10 || base_cents % 10 != 0 {
            return Err(UiError::InvalidParam(
                "base_cents deve ser múltiplo de 10 >= 10".to_string(),
            ));
        }
        let mut g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        g.cfg.base_cents = base_cents;
        Ok(g.cfg.clone())
    }

    /// Comando IPC de escrita: `set_steps` (§7).
    pub fn set_steps(&self, max_steps: u8) -> Result<Cfg, UiError> {
        if !(1..=6).contains(&max_steps) {
            return Err(UiError::InvalidParam(
                "max_steps deve estar entre 1 e 6".to_string(),
            ));
        }
        let mut g = self.inner.lock().map_err(|_| UiError::StateLock)?;
        g.cfg.max_steps = max_steps;
        Ok(g.cfg.clone())
    }

    /// Monta o lote completo `bus://tick` a 4 Hz (§7).
    pub fn build_bus_tick(
        &self,
        pool: &SqlitePool,
        strategy_id: &str,
    ) -> Result<BusTickPayload, UiError> {
        let seq = self.tick_seq.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(BusTickPayload {
            seq,
            session: self.get_session()?,
            health: self.get_health()?,
            spins: self.get_spins(pool)?,
            ladder: self.get_ladder(pool, strategy_id)?,
            decisions: self.get_decisions(pool)?,
            fills: self.get_fills(pool)?,
            orphans: self.get_orphans(pool)?,
            errors: self.get_errors(pool)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monster_store::{init_pool, insert_spin, NewSpin, SpinSource};
    use std::time::Instant;

    #[test]
    fn six_states_have_distinct_glyphs_and_valid_tray_colors() {
        let all = [
            (GlobalState::Observando, TrayColor::Green, "●", false),
            (GlobalState::Entrada, TrayColor::Amber, "◈", false),
            (GlobalState::Pausado, TrayColor::Gray, "⏸", false),
            (GlobalState::Degraded, TrayColor::Amber, "⚠", true),
            (GlobalState::Erro, TrayColor::Red, "✖", true),
            (GlobalState::Killed, TrayColor::Red, "■", true),
        ];
        for (st, expected_tray, expected_glyph, expected_banner) in all {
            assert_eq!(st.tray_color(), expected_tray);
            assert_eq!(st.glyph(), expected_glyph);
            assert_eq!(st.requires_banner(), expected_banner);
        }
    }

    #[test]
    fn missing_data_formats_as_em_dash_never_zero() {
        assert_eq!(format_optional_cents(None), "—");
        assert_eq!(format_optional_cents(Some(7058)), "R$ 70,58");
        assert_eq!(format_optional_cents(Some(-1600)), "-R$ 16,00");
    }

    #[test]
    fn window_close_hides_and_kill_shortcut_is_registered() {
        let ctrl = FuiController::default();
        assert_eq!(
            ctrl.on_window_close_requested(),
            CloseDisposition::HideWindowKeepAlive
        );
        assert_eq!(GLOBAL_KILL_SHORTCUT, "Ctrl+Alt+K");

        assert_eq!(ctrl.pause().expect("pause"), GlobalState::Pausado);
        assert_eq!(ctrl.kill().expect("kill"), GlobalState::Killed);
        // Pausa não sobrescreve KILLED
        assert_eq!(ctrl.pause().expect("pause after kill"), GlobalState::Killed);
        // Só resume restaura de KILLED
        assert_eq!(ctrl.resume().expect("resume"), GlobalState::Observando);
    }

    #[test]
    fn bus_tick_with_500_spins_in_db_executes_under_16ms_budget() {
        let pool = init_pool(":memory:").expect("init in-memory sqlite");
        for i in 1..=500u32 {
            insert_spin(
                &pool,
                &NewSpin {
                    spin_id: format!("s-{i}"),
                    number: (i % 37) as u8,
                    ts: 1_730_000_000_000 + i64::from(i) * 1000,
                    source: SpinSource::Ws,
                    received_at: 1_730_000_000_020 + i64::from(i) * 1000,
                    latency_ms: Some(20),
                },
            )
            .expect("insert spin");
        }

        let ctrl = FuiController::default();
        let start = Instant::now();
        let tick = ctrl
            .build_bus_tick(&pool, "quebra_permanencia_coluna")
            .expect("build_bus_tick");
        let elapsed_ms = start.elapsed().as_millis();

        assert_eq!(tick.spins.len(), 20);
        assert_eq!(tick.ladder.rows.len(), 4);
        assert_eq!(tick.ladder.blow_cost_cents, Some(800));
        assert!(
            elapsed_ms < 16,
            "Critério de aceite §10 violado: build_bus_tick levou {elapsed_ms} ms (máximo 16 ms)"
        );
    }
}
