// SPDX-License-Identifier: Apache-2.0
//! Motor de risco e matemática da escada tripla (×3) do projeto MONSTER.
//! Opera estritamente com centavos inteiros (`u64`/`i64`) e probabilidades `f64`.
//! Implementa autorização de entradas, limites de mesa e decomposição de fichas.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

pub mod mc;
pub mod mc_numbers;
pub use mc::{run_monte_carlo, McParams, McReport};
pub use mc_numbers::{
    run_monte_carlo_numbers, NumbersMcParams, NumbersMcReport, P_RUIN_GATE,
};

/// Probabilidade de perda ao cobrir 2 colunas em roleta single-zero (13/37).
pub const P_LOSE: f64 = 13.0 / 37.0;

/// Probabilidade de vitória ao cobrir 2 colunas em roleta single-zero (24/37).
pub const P_WIN: f64 = 24.0 / 37.0;

/// Denominações padrão de fichas em centavos inteiros (ordem decrescente).
pub const DEFAULT_CHIP_DENOMS_CENTS: [u64; 8] = [5000, 2000, 1000, 500, 100, 50, 20, 10];

/// Ficha por cobertura no degrau `step` (cresce ×3 por degrau para cobrir perda `2x` + lucro `base_cents`).
pub fn stake_cents(step: u8, base_cents: u64) -> u64 {
    let mut d = 0u64;
    for _ in 1..step { let x = d + base_cents; d += 2 * x; }
    d + base_cents
}

/// Perda acumulada em centavos após perder até o degrau `step` (inclusivo).
pub fn cum_loss_cents(step: u8, base_cents: u64) -> u64 {
    (1..=step).fold(0u64, |d, _| d + 2 * (d + base_cents))
}

/// Ficha mínima da bandeja em centavos (todas as denominações são múltiplas dela).
pub const MIN_CHIP_CENTS: u64 = 10;

/// Teto de números distintos que uma cobertura de número pode cobrir.
pub const MAX_NUMBER_COVERS: u8 = 24;

/// Divisão para cima em inteiros (centavos), sem float.
const fn ceil_div(a: u64, b: u64) -> u64 {
    if b == 0 {
        return a;
    }
    a / b + if a % b == 0 { 0 } else { 1 }
}

/// Escada de cobertura por números (pleno e terminal), onde cada número paga 35:1.
///
/// Com `k` números pagos `x` cada: exposição `k·x`, retorno `36·x` no acerto e lucro
/// `36·x − k·x = (36 − k)·x`. Para recuperar a perda `D` e lucrar `G` o degrau é
/// `x = (D + G) / (36 − k)`, arredondado para cima até a ficha mínima.
///
/// A escada ×3 de [`stake_cents`] **não vale aqui**: ela assume exposição `2x` e retorno `3x`,
/// que é a geometria de duas colunas/dúzias. Em cobertura de números ela superaposta a
/// exposição (para `k=15` o degrau 2 correto é 10¢ por número, e ×3 pediria 30¢).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberLadder {
    /// Quantidade de números distintos cobertos (`1` = pleno seco, `15` = cluster 1,4,7 + 0).
    pub numbers: u8,
    /// Lucro desejado por ciclo em centavos (o `base_cents` do §6).
    pub base_cents: u64,
    /// Ficha mínima aceita pela mesa em centavos.
    pub min_chip_cents: u64,
}

impl NumberLadder {
    /// Escada para `numbers` números com lucro alvo `base_cents` e piso de ficha da bandeja.
    pub const fn new(numbers: u8, base_cents: u64) -> Self {
        Self {
            numbers,
            base_cents,
            min_chip_cents: MIN_CHIP_CENTS,
        }
    }

    /// `k` normalizado em `1..=MAX_NUMBER_COVERS`.
    fn k(&self) -> u64 {
        u64::from(self.numbers.clamp(1, MAX_NUMBER_COVERS))
    }

    /// Unidades de lucro por unidade apostada no acerto: `36 − k`.
    fn payout_units(&self) -> u64 {
        36 - self.k()
    }

    /// Arredonda para cima até o múltiplo da ficha mínima, para a ficha ser decomponível.
    fn round_chip(&self, cents: u64) -> u64 {
        let step = self.min_chip_cents.max(1);
        ceil_div(cents, step) * step
    }

    /// Ficha por número necessária para recuperar `d` e lucrar `base_cents`.
    fn ficha(&self, d: u64) -> u64 {
        self.round_chip(ceil_div(
            d.saturating_add(self.base_cents),
            self.payout_units(),
        ))
        .max(self.min_chip_cents)
    }

    /// Ficha por número no degrau `step` (1-based), já arredondada para ficha possível.
    pub fn stake_cents(&self, step: u8) -> u64 {
        let mut d = 0u64;
        for _ in 1..step {
            d = d.saturating_add(self.k().saturating_mul(self.ficha(d)));
        }
        self.ficha(d)
    }

    /// Perda acumulada em centavos ao perder até o degrau `step` (inclusivo).
    pub fn cum_loss_cents(&self, step: u8) -> u64 {
        let mut d = 0u64;
        for _ in 1..=step {
            d = d.saturating_add(self.k().saturating_mul(self.ficha(d)));
        }
        d
    }

    /// Exposição total do degrau `step`: uma ficha por número coberto.
    pub fn exposure_cents(&self, step: u8) -> u64 {
        self.k().saturating_mul(self.stake_cents(step))
    }

    /// Lucro líquido no acerto do degrau `step` (sempre `>= base_cents` pelo arredondamento).
    pub fn win_profit_cents(&self, step: u8) -> i64 {
        let x = self.stake_cents(step) as i64;
        let d = self.cum_loss_cents(step.saturating_sub(1)) as i64;
        (36 - self.k() as i64) * x - d
    }

    /// Último degrau cuja ficha por número ainda cabe no limite da mesa.
    pub fn steps_within_limit(&self, max_bet_per_cover_cents: u64) -> Option<u8> {
        let mut last = None;
        for step in 1..=64u8 {
            if self.stake_cents(step) > max_bet_per_cover_cents {
                break;
            }
            last = Some(step);
        }
        last
    }
}

/// Motivos de pulo temporário de uma entrada (`Skip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Skip {
    /// Janela restante menor que `min_window_ms` (2000 ms).
    WindowTooShort,
    /// Ainda em período de cooldown pós-estouro de escada.
    Cooldown,
}

/// Motivos de aborto imediato da entrada (`Abort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Abort {
    /// Próximo degrau excede `max_steps`.
    LadderBlown,
    /// Valor por cobertura excede o teto da mesa (`max_bet_per_cover_cents`).
    TableLimit,
    /// Exposição total excede a fração máxima da banca (`max_exposure_pct`).
    Exposure,
    /// P&L da sessão atingiu ou ultrapassou o limite de `stop_loss_cents`.
    StopLoss,
}

/// Veredito determinístico do gate de risco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Autoriza o envio da intenção ao compilador de fichas e executor.
    Place,
    /// Ignora este spin sem alterar a escada.
    Skip(Skip),
    /// Aborta a operação por violação de invariante de risco.
    Abort(Abort),
}

/// Erros de configuração ou decomposição de risco.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RiskError {
    /// Lista de denominações vazia ou inválida.
    #[error("lista de denominações de fichas vazia")]
    EmptyDenominations,
    /// Valor não decomponível exatamente pelas fichas fornecidas.
    #[error("valor {amount_cents} centavos deixa resto {remainder_cents} centavos")]
    Remainder {
        /// Valor total solicitado em centavos.
        amount_cents: u64,
        /// Resto não coberto pelas denominações.
        remainder_cents: u64,
    },
}

/// Parâmetros de configuração do gate de risco (`config/risk.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cfg {
    /// Valor base por cobertura no degrau 1 em centavos inteiros (escada de fora: coluna/dúzia/cor).
    pub base_cents: u64,
    /// Lucro alvo por ciclo da escada de **números** (`NumberLadder`), separado da ficha de fora.
    #[serde(default = "default_number_base_cents")]
    pub number_base_cents: u64,
    /// Número máximo de degraus da escada.
    pub max_steps: u8,
    /// Limite máximo da mesa por cobertura em centavos inteiros.
    pub max_bet_per_cover_cents: u64,
    /// Fração máxima de exposição da banca (`0.0..=1.0`).
    pub max_exposure_pct: f64,
    /// Limite de perda da sessão em centavos inteiros positivos (ex.: `7260`).
    pub stop_loss_cents: i64,
    /// Meta percentual de ganho da sessão (`0.0..=1.0`).
    pub stop_win_pct: f64,
    /// Tempo mínimo restante da janela de apostas em milissegundos.
    pub min_window_ms: u32,
    /// Quantidade de spins de espera após um estouro de escada.
    pub cooldown_spins: u32,
    /// Limite máximo de entradas por hora.
    pub max_entries_per_hour: u32,
    /// Denominações de fichas disponíveis em centavos inteiros.
    pub chip_denoms_cents: Vec<u64>,
}

const fn default_number_base_cents() -> u64 {
    10
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            base_cents: 10,
            number_base_cents: 10,
            max_steps: 5,
            max_bet_per_cover_cents: 50000,
            max_exposure_pct: 0.16,
            stop_loss_cents: 7260,
            stop_win_pct: 0.08,
            min_window_ms: 2000,
            cooldown_spins: 2,
            max_entries_per_hour: 45,
            chip_denoms_cents: DEFAULT_CHIP_DENOMS_CENTS.to_vec(),
        }
    }
}

/// Estado instantâneo da mesa e da sessão avaliado pelo gate de risco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Banca atual em centavos inteiros.
    pub bankroll_cents: i64,
    /// Exposição já aberta no spin atual em centavos inteiros.
    pub exposure_cents: u64,
    /// Resultado líquido acumulado na sessão em centavos inteiros.
    pub session_pnl_cents: i64,
    /// Janela de apostas restante em milissegundos.
    pub remaining_window_ms: u32,
    /// Spins decorridos desde o último estouro de escada.
    pub spins_since_blow: u32,
    /// Degrau concluído anteriormente (`0` quando no início da escada).
    pub step: u8,
}

/// Avalia a autorização de aposta obedecendo estritamente a ordem:
/// `StopLoss → LadderBlown → TableLimit → Exposure → Cooldown → Window`.
pub fn authorize(cfg: &Cfg, st: &State) -> Verdict {
    if st.session_pnl_cents <= -cfg.stop_loss_cents {
        warn!(pnl = st.session_pnl_cents, stop_loss = cfg.stop_loss_cents, "gate abort: StopLoss");
        return Verdict::Abort(Abort::StopLoss);
    }

    let next = st.step.saturating_add(1);
    if next > cfg.max_steps {
        warn!(next_step = next, max_steps = cfg.max_steps, "gate abort: LadderBlown");
        return Verdict::Abort(Abort::LadderBlown);
    }

    let stake = stake_cents(next, cfg.base_cents);
    if stake > cfg.max_bet_per_cover_cents {
        warn!(
            stake_cents = stake,
            max_bet_per_cover_cents = cfg.max_bet_per_cover_cents,
            "gate abort: TableLimit"
        );
        return Verdict::Abort(Abort::TableLimit);
    }

    let total_exposure = st.exposure_cents.saturating_add(2 * stake);
    let max_exposure_allowed = (st.bankroll_cents.max(0) as f64) * cfg.max_exposure_pct;
    if st.bankroll_cents <= 0 || (total_exposure as f64) > max_exposure_allowed {
        warn!(
            total_exposure,
            bankroll_cents = st.bankroll_cents,
            max_exposure_pct = cfg.max_exposure_pct,
            "gate abort: Exposure"
        );
        return Verdict::Abort(Abort::Exposure);
    }

    if st.spins_since_blow < cfg.cooldown_spins {
        return Verdict::Skip(Skip::Cooldown);
    }

    if st.remaining_window_ms < cfg.min_window_ms {
        return Verdict::Skip(Skip::WindowTooShort);
    }

    Verdict::Place
}

/// Gate de risco para cobertura de números, com a exposição real `k·x` do degrau.
///
/// Mesma precedência do [`authorize`] de colunas: `StopLoss → LadderBlown → TableLimit
/// → Exposure → Cooldown → Window`. O limite de mesa é checado na ficha por número
/// (é o valor de uma casa do pano); a exposição, no total do degrau (`k·x`).
pub fn authorize_numbers(cfg: &Cfg, nl: &NumberLadder, st: &State) -> Verdict {
    if st.session_pnl_cents <= -cfg.stop_loss_cents {
        warn!(pnl = st.session_pnl_cents, stop_loss = cfg.stop_loss_cents, "gate abort: StopLoss (numeros)");
        return Verdict::Abort(Abort::StopLoss);
    }

    let next = st.step.saturating_add(1);
    if next > cfg.max_steps {
        warn!(next_step = next, max_steps = cfg.max_steps, "gate abort: LadderBlown (numeros)");
        return Verdict::Abort(Abort::LadderBlown);
    }

    let stake = nl.stake_cents(next);
    if stake > cfg.max_bet_per_cover_cents {
        warn!(
            stake_cents = stake,
            max_bet_per_cover_cents = cfg.max_bet_per_cover_cents,
            "gate abort: TableLimit (numeros)"
        );
        return Verdict::Abort(Abort::TableLimit);
    }

    let total_exposure = st.exposure_cents.saturating_add(nl.exposure_cents(next));
    let max_exposure_allowed = (st.bankroll_cents.max(0) as f64) * cfg.max_exposure_pct;
    if st.bankroll_cents <= 0 || (total_exposure as f64) > max_exposure_allowed {
        warn!(
            total_exposure,
            numbers = nl.numbers,
            bankroll_cents = st.bankroll_cents,
            max_exposure_pct = cfg.max_exposure_pct,
            "gate abort: Exposure (numeros)"
        );
        return Verdict::Abort(Abort::Exposure);
    }

    if st.spins_since_blow < cfg.cooldown_spins {
        return Verdict::Skip(Skip::Cooldown);
    }

    if st.remaining_window_ms < cfg.min_window_ms {
        return Verdict::Skip(Skip::WindowTooShort);
    }

    Verdict::Place
}

/// Decompõe `total_cents` nas denominações de fichas de forma greedy decrescente.
/// Retorna pares `(denom_cents, count)` apenas para denominações utilizadas (`count > 0`).
pub fn decompose(total_cents: u64, denoms: &[u64]) -> Vec<(u64, u8)> {
    let mut sorted: Vec<u64> = denoms.iter().copied().filter(|&d| d > 0).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();

    let mut rest = total_cents;
    let mut out = Vec::new();
    for d in sorted {
        if rest == 0 {
            break;
        }
        let count_u64 = rest / d;
        if count_u64 > 0 {
            let count = u8::try_from(count_u64).unwrap_or(u8::MAX);
            out.push((d, count));
            rest -= d * u64::from(count);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandatory_ladder_and_net_profit_invariants() {
        assert_eq!(stake_cents(1,10),10); assert_eq!(stake_cents(2,10),30);
        assert_eq!(stake_cents(3,10),90); assert_eq!(stake_cents(4,10),270);
        assert_eq!(stake_cents(5,10),810); assert_eq!(cum_loss_cents(5,10),2420);
        for k in 1..=5u8 {
          assert_eq!(stake_cents(k,10) as i64 - cum_loss_cents(k-1,10) as i64, 10);
        }
    }

    #[test]
    fn fichas_da_mesa_usam_a_de_20_centavos() {
        let d = DEFAULT_CHIP_DENOMS_CENTS;
        assert_eq!(decompose(20, &d), vec![(20, 1)]);
        assert_eq!(decompose(180, &d), vec![(100, 1), (50, 1), (20, 1), (10, 1)]);
        assert_eq!(decompose(540, &d), vec![(500, 1), (20, 2)]);
        assert_eq!(decompose(1620, &d), vec![(1000, 1), (500, 1), (100, 1), (20, 1)]);
        assert!(!d.contains(&2500), "a mesa não tem ficha de 25");
    }

    #[test]
    fn decompose_810_is_500_3x100_10() {
        let denoms = [2500, 1000, 500, 100, 50, 10];
        assert_eq!(
            decompose(810, &denoms),
            vec![(500, 1), (100, 3), (10, 1)]
        );
    }

    #[test]
    fn property_decompose_reconstructs_amount_in_10_to_5000() {
        let denoms_table = [2500, 1000, 500, 100, 50, 10];
        for x in (10..=5000u64).step_by(10) {
            let chips = decompose(x, &denoms_table);
            let sum: u64 = chips.iter().map(|&(d, c)| d * u64::from(c)).sum();
            assert_eq!(sum, x, "falha de reconstrução em {x} centavos (múltiplo de 10)");
        }

        let denoms_with_cent = [2500, 1000, 500, 100, 50, 10, 1];
        for x in 10..=5000u64 {
            let chips = decompose(x, &denoms_with_cent);
            let sum: u64 = chips.iter().map(|&(d, c)| d * u64::from(c)).sum();
            assert_eq!(sum, x, "falha de reconstrução exata em {x} centavos");
        }
    }

    #[test]
    fn authorize_enforces_strict_precedence_order() {
        let cfg = Cfg::default();
        let valid_state = State {
            bankroll_cents: 15_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 5000,
            spins_since_blow: 5,
            step: 0,
        };
        assert_eq!(authorize(&cfg, &valid_state), Verdict::Place);

        // 1. StopLoss precedes LadderBlown, TableLimit, Exposure, Cooldown, Window
        let st_stop_loss = State {
            session_pnl_cents: -7260,
            step: 5,
            bankroll_cents: 100,
            spins_since_blow: 0,
            remaining_window_ms: 100,
            ..valid_state
        };
        assert_eq!(authorize(&cfg, &st_stop_loss), Verdict::Abort(Abort::StopLoss));

        // 2. LadderBlown precedes TableLimit, Exposure, Cooldown, Window
        let st_ladder = State {
            step: 5,
            bankroll_cents: 100,
            spins_since_blow: 0,
            remaining_window_ms: 100,
            ..valid_state
        };
        assert_eq!(authorize(&cfg, &st_ladder), Verdict::Abort(Abort::LadderBlown));

        // 3. TableLimit precedes Exposure, Cooldown, Window
        let mut cfg_low_limit = cfg.clone();
        cfg_low_limit.max_bet_per_cover_cents = 500;
        let st_table_limit = State {
            step: 4, // next = 5 -> stake = 810 > 500
            bankroll_cents: 100,
            spins_since_blow: 0,
            remaining_window_ms: 100,
            ..valid_state
        };
        assert_eq!(
            authorize(&cfg_low_limit, &st_table_limit),
            Verdict::Abort(Abort::TableLimit)
        );

        // 4. Exposure precedes Cooldown, Window
        let st_exposure = State {
            step: 4, // next = 5 -> stake = 810 -> 2*stake = 1620 > 16% of 5000 (800)
            bankroll_cents: 5000,
            spins_since_blow: 0,
            remaining_window_ms: 100,
            ..valid_state
        };
        assert_eq!(authorize(&cfg, &st_exposure), Verdict::Abort(Abort::Exposure));

        // 5. Cooldown precedes Window
        let st_cooldown = State {
            spins_since_blow: 1,
            remaining_window_ms: 500,
            ..valid_state
        };
        assert_eq!(authorize(&cfg, &st_cooldown), Verdict::Skip(Skip::Cooldown));

        // 6. Window
        let st_window = State {
            remaining_window_ms: 1999,
            ..valid_state
        };
        assert_eq!(
            authorize(&cfg, &st_window),
            Verdict::Skip(Skip::WindowTooShort)
        );
    }

    /// Escada de números: um acerto em qualquer degrau devolve ao menos `base_cents`
    /// de lucro líquido, sem estourar a exposição do ciclo.
    #[test]
    fn number_ladder_win_always_returns_at_least_base_profit() {
        for k in 3..=15u8 {
            for base in (10..=100u64).step_by(10) {
                let nl = NumberLadder::new(k, base);
                for step in 1..=8u8 {
                    assert!(
                        nl.win_profit_cents(step) >= base as i64,
                        "k={k} base={base} step={step}: lucro {} < base {base}",
                        nl.win_profit_cents(step)
                    );
                }
            }
        }
    }

    /// A ficha por número nunca diminui ao subir degrau (senão a escada não recupera).
    #[test]
    fn number_ladder_stake_is_monotonic_in_step() {
        for k in 1..=20u8 {
            let nl = NumberLadder::new(k, 10);
            for step in 1..=12u8 {
                assert!(
                    nl.stake_cents(step + 1) >= nl.stake_cents(step),
                    "k={k}: degrau {} maior que degrau {}",
                    step + 1,
                    step
                );
            }
        }
    }

    /// Degrau 1 nunca desce do piso da ficha (10¢) e toda ficha é decomponível.
    #[test]
    fn number_ladder_step_one_is_min_chip_and_decomposable() {
        let denoms = [2500, 1000, 500, 100, 50, 10];
        let nl = NumberLadder::new(15, 10);
        assert_eq!(nl.stake_cents(1), 10);
        for step in 1..=10u8 {
            let x = nl.stake_cents(step);
            let chips = decompose(x, &denoms);
            let sum: u64 = chips.iter().map(|&(d, c)| d * u64::from(c)).sum();
            assert_eq!(sum, x, "ficha {x} do degrau {step} não decomponível");
        }
    }

    /// O terminal (k=4) recupera com a regra `(D+G)/(36-k)`, não com a ×3 de duas colunas.
    #[test]
    fn number_ladder_does_not_reuse_the_x3_ladder() {
        let nl = NumberLadder::new(15, 10);
        // Perder o degrau 1 custa 15 x 10 = 150¢.
        assert_eq!(nl.cum_loss_cents(1), 150);
        // Com ×3 a ficha do degrau 2 seria 30¢ por número: 450¢ de exposição para recuperar 150¢.
        assert!(nl.exposure_cents(2) < 15 * stake_cents(2, 10));
        // Confirma que o lucro do acerto cobre a perda acumulada com folga mínima.
        assert!(nl.win_profit_cents(2) >= 10);
    }

    /// O limite de mesa corta a escada no último degrau que ainda cabe.
    #[test]
    fn number_ladder_reports_last_step_within_table_limit() {
        let nl = NumberLadder::new(15, 10);
        assert_eq!(nl.stake_cents(4), 30);
        assert_eq!(nl.stake_cents(5), 60);
        assert_eq!(nl.steps_within_limit(50), Some(4));
        assert_eq!(nl.steps_within_limit(5), None);
    }

    /// `authorize_numbers` mantém a precedência do gate de colunas.
    #[test]
    fn authorize_numbers_keeps_strict_precedence_order() {
        let cfg = Cfg::default();
        let nl = NumberLadder::new(15, 10);
        let base = State {
            bankroll_cents: 200_000,
            exposure_cents: 0,
            session_pnl_cents: 0,
            remaining_window_ms: 5000,
            spins_since_blow: 5,
            step: 0,
        };
        assert_eq!(authorize_numbers(&cfg, &nl, &base), Verdict::Place);

        let stop = State { session_pnl_cents: -cfg.stop_loss_cents, ..base };
        assert_eq!(
            authorize_numbers(&cfg, &nl, &stop),
            Verdict::Abort(Abort::StopLoss)
        );

        let blown = State { step: cfg.max_steps, ..base };
        assert_eq!(
            authorize_numbers(&cfg, &nl, &blown),
            Verdict::Abort(Abort::LadderBlown)
        );

        let mut low_limit = cfg.clone();
        low_limit.max_bet_per_cover_cents = 5;
        assert_eq!(
            authorize_numbers(&low_limit, &nl, &base),
            Verdict::Abort(Abort::TableLimit)
        );

        let poor = State { bankroll_cents: 100, ..base };
        assert_eq!(
            authorize_numbers(&cfg, &nl, &poor),
            Verdict::Abort(Abort::Exposure)
        );

        let cooling = State { spins_since_blow: 1, remaining_window_ms: 100, ..base };
        assert_eq!(
            authorize_numbers(&cfg, &nl, &cooling),
            Verdict::Skip(Skip::Cooldown)
        );

        let late = State { remaining_window_ms: 1999, ..base };
        assert_eq!(
            authorize_numbers(&cfg, &nl, &late),
            Verdict::Skip(Skip::WindowTooShort)
        );
    }

    #[test]
    fn monte_carlo_1h_and_8h_gates_have_p_ruin_below_5_percent() {
        let params_1h = McParams::default();
        let report_1h = run_monte_carlo(&params_1h);
        assert_eq!(report_1h.sessions, 10_000);
        assert!(
            report_1h.p_ruin < 0.05,
            "P(ruína 1h) = {:.4} excede o gate de 5%",
            report_1h.p_ruin
        );

        let params_8h = McParams {
            hours_per_session: 8,
            ..McParams::default()
        };
        let report_8h = run_monte_carlo(&params_8h);
        assert_eq!(report_8h.sessions, 10_000);
        assert!(
            report_8h.p_ruin < 0.05,
            "P(ruína 8h) = {:.4} excede o gate de 5%",
            report_8h.p_ruin
        );
    }
}
