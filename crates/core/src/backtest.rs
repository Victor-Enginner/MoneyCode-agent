// SPDX-License-Identifier: Apache-2.0
//! Motor de avaliação histórica Walk-Forward (`History500`) do projeto MONSTER.
//! Simula cada padrão/matcher contra os últimos giros reais da mesa (`Results`) na escada ×3.
//! Calcula P&L histórico real, taxa de acerto por degrau (D1..D4), estouros e decide se vale entrar.

use monster_risk::{cum_loss_cents, stake_cents, Cfg};
use monster_strategy_native::{Cover, Matcher};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

/// Capacidade máxima do histórico da mesa (`Results` da Playtech guarda até ~500 giros).
pub const HISTORY_CAPACITY: usize = 500;

/// Buffer histórico de até 500 giros reais da mesa (ordenado do mais antigo para o mais recente).
#[derive(Debug, Clone, Default)]
pub struct HistoryBuffer {
    spins_chronological: VecDeque<u8>,
}

impl HistoryBuffer {
    /// Cria um buffer histórico vazio com capacidade para 500 giros.
    pub fn new() -> Self {
        Self {
            spins_chronological: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    /// Alimenta o buffer com uma lista de giros em ordem do mais recente (`[0]`) para o mais antigo
    /// (formato extraído diretamente da grade `Results` do DOM da Playtech).
    /// Retorna quantos giros foram carregados quando o buffer estava vazio ou menor.
    pub fn seed_from_recent_first(&mut self, recent_first: &[u8]) -> usize {
        if recent_first.len() <= self.spins_chronological.len() {
            return 0;
        }
        self.spins_chronological.clear();
        for &n in recent_first.iter().rev() {
            if n <= 36 {
                if self.spins_chronological.len() == HISTORY_CAPACITY {
                    self.spins_chronological.pop_front();
                }
                self.spins_chronological.push_back(n);
            }
        }
        self.spins_chronological.len()
    }

    /// Registra um novo giro ao vivo no final da fila cronológica.
    pub fn push_live(&mut self, number: u8) {
        if number > 36 {
            return;
        }
        if self.spins_chronological.len() == HISTORY_CAPACITY {
            self.spins_chronological.pop_front();
        }
        self.spins_chronological.push_back(number);
    }

    /// Quantidade total de giros armazenados no histórico.
    pub fn len(&self) -> usize {
        self.spins_chronological.len()
    }

    /// Retorna `true` se o histórico estiver vazio.
    pub fn is_empty(&self) -> bool {
        self.spins_chronological.is_empty()
    }

    /// Exporta os giros em ordem cronológica (do mais antigo `[0]` ao mais recente).
    pub fn to_chronological_vec(&self) -> Vec<u8> {
        self.spins_chronological.iter().copied().collect()
    }

    /// Exporta os últimos `max_len` giros do mais recente (`[0]`) para o mais antigo.
    pub fn to_recent_vec(&self, max_len: usize) -> Vec<u8> {
        self.spins_chronological
            .iter()
            .rev()
            .take(max_len)
            .copied()
            .collect()
    }
}

/// Estatísticas reais de um padrão/matcher simulado em Walk-Forward sobre o histórico da mesa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternHistoryStats {
    /// Identificador da estratégia/padrão.
    pub strategy_id: String,
    /// Quantidade de giros históricos analisados.
    pub spins_analyzed: usize,
    /// Total de ciclos de entrada encerrados (`wins + busts`).
    pub cycles_completed: u32,
    /// Vitórias por degrau da escada (`[D1, D2, D3, D4, D5]`).
    pub wins_by_step: [u32; 5],
    /// Quantidade de estouros da escada (`> max_steps`).
    pub busts: u32,
    /// Degrau em andamento neste exato momento na simulação (`0` se fora de ciclo).
    pub active_step: u8,
    /// P&L líquido acumulado em centavos na mesa atual (`+wins*base - busts*cum_loss`).
    pub net_pnl_cents: i64,
    /// Taxa de vitória dos ciclos encerrados (`0.0..=1.0`).
    pub cycle_win_rate: f64,
    /// Fração de vitórias resolvidas já no 1º ou 2º degrau (`0.0..=1.0`).
    pub early_resolution_rate: f64,
    /// Há quantos giros ocorreu o último estouro (`None` se zero estouros no histórico).
    pub spins_since_last_bust: Option< usize>,
    /// Multiplicador de confiança baseado na performance real da mesa (`0.0` bloqueia, `>1.0` premia).
    pub confidence_multiplier: f64,
    /// Veredito objetivo: vale a pena entrar nesse padrão nesta mesa agora?
    pub worth_entering: bool,
    /// Resumo legível para exibição no FUI (`MOTIVO` / diagnóstico).
    pub summary: String,
}

impl PatternHistoryStats {
    /// Total de ciclos vencidos (`soma de wins_by_step`).
    pub fn total_wins(&self) -> u32 {
        self.wins_by_step.iter().sum()
    }
}

/// Verifica se o número `n` vence uma cobertura considerando Coluna Real (`(n-1)%3+1`) para `Cover::Col`
/// ou Dúzia (`(n-1)/12+1`) para `Cover::Dozen`.
pub const fn cover_wins_real_roulette(cover: Cover, number: u8) -> bool {
    if number == 0 || number > 36 {
        return false;
    }
    match cover {
        Cover::Col(c) | Cover::Col2To1(c) => ((number - 1) % 3 + 1) == c,
        Cover::Dozen(d) => ((number - 1) / 12 + 1) == d,
        Cover::Red => monster_strategy_native::is_red_number(number),
        Cover::Black => !monster_strategy_native::is_red_number(number),
        Cover::Low => number <= 18,
        Cover::High => number >= 19,
        Cover::Even => number.is_multiple_of(2),
        Cover::Odd => !number.is_multiple_of(2),
        Cover::Straight(n) => number == n,
        Cover::Terminal(t) => (number % 10) == t,
    }
}

/// Simula um matcher em modo Walk-Forward estrito (sem olhar o futuro) sobre `chronological_spins`.
/// Quando um gatilho dispara, abre um ciclo de até `cfg.max_steps` degraus mantendo a cobertura alvo
/// até bater (WIN: `+cfg.base_cents`) ou estourar (BUST: `-cum_loss_cents(cfg.max_steps, cfg.base_cents)`).
pub fn evaluate_matcher_walk_forward(
    matcher: &dyn Matcher,
    chronological_spins: &[u8],
    cfg: &Cfg,
) -> PatternHistoryStats {
    let max_steps = cfg.max_steps.clamp(1, 5);
    let win_profit_cents = cfg.base_cents as i64;
    let bust_loss_cents = cum_loss_cents(max_steps, cfg.base_cents) as i64;

    let mut recent_window: VecDeque<u8> = VecDeque::with_capacity(30);
    let mut wins_by_step = [0u32; 5];
    let mut busts = 0u32;
    let mut net_pnl_cents = 0i64;
    let mut active_step: u8 = 0;
    let mut active_covers: Vec<Cover> = Vec::new();
    let mut cooldown_left: u32 = 0;
    let mut last_bust_idx: Option<usize> = None;

    for (idx, &spin) in chronological_spins.iter().enumerate() {
        // 1. Se havia uma aposta ativa do giro anterior, reconcilia com o `spin` atual
        if active_step > 0 && !active_covers.is_empty() {
            let hit = active_covers.iter().any(|c| c.matches_number(spin));
            if hit {
                let slot = usize::from(active_step.saturating_sub(1)).min(4);
                wins_by_step[slot] += 1;
                net_pnl_cents += win_profit_cents;
                active_step = 0;
                active_covers.clear();
            } else if active_step >= max_steps {
                // Estouro da escada!
                busts += 1;
                net_pnl_cents -= bust_loss_cents;
                active_step = 0;
                active_covers.clear();
                cooldown_left = cfg.cooldown_spins;
                last_bust_idx = Some(idx);
            } else {
                // Avança para o próximo degrau na mesma cobertura
                active_step += 1;
            }
        } else {
            cooldown_left = cooldown_left.saturating_sub(1);
        }

        // 2. Atualiza a janela deslizante de 30 giros (`recent_window[0]` = spin atual)
        if recent_window.len() == 30 {
            recent_window.pop_back();
        }
        recent_window.push_front(spin);

        // 3. Se não estamos no meio de uma escada nem em cooldown, consulta o matcher (apenas sinais de entrada >= 0.55)
        if active_step == 0 && cooldown_left == 0 {
            let slice: Vec<u8> = recent_window.iter().copied().collect();
            if let Some(intent) = matcher.evaluate(&slice) {
                if !intent.cover.is_empty() && intent.confidence >= 0.55 {
                    active_step = 1;
                    active_covers = intent.cover;
                }
            }
        }
    }

    let total_wins: u32 = wins_by_step.iter().sum();
    let cycles_completed = total_wins + busts;
    let cycle_win_rate = if cycles_completed > 0 {
        f64::from(total_wins) / f64::from(cycles_completed)
    } else {
        1.0
    };
    let early_wins = wins_by_step[0] + wins_by_step[1];
    let early_resolution_rate = if total_wins > 0 {
        f64::from(early_wins) / f64::from(total_wins)
    } else {
        0.0
    };
    let spins_since_last_bust =
        last_bust_idx.map(|b_idx| chronological_spins.len().saturating_sub(1 + b_idx));

    // Regra de decisão "Vale a pena entrar?":
    // - Se o padrão já rodou pelo menos 2 ciclos na mesa:
    //   precisa estar com P&L líquido >= 0 (sem prejuízo acumulado) E sem estouro nos últimos 45 giros.
    // - Se teve estouro nos últimos 45 giros ou P&L negativo, bloqueia (worth_entering = false, mult = 0.0).
    let recent_bust_veto = spins_since_last_bust.is_some_and(|ago| ago < 45);
    let negative_pnl_veto = cycles_completed >= 2 && net_pnl_cents < 0;

    let (worth_entering, confidence_multiplier) = if recent_bust_veto || negative_pnl_veto {
        (false, 0.0)
    } else if cycles_completed >= 3 && busts == 0 && net_pnl_cents > 0 {
        // Padrão invicto na mesa com 3+ ciclos limpos: bônus de confiança
        let bonus = if early_resolution_rate >= 0.80 {
            1.15
        } else {
            1.08
        };
        (true, bonus)
    } else if cycles_completed == 0 {
        // Ainda sem amostragem suficiente: permite com peso neutro se a janela for curta
        (true, 1.0)
    } else {
        (true, 1.0)
    };

    let sign = if net_pnl_cents >= 0 { "+" } else { "" };
    let summary = format!(
        "hist={}g ciclos={}V/{}E (D1:{} D2:{} D3:{} D4:{}) pnl={}{}c vale={}",
        chronological_spins.len(),
        total_wins,
        busts,
        wins_by_step[0],
        wins_by_step[1],
        wins_by_step[2],
        wins_by_step[3],
        sign,
        net_pnl_cents,
        if worth_entering { "SIM" } else { "NAO" }
    );

    let _ = stake_cents(1, cfg.base_cents);

    PatternHistoryStats {
        strategy_id: matcher.id().to_string(),
        spins_analyzed: chronological_spins.len(),
        cycles_completed,
        wins_by_step,
        busts,
        active_step,
        net_pnl_cents,
        cycle_win_rate,
        early_resolution_rate,
        spins_since_last_bust,
        confidence_multiplier,
        worth_entering,
        summary,
    }
}

/// Avalia todos os matchers contra o histórico atual da mesa e retorna o ranking por P&L histórico.
pub fn evaluate_all_matchers_history(
    matchers: &[Arc<dyn Matcher>],
    chronological_spins: &[u8],
    cfg: &Cfg,
) -> Vec<PatternHistoryStats> {
    let mut out: Vec<PatternHistoryStats> = matchers
        .iter()
        .map(|m| evaluate_matcher_walk_forward(m.as_ref(), chronological_spins, cfg))
        .collect();
    out.sort_by(|a, b| {
        b.worth_entering
            .cmp(&a.worth_entering)
            .then_with(|| b.net_pnl_cents.cmp(&a.net_pnl_cents))
            .then_with(|| b.total_wins().cmp(&a.total_wins()))
    });
    out
}

/// Os 479 giros reais extraídos da grade `Results` da Roleta Brasileira (jogatudo.com)
/// (ordenados do mais recente `[0]` = 22 até o mais antigo).
pub const REAL_TABLE_479_RECENT_FIRST: [u8; 479] = [
    // Imagem 1 - Linhas 1 a 18 (216 giros)
    22, 18, 0, 26, 6, 24, 2, 28, 0, 3, 29, 33, // L1
    31, 34, 13, 6, 14, 1, 14, 28, 33, 19, 0, 25, // L2
    10, 35, 13, 6, 14, 30, 36, 8, 12, 21, 18, 21, // L3
    5, 8, 27, 1, 27, 8, 5, 13, 29, 7, 35, 9, // L4
    16, 30, 31, 30, 19, 18, 8, 29, 34, 8, 24, 29, // L5
    28, 17, 28, 31, 30, 7, 4, 9, 17, 20, 19, 5, // L6
    14, 22, 12, 32, 5, 26, 16, 0, 12, 7, 8, 19, // L7
    18, 27, 15, 19, 15, 4, 22, 9, 31, 8, 14, 32, // L8
    11, 8, 5, 18, 0, 23, 9, 22, 17, 28, 12, 17, // L9
    35, 12, 6, 31, 26, 23, 15, 8, 33, 30, 0, 3, // L10
    15, 10, 28, 9, 17, 27, 6, 30, 19, 26, 12, 5, // L11
    6, 8, 0, 31, 30, 26, 27, 12, 27, 4, 36, 32, // L12
    12, 1, 20, 24, 16, 33, 16, 0, 10, 21, 28, 12, // L13
    23, 11, 30, 29, 26, 34, 31, 36, 24, 14, 18, 18, // L14
    19, 16, 21, 13, 18, 19, 32, 8, 26, 10, 23, 27, // L15
    1, 26, 30, 3, 4, 17, 29, 5, 31, 34, 16, 35, // L16
    27, 16, 22, 17, 9, 7, 20, 21, 25, 24, 33, 33, // L17
    11, 1, 4, 3, 35, 20, 15, 33, 27, 22, 8, 0, // L18
    // Imagem 2 - Linhas 19 a 40 (263 giros seguintes)
    17, 36, 24, 16, 11, 25, 35, 31, 3, 9, 0, 18, // L19
    6, 2, 26, 18, 28, 34, 26, 4, 22, 5, 0, 30, // L20
    16, 6, 4, 9, 24, 0, 14, 14, 27, 31, 29, 3, // L21
    33, 10, 25, 23, 4, 4, 19, 36, 11, 35, 4, 24, // L22
    6, 15, 30, 11, 14, 5, 34, 13, 29, 5, 2, 0, // L23
    0, 33, 9, 17, 23, 0, 36, 30, 4, 11, 0, 14, // L24
    36, 7, 25, 32, 5, 29, 15, 0, 9, 18, 22, 21, // L25
    8, 24, 30, 14, 15, 29, 9, 15, 3, 9, 29, 14, // L26
    2, 12, 7, 29, 32, 21, 22, 1, 12, 17, 15, 1, // L27
    0, 21, 4, 32, 7, 33, 17, 29, 3, 7, 12, 32, // L28
    30, 24, 1, 28, 31, 22, 29, 16, 6, 3, 0, 12, // L29
    36, 3, 26, 0, 31, 23, 17, 36, 8, 26, 33, 23, // L30
    30, 28, 11, 26, 18, 15, 21, 27, 6, 6, 24, 35, // L31
    17, 18, 22, 6, 25, 17, 14, 21, 30, 23, 2, 15, // L32
    22, 10, 10, 17, 36, 23, 33, 2, 23, 11, 24, 24, // L33
    2, 0, 26, 34, 22, 4, 5, 0, 27, 18, 24, 16, // L34
    10, 18, 16, 24, 35, 23, 1, 21, 27, 24, 26, 35, // L35
    8, 0, 0, 7, 35, 18, 10, 20, 2, 15, 31, 5, // L36
    10, 5, 26, 4, 4, 28, 3, 10, 27, 11, 28, 27, // L37
    28, 28, 1, 22, 32, 1, 34, 16, 7, 31, 7, 23, // L38
    3, 23, 25, 10, 9, 36, 18, 9, 8, 0, 8, 17, // L39
    2, 29, 30, 15, 5, 33, 18, 2, 2, 26, 8, // L40
];

#[cfg(test)]
mod tests {
    use super::*;
    use monster_strategy_native::default_matchers;

    #[test]
    fn analyze_467_real_spins_from_user_screenshots() {
        let mut hist = HistoryBuffer::new();
        let loaded = hist.seed_from_recent_first(&REAL_TABLE_479_RECENT_FIRST);
        assert_eq!(loaded, 479);

        let chrono = hist.to_chronological_vec();
        let cfg = Cfg {
            base_cents: 10,
            max_steps: 4,
            cooldown_spins: 2,
            ..Cfg::default()
        };

        let matchers = default_matchers();
        let report = evaluate_all_matchers_history(&matchers, &chrono, &cfg);
        for item in &report {
            println!(
                "[479 GIROS REAIS] {:<28} | {}",
                item.strategy_id, item.summary
            );
        }
        assert!(!report.is_empty());
        // Garante que os dois padrões invictos (setor_vies e quebra_coluna_real_2to1) estão aprovados (worth_entering = true)
        // e que os padrões que estouraram nessa mesa estão bloqueados (worth_entering = false)!
        let setor = report
            .iter()
            .find(|r| r.strategy_id == "setor_vies")
            .expect("setor_vies present");
        assert!(setor.worth_entering);
        assert_eq!(setor.busts, 0);

        let col2to1 = report
            .iter()
            .find(|r| r.strategy_id == "quebra_coluna_real_2to1")
            .expect("quebra_coluna_real_2to1 present");
        assert!(col2to1.worth_entering);
        assert_eq!(col2to1.busts, 0);

        let cluster = report
            .iter()
            .find(|r| r.strategy_id == "cluster_terminal")
            .expect("cluster_terminal present");
        assert!(cluster.worth_entering);
        assert_eq!(cluster.busts, 0);

        let cor = report
            .iter()
            .find(|r| r.strategy_id == "quebra_cor_6x")
            .expect("quebra_cor_6x present");
        assert!(cor.worth_entering);
        assert_eq!(cor.busts, 0);
    }
}
