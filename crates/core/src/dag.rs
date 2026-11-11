// SPDX-License-Identifier: Apache-2.0
//! Grafo acíclico dirigido (DAG) de decisão e classificador de regime do projeto MONSTER.
//! Implementa `feat.window(30)` O(1), `regime.classifier`, `fan_in`, `gate.edge`, `gate.risk` e `slip.compiler`.
//! Executa 100% em memória sem alocação pesada e sem LLM no caminho crítico (<250 ms).

use crate::backtest::{evaluate_matcher_walk_forward, PatternHistoryStats};
use monster_risk::{
    authorize, authorize_numbers, decompose, stake_cents, Cfg, NumberLadder, State, Verdict,
};
use monster_strategy_native::{number_coverage, BetIntent, Cover, Matcher};
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

/// EV inteiro em centavos de uma cobertura de `k` números com ficha `x` em cada.
/// Acerto em `k` casas paga `35x` e a exposição é `k·x`: `EV = −k·x/37` (borda constante).
pub const fn compute_ev_cents_numbers(numbers: u64, stake_per_number_cents: u64) -> i64 {
    let num = -((numbers * stake_per_number_cents) as i64);
    if num >= 0 {
        num / 37
    } else {
        (num - 36) / 37
    }
}

/// Gate do §6 aplicado à geometria do vencedor: cobertura de número usa `k·x` de exposição
/// e ficha por número no limite de mesa; o resto segue a regra de duas colunas (`2x`).
fn gate_for(winner: &Aggregate, cfg: &Cfg, state: &State) -> Verdict {
    match number_coverage(&winner.cover) {
        Some(numbers) => {
            let nl = NumberLadder::new(numbers.len() as u8, cfg.base_cents);
            authorize_numbers(cfg, &nl, state)
        }
        None => authorize(cfg, state),
    }
}

/// Compila o `slip.compiler` para o vencedor do fan-in.
///
/// Cobertura de número (`Straight`/`Terminal`) é expandida em um pleno por número, com a ficha
/// da [`NumberLadder`] (`x = (D+G)/(36−k)`). É isso que faz `Terminal(0)` virar 4 cliques em
/// `straight:0`, `straight:10`, `straight:20`, `straight:30` — o plano nunca chega ao executor
/// com um `terminal:` que o pano não sabe resolver. Coluna/dúzia/cor seguem a escada ×3 do §6.
fn compile_plan(spin_id: &str, winner: &Aggregate, ladder_step: u8, cfg: &Cfg) -> Option<Plan> {
    let (covers, stake_per_cover_cents, ev_cents): (Vec<(Cover, u64)>, u64, i64) =
        if let Some(numbers) = number_coverage(&winner.cover) {
            let nl = NumberLadder::new(numbers.len() as u8, cfg.base_cents);
            let stake = nl.stake_cents(ladder_step);
            let covers = numbers
                .iter()
                .map(|&n| (Cover::Straight(n), stake))
                .collect();
            let ev = compute_ev_cents_numbers(numbers.len() as u64, stake);
            (covers, stake, ev)
        } else {
            let stake = stake_cents(ladder_step, cfg.base_cents);
            let covers = winner.cover.iter().copied().map(|c| (c, stake)).collect();
            (covers, stake, compute_ev_cents_two_covers(stake))
        };

    let total_cents = stake_per_cover_cents.saturating_mul(covers.len() as u64);
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

/// Degrau de escada por estratégia (§5.3: `ladder_state` é chaveado por `strategy_id`).
/// A escada é individual: um degrau global fazia a perda da coluna entrar como degrau 2 da dúzia.
pub type LadderSteps = std::collections::HashMap<String, u8>;

/// Contexto do gate de risco: estado instantâneo da mesa + degrau de cada estratégia (§6 e §8).
#[derive(Debug, Clone, Copy)]
pub struct GateCtx<'a> {
    /// Banca, P&L da sessão, janela restante e cooldown.
    pub state: &'a State,
    /// Degrau persistido em `ladder_state` por `strategy_id`.
    pub ladder_steps: &'a LadderSteps,
}

/// Executa o pipeline completo do DAG (§8) combinado com validação Walk-Forward histórica (`HistoryBuffer`):
/// Se `chronological_history.len() >= 25`, simula cada candidato nos giros passados da mesa e bloqueia
/// (`confidence = 0.0`) qualquer padrão que esteja com P&L negativo ou estouro recente na mesa atual,
/// premiando padrões invictos (`0` estouros e P&L > 0).
pub fn evaluate_dag_with_history(
    spin_id: &str,
    window: &[u8],
    chronological_history: &[u8],
    matchers: &[Arc<dyn Matcher>],
    cfg: &Cfg,
    gate: &GateCtx<'_>,
    min_confidence: f64,
) -> (DagOutput, Option<PatternHistoryStats>) {
    let state = gate.state;
    let ladder_steps = gate.ladder_steps;
    let start = Instant::now();
    let regime = classify(window);
    let raw_aggregates = fan_in(window, matchers);

    let mut adjusted_aggregates = Vec::with_capacity(raw_aggregates.len());
    let mut best_history: Option<PatternHistoryStats> = None;

    for mut agg in raw_aggregates {
        if chronological_history.len() >= 25 {
            if let Some(m) = matchers.iter().find(|m| m.id() == agg.strategy_id) {
                let stats = evaluate_matcher_walk_forward(m.as_ref(), chronological_history, cfg);
                // Para os gatilhos de repetição (quebra de coluna/dúzia 4x e cor/alto-baixo/par-ímpar 6x),
                // aplica bônus quando positivo no histórico mas nunca bloqueia o gatilho de repetição!
                let is_core_streak = agg.strategy_id.starts_with("quebra_")
                    || agg.strategy_id == "setor_vies";
                let effective_mult = if is_core_streak {
                    stats.confidence_multiplier.max(1.0)
                } else {
                    stats.confidence_multiplier
                };
                if agg.confidence >= min_confidence {
                    agg.confidence = (agg.confidence * effective_mult).min(0.99);
                }
                if best_history.is_none() && agg.confidence >= min_confidence {
                    best_history = Some(stats);
                }
            }
        }
        adjusted_aggregates.push(agg);
    }

    adjusted_aggregates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let best = adjusted_aggregates
        .iter()
        .find(|agg| agg.confidence >= min_confidence && !agg.cover.is_empty());

    let Some(winner) = best else {
        return (
            DagOutput {
                regime,
                aggregates: adjusted_aggregates,
                verdict: None,
                plan: None,
                elapsed_us: start.elapsed().as_micros() as u64,
            },
            best_history,
        );
    };

    if best_history.is_none() && chronological_history.len() >= 25 {
        if let Some(m) = matchers.iter().find(|m| m.id() == winner.strategy_id) {
            best_history = Some(evaluate_matcher_walk_forward(
                m.as_ref(),
                chronological_history,
                cfg,
            ));
        }
    }

    // O degrau é o da estratégia vencedora — nunca o de outra que perdeu antes (§6 e §9).
    let step = match ladder_steps.get(&winner.strategy_id).copied() {
        Some(step) => step,
        // Escada carregada: estratégia sem linha está no degrau 1 e não herda o degrau de outra.
        None if !ladder_steps.is_empty() => 0,
        // Escada não carregada (chamador puro/teste): respeita o degrau do estado.
        None => state.step,
    };
    let strategy_state = State { step, ..*state };
    let verdict = gate_for(winner, cfg, &strategy_state);
    let plan = if verdict == Verdict::Place {
        compile_plan(spin_id, winner, step.saturating_add(1), cfg)
    } else {
        None
    };

    (
        DagOutput {
            regime,
            aggregates: adjusted_aggregates,
            verdict: Some(verdict),
            plan,
            elapsed_us: start.elapsed().as_micros() as u64,
        },
        best_history,
    )
}
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

    let verdict = gate_for(winner, cfg, state);
    let plan = if verdict == Verdict::Place {
        compile_plan(spin_id, winner, state.step.saturating_add(1), cfg)
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
