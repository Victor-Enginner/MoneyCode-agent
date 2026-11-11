// SPDX-License-Identifier: Apache-2.0
//! Grafo acíclico dirigido (DAG) de decisão e classificador de regime do projeto MONSTER.
//! Implementa `feat.window(30)` O(1), `regime.classifier`, `fan_in`, `gate.edge`, `gate.risk` e `slip.compiler`.
//! Executa 100% em memória sem alocação pesada e sem LLM no caminho crítico (<250 ms).

use monster_risk::{authorize, decompose, stake_cents, Cfg, State, Verdict};
use monster_strategy_native::{BetIntent, Cover, Matcher};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

/// Capacidade fixa do ring buffer `feat.window(30)` (§8).
pub const WINDOW_CAPACITY: usize = 30;

/// Ring buffer circular O(1) para armazenar os últimos 30 números sorteados (`feat.window(30)`).
#[derive(Debug, Clone)]
pub struct Window30 {
    buf: [u8; WINDOW_CAPACITY],
    head: usize,
    len: usize,
}

impl Default for Window30 {
    fn default() -> Self {
        Self::new()
    }
}

impl Window30 {
    /// Cria um ring buffer vazio de capacidade 30.
    pub const fn new() -> Self {
        Self {
            buf: [0u8; WINDOW_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    /// Insere um novo número (`0..=36`) em tempo constante O(1).
    pub fn push(&mut self, number: u8) {
        self.buf[self.head] = number;
        self.head = (self.head + 1) % WINDOW_CAPACITY;
        if self.len < WINDOW_CAPACITY {
            self.len += 1;
        }
    }

    /// Quantidade atual de números armazenados (`0..=30`).
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Retorna `true` se nenhum spin foi registrado ainda.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exporta a janela ordenada do spin mais recente (`[0]`) para o mais antigo.
    pub fn to_recent_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        for i in 0..self.len {
            let idx = (self.head + WINDOW_CAPACITY - 1 - i) % WINDOW_CAPACITY;
            out.push(self.buf[idx]);
        }
        out
    }
}

/// Regime estatístico classificado sobre janela de 20 spins (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
    /// Razão de runs `< 0.75` (repetições coladas): penaliza matchers de cluster em `0.85`.
    Permanence,
    /// Razão de runs `> 1.25` (alternância rápida): penaliza matchers de permanência em `0.85`.
    Momentum,
    /// Regime neutro (`0.75..=1.25` ou janela `< 20`): sem penalidade.
    Neutral,
}

#[inline]
const fn col_bucket(n: u8) -> u8 {
    if n == 0 || n > 36 {
        0
    } else {
        (n - 1) / 12 + 1
    }
}

/// Classifica o regime usando a razão de runs de colunas sobre os 20 giros mais recentes (§8 do AGENTS.md).
pub fn classify(window: &[u8]) -> Regime {
    if window.len() < 20 {
        return Regime::Neutral;
    }
    let slice = &window[..20];
    let runs = 1
        + slice
            .windows(2)
            .filter(|w| col_bucket(w[0]) != col_bucket(w[1]))
            .count();
    let n = slice.len() as f64;
    let ratio = (runs as f64) / (n / 2.0);
    if ratio < 0.75 {
        Regime::Permanence
    } else if ratio > 1.25 {
        Regime::Momentum
    } else {
        Regime::Neutral
    }
}

/// Resultado agregado de um matcher após ponderação pelo `regime.classifier`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aggregate {
    /// Identificador da estratégia.
    pub strategy_id: String,
    /// Confiança ajustada pelo regime (`0.0..=1.0`).
    pub confidence: f64,
    /// Coberturas recomendadas pelo matcher.
    pub cover: Vec<Cover>,
}

/// Executa o fan-out sobre todos os matchers e aplica o fan-in ponderado pelo `Regime` (§8).
pub fn fan_in(window: &[u8], matchers: &[Arc<dyn Matcher>]) -> Vec<Aggregate> {
    let regime = classify(window);
    let mut list: Vec<Aggregate> = matchers
        .iter()
        .filter_map(|m| {
            let intent: BetIntent = m.evaluate(window)?;
            let bias = match (regime, m.id()) {
                (Regime::Permanence, id) if id.contains("cluster") => 0.85,
                (Regime::Momentum, id) if id.contains("permanencia") => 0.85,
                _ => 1.0,
            };
            Some(Aggregate {
                strategy_id: intent.strategy_id,
                confidence: (intent.confidence * bias).min(0.99),
                cover: intent.cover,
            })
        })
        .collect();

    list.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    list
}

/// Calcula o valor esperado (`ev_cents`) inteiro em centavos para cobertura dupla (borda de `-2/37` por ficha).
pub const fn compute_ev_cents_two_covers(stake_per_cover_cents: u64) -> i64 {
    // Ganho em 24 casas: +stake; perda em 13 casas: -2*stake -> EV = (24*s - 26*s)/37 = -2*s/37
    let s = stake_per_cover_cents as i64;
    let num = -2 * s;
    if num >= 0 {
        num / 37
    } else {
        (num - 18) / 37
    }
}

/// Plano compilado pelo `slip.compiler` pronto para o `executor.place`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Identificador do spin gatilho.
    pub spin_id: String,
    /// Identificador da estratégia vencedora no `aggregator.score`.
    pub strategy_id: String,
    /// Degrau da escada (`1..=max_steps`).
    pub ladder_step: u8,
    /// Valor apostado em cada cobertura em centavos inteiros.
    pub stake_per_cover_cents: u64,
    /// Valor total do slip em centavos inteiros (`covers.len() * stake_per_cover_cents`).
    pub total_cents: u64,
    /// EV estimado da entrada em centavos inteiros.
    pub ev_cents: i64,
    /// Confiança final após o `regime.classifier`.
    pub confidence: f64,
    /// Lista de `(Cover, valor_em_centavos)`.
    pub covers: Vec<(Cover, u64)>,
    /// Decomposição greedy decrescente das fichas por cobertura (`(denom_cents, count)`).
    pub chips_per_cover: Vec<(u64, u8)>,
}

/// Resultado completo da execução do DAG para um spin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagOutput {
    /// Regime classificado na janela atual.
    pub regime: Regime,
    /// Sinais agregados ordenados por confiança decrescente.
    pub aggregates: Vec<Aggregate>,
    /// Veredito do gate de risco (quando houve sinal elegível).
    pub verdict: Option<Verdict>,
    /// Plano compilado de fichas (quando `verdict == Some(Verdict::Place)`).
    pub plan: Option<Plan>,
    /// Latência total do DAG em microssegundos (orçamento: < 250.000 µs).
    pub elapsed_us: u64,
}

/// Executa o pipeline completo do DAG (§8):
/// `feat.window(30) → matchers → regime.classifier → aggregator.score → gate.edge → gate.risk → slip.compiler`.
pub fn evaluate_dag(
    spin_id: &str,
    window: &[u8],
    matchers: &[Arc<dyn Matcher>],
    cfg: &Cfg,
    state: &State,
    min_confidence: f64,
) -> DagOutput {
    let start = Instant::now();
    let regime = classify(window);
    let aggregates = fan_in(window, matchers);

    let best = aggregates
        .iter()
        .find(|agg| agg.confidence >= min_confidence && !agg.cover.is_empty());

    let Some(winner) = best else {
        return DagOutput {
            regime,
            aggregates,
            verdict: None,
            plan: None,
            elapsed_us: start.elapsed().as_micros() as u64,
        };
    };

    let verdict = authorize(cfg, state);
    let plan = if verdict == Verdict::Place {
        let ladder_step = state.step.saturating_add(1);
        let stake_per_cover_cents = stake_cents(ladder_step, cfg.base_cents);
        let covers: Vec<(Cover, u64)> = winner
            .cover
            .iter()
            .copied()
            .map(|c| (c, stake_per_cover_cents))
            .collect();
        let total_cents = stake_per_cover_cents.saturating_mul(covers.len() as u64);
        let ev_cents = compute_ev_cents_two_covers(stake_per_cover_cents);
        let chips_per_cover = decompose(stake_per_cover_cents, &cfg.chip_denoms_cents);
        Some(Plan {
            spin_id: spin_id.to_string(),
            strategy_id: winner.strategy_id.clone(),
            ladder_step,
            stake_per_cover_cents,
            total_cents,
            ev_cents,
            confidence: winner.confidence,
            covers,
            chips_per_cover,
        })
    } else {
        None
    };

    DagOutput {
        regime,
        aggregates,
        verdict: Some(verdict),
        plan,
        elapsed_us: start.elapsed().as_micros() as u64,
    }
}
