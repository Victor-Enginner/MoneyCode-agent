// SPDX-License-Identifier: Apache-2.0
//! Monte Carlo da escada de números (pleno e terminal) sobre 10.000 sessões.
//! Mede o que a conta fechada não mostra: frequência de estouro por ciclo, drawdown p95,
//! P(ruína) e EV/h com a geometria real `k·x` de exposição.
//! É o gate do §13 antes de qualquer `--live` em cobertura de números.

use crate::{authorize_numbers, Cfg, NumberLadder, State, Verdict};
use serde::{Deserialize, Serialize};

/// P(ruína) máxima aceita para autorizar operação com cobertura de números (§13).
pub const P_RUIN_GATE: f64 = 0.05;

/// Parâmetros da simulação de números. `cfg.max_steps` é o que se está decidindo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumbersMcParams {
    /// Sessões independentes a simular (recomendado: 10.000).
    pub sessions: u32,
    /// Duração de cada sessão em horas.
    pub hours_per_session: u32,
    /// Giros por hora da mesa (Roleta Brasileira: ~60).
    pub spins_per_hour: u32,
    /// Probabilidade de um giro disparar sinal quando a escada está zerada.
    pub entry_rate: f64,
    /// Banca inicial em centavos (o `--bank-cents` da sessão real).
    pub initial_bankroll_cents: i64,
    /// Números cobertos por entrada (`k` de `NumberLadder`; cluster 1-4-7 + 0 = 15).
    pub numbers: u8,
    /// Configuração de risco vigente (o `max_steps` daqui é o que o relatório julga).
    pub cfg: Cfg,
    /// Semente determinística: mesma semente, mesmo relatório.
    pub seed: u64,
}

impl Default for NumbersMcParams {
    fn default() -> Self {
        Self {
            sessions: 10_000,
            hours_per_session: 1,
            spins_per_hour: 60,
            entry_rate: 0.035,
            initial_bankroll_cents: 45_000,
            numbers: 15,
            cfg: Cfg::default(),
            seed: 0x4E55_4D45_524F_5331,
        }
    }
}

/// Relatório do MC de números: risco de sobrevivência, não previsão de lucro.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NumbersMcReport {
    /// Sessões simuladas.
    pub sessions: u32,
    /// Horas por sessão.
    pub hours_per_session: u32,
    /// Números cobertos por entrada.
    pub numbers: u8,
    /// Degraus máximos permitidos na simulação (`cfg.max_steps`).
    pub max_steps: u8,
    /// Entradas efetivamente colocadas.
    pub entries: u64,
    /// Ciclos fechados (um acerto ou um estouro).
    pub cycles: u64,
    /// Estouros de escada (perdeu todos os degraus seguidos).
    pub blows: u64,
    /// P(estourar a escada em um ciclo).
    pub p_blow_per_cycle: f64,
    /// Sessões que bateram stop loss, insolvência ou aborto de risco.
    pub ruined_sessions: u32,
    /// P(ruína) observada.
    pub p_ruin: f64,
    /// Drawdown no percentil 95 em centavos.
    pub drawdown_p95_cents: u64,
    /// Maior exposição de um único degrau em centavos.
    pub max_exposure_cents: u64,
    /// Total apostado nas sessões simuladas em centavos (denominador do EV).
    pub total_staked_cents: u64,
    /// EV observado por hora em centavos (negativo por construção).
    pub ev_per_hour_cents: f64,
    /// Entradas por hora.
    pub entries_per_hour: f64,
}

impl NumbersMcReport {
    /// Gate do §13: `P(ruína) < 5%` com a configuração simulada.
    pub fn gate_passes(&self) -> bool {
        self.p_ruin < P_RUIN_GATE
    }

    /// EV em fração do total apostado (deve convergir para `-2/37 ≈ -0.054` por rodada
    /// e `-0.027` por unidade apostada — a borda da casa).
    pub fn ev_pct_of_staked(&self) -> f64 {
        if self.total_staked_cents == 0 {
            return 0.0;
        }
        let total_pnl = self.ev_per_hour_cents
            * (f64::from(self.sessions) * f64::from(self.hours_per_session));
        total_pnl / (self.total_staked_cents as f64)
    }
}

struct XorShift64Star {
    state: u64,
}

impl XorShift64Star {
    const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / ((1u64 << 53) as f64))
    }

    /// Sorteia `0..37` e diz se caiu nos `k` números cobertos.
    /// É a probabilidade exata `k/37` sem precisar saber *quais* números são: o caixa
    /// não distingue cobertura, só paga ou não paga.
    fn hits(&mut self, numbers: u64) -> bool {
        (self.next_u64() % 37) < numbers
    }
}

/// Executa o MC de números com a escada e o gate reais.
pub fn run_monte_carlo_numbers(params: &NumbersMcParams) -> NumbersMcReport {
    let sessions = params.sessions.max(1);
    let hours = params.hours_per_session.max(1);
    let spins_per_session = hours * params.spins_per_hour.max(1);
    let k = u64::from(params.numbers.clamp(1, crate::MAX_NUMBER_COVERS));
    let nl = NumberLadder::new(params.numbers.clamp(1, crate::MAX_NUMBER_COVERS), params.cfg.base_cents);
    let stop_win_cents =
        (params.initial_bankroll_cents.max(0) as f64 * params.cfg.stop_win_pct).round() as i64;

    let mut rng = XorShift64Star::new(params.seed);
    let mut ruined_sessions = 0u32;
    let mut total_pnl: i128 = 0;
    let mut entries = 0u64;
    let mut cycles = 0u64;
    let mut blows = 0u64;
    let mut staked = 0u64;
    let mut max_exposure = 0u64;
    let mut drawdowns: Vec<u64> = Vec::with_capacity(sessions as usize);

    for _ in 0..sessions {
        let mut bankroll = params.initial_bankroll_cents;
        let mut pnl = 0i64;
        let mut peak = 0i64;
        let mut max_dd = 0u64;
        let mut step = 0u8;
        let mut spins_since_blow = params.cfg.cooldown_spins;
        let mut ruined = false;

        for _ in 0..spins_per_session {
            if pnl <= -params.cfg.stop_loss_cents || bankroll <= 0 {
                ruined = true;
                break;
            }
            if stop_win_cents > 0 && pnl >= stop_win_cents {
                break;
            }

            let in_progression = step > 0;
            if !in_progression && rng.next_f64() >= params.entry_rate {
                spins_since_blow = spins_since_blow.saturating_add(1);
                continue;
            }

            let st = State {
                bankroll_cents: bankroll,
                exposure_cents: 0,
                session_pnl_cents: pnl,
                remaining_window_ms: params.cfg.min_window_ms + 3000,
                spins_since_blow,
                step,
            };

            match authorize_numbers(&params.cfg, &nl, &st) {
                Verdict::Place => {
                    let next = step.saturating_add(1);
                    let x = nl.stake_cents(next);
                    let exposure = nl.exposure_cents(next);
                    let cost = exposure as i64;
                    entries += 1;
                    staked = staked.saturating_add(exposure);
                    if exposure > max_exposure {
                        max_exposure = exposure;
                    }

                    if rng.hits(k) {
                        // O acerto devolve 36x (35:1 + a ficha da casa vencedora) e custou k·x:
                        // o líquido da rodada é (36−k)·x. As perdas dos degraus anteriores já
                        // saíram do caixa quando aconteceram — subtraí-las de novo aqui
                        // contaria a mesma perda duas vezes.
                        let gain = (36 - k as i64) * x as i64;
                        bankroll += gain;
                        pnl += gain;
                        step = 0;
                        cycles += 1;
                        spins_since_blow = spins_since_blow.saturating_add(1);
                    } else {
                        bankroll -= cost;
                        pnl -= cost;
                        if next >= params.cfg.max_steps {
                            step = 0;
                            cycles += 1;
                            blows += 1;
                            spins_since_blow = 0;
                        } else {
                            step = next;
                            spins_since_blow = spins_since_blow.saturating_add(1);
                        }
                    }

                    if pnl > peak {
                        peak = pnl;
                    }
                    let dd = (peak - pnl).max(0) as u64;
                    if dd > max_dd {
                        max_dd = dd;
                    }

                    if pnl <= -params.cfg.stop_loss_cents || bankroll <= 0 {
                        ruined = true;
                        break;
                    }
                }
                Verdict::Skip(_) => {
                    spins_since_blow = spins_since_blow.saturating_add(1);
                }
                // Aborto (stop loss, escada estourada, mesa, exposição): sessão encerrada.
                Verdict::Abort(_) => {
                    ruined = true;
                    break;
                }
            }
        }

        if ruined {
            ruined_sessions += 1;
        }
        total_pnl += i128::from(pnl);
        drawdowns.push(max_dd);
    }

    drawdowns.sort_unstable();
    let p95_idx = ((sessions as usize * 95) / 100).min(drawdowns.len() - 1);
    let total_hours = f64::from(sessions) * f64::from(hours);
    let ev_per_hour_cents = total_pnl as f64 / total_hours;

    NumbersMcReport {
        sessions,
        hours_per_session: hours,
        numbers: params.numbers.clamp(1, crate::MAX_NUMBER_COVERS),
        max_steps: params.cfg.max_steps,
        entries,
        cycles,
        blows,
        p_blow_per_cycle: if cycles == 0 {
            0.0
        } else {
            blows as f64 / cycles as f64
        },
        ruined_sessions,
        p_ruin: f64::from(ruined_sessions) / f64::from(sessions),
        drawdown_p95_cents: drawdowns[p95_idx],
        max_exposure_cents: max_exposure,
        total_staked_cents: staked,
        ev_per_hour_cents,
        entries_per_hour: entries as f64 / total_hours,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loose_cfg(max_steps: u8) -> Cfg {
        Cfg {
            max_steps,
            stop_loss_cents: 100_000_000,
            max_exposure_pct: 1.0,
            max_bet_per_cover_cents: 1_000_000,
            cooldown_spins: 0,
            stop_win_pct: 0.0,
            ..Cfg::default()
        }
    }

    #[test]
    fn blow_rate_matches_the_theory() {
        // Só a escada importa: banca enorme, sem stop loss, entrada a cada giro.
        let params = NumbersMcParams {
            sessions: 2_000,
            hours_per_session: 1,
            spins_per_hour: 2_000,
            entry_rate: 1.0,
            initial_bankroll_cents: 10_000_000,
            numbers: 15,
            cfg: loose_cfg(5),
            seed: 7,
        };
        let rep = run_monte_carlo_numbers(&params);
        let theory = (22.0f64 / 37.0).powi(5);
        assert!(
            (rep.p_blow_per_cycle - theory).abs() < 0.02,
            "P(estouro) observada {:.4} deveria bater com (22/37)^5 = {:.4}",
            rep.p_blow_per_cycle,
            theory
        );
        assert!(rep.blows > 0 && rep.cycles > rep.blows);
    }

    #[test]
    fn fewer_steps_blow_more_often_but_cost_less() {
        let base = NumbersMcParams {
            sessions: 1_000,
            hours_per_session: 1,
            spins_per_hour: 1_000,
            entry_rate: 1.0,
            initial_bankroll_cents: 10_000_000,
            numbers: 15,
            seed: 11,
            ..NumbersMcParams::default()
        };
        let three = run_monte_carlo_numbers(&NumbersMcParams {
            cfg: loose_cfg(3),
            ..base.clone()
        });
        let five = run_monte_carlo_numbers(&NumbersMcParams {
            cfg: loose_cfg(5),
            ..base
        });
        assert!(
            three.p_blow_per_cycle > five.p_blow_per_cycle,
            "3 degraus devem estourar mais que 5: {:.3} vs {:.3}",
            three.p_blow_per_cycle,
            five.p_blow_per_cycle
        );
        // O custo de um estouro de 5 degraus é maior: exposição máxima cresce com o degrau.
        assert!(five.max_exposure_cents > three.max_exposure_cents);
    }

    #[test]
    fn ev_tracks_the_house_edge() {
        let params = NumbersMcParams {
            sessions: 3_000,
            hours_per_session: 1,
            spins_per_hour: 600,
            entry_rate: 1.0,
            initial_bankroll_cents: 10_000_000,
            numbers: 15,
            cfg: loose_cfg(4),
            seed: 23,
        };
        let rep = run_monte_carlo_numbers(&params);
        assert!(rep.ev_per_hour_cents < 0.0, "roleta é -EV por construção");
        let edge = rep.ev_pct_of_staked();
        assert!(
            (edge + 0.027).abs() < 0.015,
            "EV/unidade apostada deveria ficar perto de -2.7% (1/37), veio {:.4}",
            edge
        );
    }

    #[test]
    fn gate_rejects_a_ruinous_configuration() {
        let params = NumbersMcParams {
            sessions: 500,
            hours_per_session: 1,
            spins_per_hour: 200,
            entry_rate: 0.5,
            initial_bankroll_cents: 1_000,
            numbers: 15,
            cfg: Cfg {
                stop_loss_cents: 300,
                max_exposure_pct: 1.0,
                ..loose_cfg(4)
            },
            seed: 5,
        };
        let rep = run_monte_carlo_numbers(&params);
        assert!(rep.p_ruin > P_RUIN_GATE, "banca de R$10 com stop de R$3 quebra");
        assert!(!rep.gate_passes());
    }

    #[test]
    fn same_seed_same_report() {
        let p = NumbersMcParams {
            sessions: 200,
            ..NumbersMcParams::default()
        };
        assert_eq!(
            run_monte_carlo_numbers(&p),
            run_monte_carlo_numbers(&p)
        );
    }
}
