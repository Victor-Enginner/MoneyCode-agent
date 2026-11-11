// SPDX-License-Identifier: Apache-2.0
//! Simulador de Monte Carlo determinístico para validação de risco da escada ×3.
//! Executa 10.000 sessões simuladas com aritmética inteira em centavos para banca e drawdown.
//! Verifica os gates da Fase 4: P(ruína 1h) < 5% e P(ruína 8h) < 5%.

use crate::{authorize, stake_cents, Cfg, State, Verdict};
use serde::{Deserialize, Serialize};

/// Parâmetros de entrada da simulação de Monte Carlo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McParams {
    /// Quantidade de sessões independentes a simular (mínimo recomendado: 10.000).
    pub sessions: u32,
    /// Duração de cada sessão em horas (ex.: 1 ou 8).
    pub hours_per_session: u32,
    /// Quantidade média de giros da Roleta Brasileira por hora (~60).
    pub spins_per_hour: u32,
    /// Probabilidade de um giro disparar sinal de entrada quando no degrau 0.
    pub entry_rate: f64,
    /// Banca inicial da sessão em centavos inteiros (ex.: 15_000 = R$ 150,00).
    pub initial_bankroll_cents: i64,
    /// Configuração do gate de risco.
    pub cfg: Cfg,
    /// Semente determinística do gerador pseudo-aleatório Xorshift64*.
    pub seed: u64,
}

impl Default for McParams {
    fn default() -> Self {
        Self {
            sessions: 10_000,
            hours_per_session: 1,
            spins_per_hour: 60,
            entry_rate: 0.035,
            initial_bankroll_cents: 45_000,
            cfg: Cfg::default(),
            seed: 0x4D4F_4E53_5445_5231,
        }
    }
}

/// Relatório consolidado da simulação de Monte Carlo (§13 e §15.4).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct McReport {
    /// Total de sessões simuladas.
    pub sessions: u32,
    /// Horas simuladas por sessão.
    pub hours_per_session: u32,
    /// Quantidade de sessões que atingiram stop-loss ou insolvência.
    pub ruined_sessions: u32,
    /// Probabilidade observada de ruína (`0.0..=1.0`).
    pub p_ruin: f64,
    /// Drawdown no percentil 95 em centavos inteiros.
    pub drawdown_p95_cents: u64,
    /// Valor esperado observado por hora em centavos (`f64`).
    pub ev_per_hour_cents: f64,
}

struct XorShift64Star {
    state: u64,
}

impl XorShift64Star {
    const fn new(seed: u64) -> Self {
        let s = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
        Self { state: s }
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
        let bits = self.next_u64() >> 11;
        (bits as f64) * (1.0 / ((1u64 << 53) as f64))
    }

    fn next_pocket(&mut self) -> u8 {
        (self.next_u64() % 37) as u8
    }
}

/// Executa a simulação de Monte Carlo para a configuração fornecida.
pub fn run_monte_carlo(params: &McParams) -> McReport {
    let total_sessions = params.sessions.max(1);
    let spins_per_session = params.hours_per_session.max(1) * params.spins_per_hour.max(1);
    let stop_win_target_cents =
        ((params.initial_bankroll_cents.max(0) as f64) * params.cfg.stop_win_pct).round() as i64;

    let mut rng = XorShift64Star::new(params.seed);
    let mut ruined_sessions = 0u32;
    let mut total_pnl_cents: i128 = 0;
    let mut max_drawdowns: Vec<u64> = Vec::with_capacity(total_sessions as usize);

    for _ in 0..total_sessions {
        let mut bankroll_cents = params.initial_bankroll_cents;
        let mut session_pnl_cents = 0i64;
        let mut peak_pnl_cents = 0i64;
        let mut max_dd_cents = 0u64;
        let mut step = 0u8;
        let mut spins_since_blow = params.cfg.cooldown_spins;
        let mut ruined = false;

        for _ in 0..spins_per_session {
            if session_pnl_cents <= -params.cfg.stop_loss_cents || bankroll_cents <= 0 {
                ruined = true;
                break;
            }
            if stop_win_target_cents > 0 && session_pnl_cents >= stop_win_target_cents {
                break;
            }

            let in_progression = step > 0;
            let triggered = in_progression || (rng.next_f64() < params.entry_rate);
            if !triggered {
                spins_since_blow = spins_since_blow.saturating_add(1);
                continue;
            }

            let st = State {
                bankroll_cents,
                exposure_cents: 0,
                session_pnl_cents,
                remaining_window_ms: params.cfg.min_window_ms + 3000,
                spins_since_blow,
                step,
            };

            match authorize(&params.cfg, &st) {
                Verdict::Place => {
                    let next_step = step + 1;
                    let stake = stake_cents(next_step, params.cfg.base_cents);
                    let pocket = rng.next_pocket();
                    // Cobrir 2 colunas (24 números vencedores: 1..=24; 13 perdedores: 0 e 25..=36).
                    let win = (1..=24).contains(&pocket);
                    if win {
                        let gain = stake as i64;
                        bankroll_cents += gain;
                        session_pnl_cents += gain;
                        step = 0;
                        spins_since_blow = spins_since_blow.saturating_add(1);
                    } else {
                        let loss = (2 * stake) as i64;
                        bankroll_cents -= loss;
                        session_pnl_cents -= loss;
                        if next_step >= params.cfg.max_steps {
                            step = 0;
                            spins_since_blow = 0;
                        } else {
                            step = next_step;
                            spins_since_blow = spins_since_blow.saturating_add(1);
                        }
                    }

                    if session_pnl_cents > peak_pnl_cents {
                        peak_pnl_cents = session_pnl_cents;
                    }
                    let dd = (peak_pnl_cents - session_pnl_cents).max(0) as u64;
                    if dd > max_dd_cents {
                        max_dd_cents = dd;
                    }

                    if session_pnl_cents <= -params.cfg.stop_loss_cents || bankroll_cents <= 0 {
                        ruined = true;
                        break;
                    }
                }
                Verdict::Skip(_) => {
                    spins_since_blow = spins_since_blow.saturating_add(1);
                }
                Verdict::Abort(_) => {
                    ruined = true;
                    break;
                }
            }
        }

        if ruined {
            ruined_sessions += 1;
        }
        total_pnl_cents += i128::from(session_pnl_cents);
        max_drawdowns.push(max_dd_cents);
    }

    max_drawdowns.sort_unstable();
    let p95_idx = (((total_sessions as usize) * 95) / 100).min(max_drawdowns.len() - 1);
    let drawdown_p95_cents = max_drawdowns[p95_idx];
    let p_ruin = (ruined_sessions as f64) / (total_sessions as f64);
    let total_hours = (total_sessions as f64) * (params.hours_per_session.max(1) as f64);
    let ev_per_hour_cents = (total_pnl_cents as f64) / total_hours;

    McReport {
        sessions: total_sessions,
        hours_per_session: params.hours_per_session.max(1),
        ruined_sessions,
        p_ruin,
        drawdown_p95_cents,
        ev_per_hour_cents,
    }
}
