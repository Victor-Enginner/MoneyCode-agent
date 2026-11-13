// SPDX-License-Identifier: Apache-2.0
//! Grafo acíclico dirigido (DAG) de decisão e classificador de regime do projeto MONSTER.
//! Implementa `feat.window(30)` O(1), `regime.classifier`, `fan_in`, `gate.edge`, `gate.risk` e `slip.compiler`.
//! Executa 100% em memória sem alocação pesada e sem LLM no caminho crítico (<250 ms).

use crate::backtest::{evaluate_matcher_walk_forward, PatternHistoryStats};
use crate::bocpd::{analyze_chronological, BocpdCfg, BocpdReport};
use monster_risk::{
    authorize, authorize_numbers, decompose, stake_cents, Cfg, NumberLadder, State, Verdict,
};
use monster_strategy_native::{number_coverage, BetIntent, Cover, Matcher};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::warn;

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
    /// O sinal foi zerado pelo veto de regime (família trocou ou esfriou).
    #[serde(default)]
    pub regime_veto: bool,
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
                regime_veto: false,
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
            let nl = NumberLadder::new(numbers.len() as u8, cfg.number_base_cents);
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
            let nl = NumberLadder::new(numbers.len() as u8, cfg.number_base_cents);
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

/// Família dos matchers "de fora" (quebra de permanência), dona da prioridade de entrada.
fn is_permanence_family(id: &str) -> bool {
    id.starts_with("quebra_")
}

/// Prioridade fixa do operador: padrão de fora manda, entrada por número espera.
///
/// Cobertura de número (`Straight`/`Terminal` — `cluster_final`, `terminal_quente`) só entra
/// quando **nenhuma** quebra de permanência está disparada neste giro e **nenhuma** escada de
/// fora está aberta. Sem isso os dois sinais opostos competem por confiança e o número ganha
/// por ter confiança mais alta, que é o bug de projeto do §8.
///
/// Devolve quantos sinais de número foram descartados (o `sinais=` do log já sai filtrado).
fn apply_permanence_priority(
    aggregates: &mut Vec<Aggregate>,
    ladder_steps: &LadderSteps,
    min_confidence: f64,
) -> usize {
    // Só bloqueia quando uma permanência está DISPARANDO neste giro. Uma escada de fora aberta
    // (degrau > 0) apenas espera o MESMO padrão voltar a disparar — o gale dela só sai nesse
    // giro, e aí `firing` já é verdadeiro. Antes, `quebra_numero_repetido` parado no degrau 1
    // (padrão raro) bloqueava todas as entradas por número por horas.
    let _ = ladder_steps;
    let firing = aggregates
        .iter()
        .any(|a| is_permanence_family(&a.strategy_id) && a.confidence >= min_confidence);
    if !firing {
        return 0;
    }
    let before = aggregates.len();
    aggregates.retain(|a| number_coverage(&a.cover).is_none());
    before - aggregates.len()
}

/// Escolhe o vencedor do fan-in (lista já ordenada por confiança decrescente).
///
/// Regra do operador: sem nenhuma quebra de permanência disparada (coluna, dúzia, cor,
/// alto/baixo, par/ímpar), a entrada **por número** tem preferência sobre as outras entradas de
/// fora que não são permanência (`cluster_terminal`, `fibonacci_lag`, `setor_vies`). Com
/// permanência disparada os sinais de número já foram removidos por `apply_permanence_priority`.
fn pick_winner(aggregates: &[Aggregate], min_confidence: f64) -> Option<&Aggregate> {
    // Teto do operador: entrada por número nunca passa de MAX_NUMBER_COVERS (24) casas.
    let eligible = |a: &&Aggregate| {
        a.confidence >= min_confidence
            && !a.cover.is_empty()
            && number_coverage(&a.cover)
                .is_none_or(|n| n.len() <= usize::from(monster_risk::MAX_NUMBER_COVERS))
    };
    // Quebra de permanência disparando SEMPRE vence (foco do operador). Antes o desempate era por
    // confiança e o cluster_terminal (aposta a favor da continuação) ganhava da quebra de coluna:
    // coluna 2 repetiu 3x e a entrada saiu em 2+3 em vez de 1+3.
    if let Some(p) = aggregates
        .iter()
        .filter(eligible)
        .find(|a| is_permanence_family(&a.strategy_id))
    {
        return Some(p);
    }
    {
        if let Some(n) = aggregates
            .iter()
            .filter(eligible)
            .find(|a| number_coverage(&a.cover).is_some())
        {
            return Some(n);
        }
    }
    aggregates.iter().find(eligible)
}

/// Janela (giros) usada para medir se os terminais estão concentrados.
pub const TERMINAL_GATE_WINDOW: usize = 12;
/// Máximo de finais distintos na janela para liberar entrada por número.
/// Numa roleta justa, ≤ 5 finais em 12 giros acontece só ~5% do tempo (média = 7).
pub const TERMINAL_GATE_MAX_DISTINCT: usize = 5;

/// Finais distintos nos últimos `TERMINAL_GATE_WINDOW` giros (`window[0]` = mais recente).
pub fn distinct_terminals(window: &[u8]) -> usize {
    let mut seen = [false; 10];
    for &n in window.iter().take(TERMINAL_GATE_WINDOW) {
        seen[usize::from(n % 10)] = true;
    }
    seen.iter().filter(|&&b| b).count()
}

/// Regra do operador: número só entra em momento de terminais concentrados (cluster real).
/// Mesa "bagunçada" (muitos finais diferentes) é terreno da quebra de permanência, não de número.
/// Devolve quantos sinais de número foram descartados.
fn apply_terminal_concentration(aggregates: &mut Vec<Aggregate>, window: &[u8]) -> usize {
    if window.len() < TERMINAL_GATE_WINDOW || distinct_terminals(window) <= TERMINAL_GATE_MAX_DISTINCT {
        return 0;
    }
    let before = aggregates.len();
    aggregates.retain(|a| number_coverage(&a.cover).is_none());
    before - aggregates.len()
}

/// Família de finais que a cobertura de número pretende cobrir (`final % 3`).
///
/// `Terminal(0)` é o curinga e não define família; cobertura sem terminal (coluna, dúzia, cor)
/// não é cobertura de família e nunca recebe veto de regime.
fn cover_family(cover: &[Cover]) -> Option<usize> {
    cover.iter().find_map(|c| match c {
        Cover::Terminal(t) if *t != 0 => Some(usize::from(*t % 3)),
        _ => None,
    })
}

/// Aplica o veto de regime aos sinais de cluster: família que acabou de trocar não recebe entrada.
///
/// O sinal não é descartado — vai a `confidence = 0.0` com `regime_veto = true`, então aparece no
/// stream de decisões com motivo (`JustChanged`/`FamilyFaded`) mas perde o fan-in para qualquer
/// matcher legítimo. Nenhum outro tipo de cobertura é afetado.
fn apply_regime_veto(aggregates: &mut [Aggregate], report: &BocpdReport, cfg: &BocpdCfg) {
    for agg in aggregates.iter_mut() {
        if !agg.strategy_id.contains("cluster") {
            continue;
        }
        let Some(family) = cover_family(&agg.cover) else {
            continue;
        };
        if let Some(reason) = report.veto(family, cfg) {
            warn!(
                strategy_id = %agg.strategy_id,
                family,
                p_change = report.p_change,
                run = report.map_run_length,
                ?reason,
                "veto de regime: familia sem sustentacao"
            );
            agg.confidence = 0.0;
            agg.regime_veto = true;
        }
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
    /// Posterior do BOCPD sobre a família de finais no momento da decisão.
    pub bocpd: BocpdReport,
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
    let mut raw_aggregates = fan_in(window, matchers);
    apply_permanence_priority(&mut raw_aggregates, gate.ladder_steps, min_confidence);
    apply_terminal_concentration(&mut raw_aggregates, window);

    let bocpd_cfg = BocpdCfg::default();
    let bocpd = if chronological_history.len() >= 2 {
        analyze_chronological(chronological_history, &bocpd_cfg)
    } else {
        let mut newest_first = window.to_vec();
        newest_first.reverse();
        analyze_chronological(&newest_first, &bocpd_cfg)
    };

    let mut adjusted_aggregates = Vec::with_capacity(raw_aggregates.len());
    let mut best_history: Option<PatternHistoryStats> = None;

    for mut agg in raw_aggregates {
        if chronological_history.len() >= 25 {
            if let Some(m) = matchers.iter().find(|m| m.id() == agg.strategy_id) {
                let stats = evaluate_matcher_walk_forward(m.as_ref(), chronological_history, cfg);
                // Para os gatilhos de repetição (quebra de coluna/dúzia 4x e cor/alto-baixo/par-ímpar 6x),
                // aplica bônus quando positivo no histórico mas nunca bloqueia o gatilho de repetição!
                // Entrada por número também não é zerada pelo histórico: cobrir 8-15 plenos
                // quase sempre mostra P&L negativo no walk-forward, e o operador quer a entrada
                // sempre que não houver permanência. Ela só recebe bônus, como as quebras.
                let is_core_streak = agg.strategy_id.starts_with("quebra_")
                    || agg.strategy_id == "setor_vies"
                    || number_coverage(&agg.cover).is_some();
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

    apply_regime_veto(&mut adjusted_aggregates, &bocpd, &bocpd_cfg);

    adjusted_aggregates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let best = pick_winner(&adjusted_aggregates, min_confidence);

    let Some(winner) = best else {
        return (
            DagOutput {
                regime,
                aggregates: adjusted_aggregates,
                verdict: None,
                bocpd,
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
            bocpd,
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
    let mut aggregates = fan_in(window, matchers);
    apply_permanence_priority(&mut aggregates, &LadderSteps::new(), min_confidence);
    apply_terminal_concentration(&mut aggregates, window);

    let bocpd_cfg = BocpdCfg::default();
    let mut oldest_first = window.to_vec();
    oldest_first.reverse();
    let bocpd = analyze_chronological(&oldest_first, &bocpd_cfg);
    apply_regime_veto(&mut aggregates, &bocpd, &bocpd_cfg);

    let best = pick_winner(&aggregates, min_confidence);

    let Some(winner) = best else {
        return DagOutput {
            regime,
            aggregates,
            verdict: None,
            plan: None,
            bocpd,
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
        bocpd,
        elapsed_us: start.elapsed().as_micros() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(map_family: Option<usize>, family_share: [f64; 3]) -> BocpdReport {
        BocpdReport {
            observations: 40,
            p_change: 0.02,
            map_run_length: 40,
            family_share,
            map_family,
        }
    }

    #[test]
    fn cover_family_ignores_the_wildcard_terminal() {
        assert_eq!(
            cover_family(&[
                Cover::Terminal(0),
                Cover::Terminal(1),
                Cover::Terminal(4),
                Cover::Terminal(7)
            ]),
            Some(1)
        );
        assert_eq!(cover_family(&[Cover::Terminal(3)]), Some(0));
        assert_eq!(cover_family(&[Cover::Terminal(0)]), None);
        assert_eq!(cover_family(&[Cover::Col(2), Cover::Col(3)]), None);
    }

    fn number_signal() -> Aggregate {
        Aggregate {
            strategy_id: "terminal_quente".to_string(),
            confidence: 0.90,
            cover: vec![Cover::Terminal(2), Cover::Terminal(3)],
            regime_veto: false,
        }
    }

    fn permanence_signal(confidence: f64) -> Aggregate {
        Aggregate {
            strategy_id: "quebra_permanencia_coluna".to_string(),
            confidence,
            cover: vec![Cover::Col(2), Cover::Col(3)],
            regime_veto: false,
        }
    }

    #[test]
    fn numero_cede_a_permanencia_de_fora() {
        // Permanência disparada (0.68 >= 0.55): o terminal, mesmo com 0.90, sai do fan-in.
        let mut aggs = vec![number_signal(), permanence_signal(0.68)];
        assert_eq!(apply_permanence_priority(&mut aggs, &LadderSteps::new(), 0.55), 1);
        assert_eq!(aggs.len(), 1);
        assert_eq!(aggs[0].strategy_id, "quebra_permanencia_coluna");
    }

    #[test]
    fn mesa_baguncada_bloqueia_numero_e_concentrada_libera() {
        // Fita real de 13/11 (mais recente primeiro): 9 finais distintos em 12 giros.
        let baguncada = [7u8, 35, 13, 19, 4, 12, 20, 11, 6, 25, 34, 10];
        assert_eq!(distinct_terminals(&baguncada), 9);
        let mut aggs = vec![number_signal(), permanence_signal(0.40)];
        assert_eq!(apply_terminal_concentration(&mut aggs, &baguncada), 1);
        assert_eq!(aggs.len(), 1, "a entrada de fora continua");

        // Cluster: só finais 1, 3, 7, 9 e 0 → 5 distintos: número liberado.
        let concentrada = [1u8, 11, 3, 21, 13, 7, 17, 31, 9, 19, 10, 33];
        assert_eq!(distinct_terminals(&concentrada), 5);
        let mut aggs = vec![number_signal()];
        assert_eq!(apply_terminal_concentration(&mut aggs, &concentrada), 0);
    }

    #[test]
    fn sem_permanencia_numero_vence_entrada_de_fora_mais_confiante() {
        // cluster_terminal (coluna, não é permanência) com 0.95 contra terminal com 0.60.
        let other = Aggregate {
            strategy_id: "cluster_terminal".to_string(),
            confidence: 0.95,
            cover: vec![Cover::Col(1), Cover::Col(2)],
            regime_veto: false,
        };
        let num = Aggregate { confidence: 0.60, ..number_signal() };
        let aggs = vec![other, num];
        assert_eq!(pick_winner(&aggs, 0.55).map(|a| a.strategy_id.as_str()), Some("terminal_quente"));
    }

    #[test]
    fn quebra_de_coluna_vence_cluster_terminal_mais_confiante() {
        // Caso real 13/11 09:40: coluna 2 repetiu 3x (26, 2, 8).
        let cluster = Aggregate {
            strategy_id: "cluster_terminal".to_string(),
            confidence: 0.90,
            cover: vec![Cover::Col(2), Cover::Col(3)],
            regime_veto: false,
        };
        let quebra = Aggregate {
            strategy_id: "quebra_permanencia_coluna".to_string(),
            confidence: 0.66,
            cover: vec![Cover::Col(1), Cover::Col(3)],
            regime_veto: false,
        };
        let aggs = vec![cluster, quebra];
        let w = pick_winner(&aggs, 0.55).expect("vencedor");
        assert_eq!(w.strategy_id, "quebra_permanencia_coluna");
        assert_eq!(w.cover, vec![Cover::Col(1), Cover::Col(3)]);
    }

    #[test]
    fn com_permanencia_disparada_ela_vence() {
        let aggs = vec![permanence_signal(0.70), Aggregate { confidence: 0.60, ..number_signal() }];
        assert_eq!(
            pick_winner(&aggs, 0.55).map(|a| a.strategy_id.as_str()),
            Some("quebra_permanencia_coluna")
        );
    }

    #[test]
    fn numero_abaixo_do_corte_nao_entra() {
        let aggs = vec![Aggregate { confidence: 0.40, ..number_signal() }];
        assert!(pick_winner(&aggs, 0.55).is_none());
    }

    #[test]
    fn numero_entra_quando_so_ha_pre_alerta_de_permanencia() {
        // Pré-alerta (0.50) não é entrada: o número continua elegível.
        let mut aggs = vec![number_signal(), permanence_signal(0.50)];
        assert_eq!(apply_permanence_priority(&mut aggs, &LadderSteps::new(), 0.55), 0);
        assert_eq!(aggs.len(), 2);
    }

    #[test]
    fn escada_de_fora_parada_nao_bloqueia_numero() {
        // Caso real 13/11: quebra_numero_repetido parado no degrau 1 sem disparar.
        // Número entra; o gale da dúzia só sai quando a dúzia disparar de novo.
        let mut steps = LadderSteps::new();
        steps.insert("quebra_numero_repetido".to_string(), 1);
        let mut aggs = vec![number_signal()];
        assert_eq!(apply_permanence_priority(&mut aggs, &steps, 0.55), 0);
        assert_eq!(aggs.len(), 1);

        // Com a permanência disparando no mesmo giro (gale saindo), o número espera.
        let mut aggs = vec![number_signal(), permanence_signal(0.70)];
        assert_eq!(apply_permanence_priority(&mut aggs, &steps, 0.55), 1);

        // Escada de número aberta não bloqueia número.
        let mut own = LadderSteps::new();
        own.insert("terminal_quente".to_string(), 1);
        let mut aggs = vec![number_signal()];
        assert_eq!(apply_permanence_priority(&mut aggs, &own, 0.55), 0);
    }

    #[test]
    fn regime_veto_zeroes_only_cluster_number_signals() {
        let cfg = BocpdCfg::default();
        // Regime aponta 3-6-9; o cluster quer 1-4-7.
        let rep = report(Some(0), [0.9, 0.05, 0.05]);
        let mut aggregates = vec![
            Aggregate {
                strategy_id: "cluster_final".to_string(),
                confidence: 0.80,
                cover: vec![
                    Cover::Terminal(0),
                    Cover::Terminal(1),
                    Cover::Terminal(4),
                    Cover::Terminal(7),
                ],
                regime_veto: false,
            },
            Aggregate {
                strategy_id: "quebra_permanencia_coluna".to_string(),
                confidence: 0.70,
                cover: vec![Cover::Col(2), Cover::Col(3)],
                regime_veto: false,
            },
        ];
        apply_regime_veto(&mut aggregates, &rep, &cfg);
        assert!(aggregates[0].regime_veto, "cluster fora de fase leva veto");
        assert_eq!(aggregates[0].confidence, 0.0);
        assert!(!aggregates[1].regime_veto, "coluna não é cobertura de família");
        assert_eq!(aggregates[1].confidence, 0.70);
    }
}
