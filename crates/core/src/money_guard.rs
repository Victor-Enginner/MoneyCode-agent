// SPDX-License-Identifier: Apache-2.0
//! Vigia de dinheiro: banca zerada (para tudo até o depósito) e stop loss móvel a partir de R$ 100.
//! O piso é 80% do melhor patrimônio da sessão, medido sem saques e depósitos; rompido, só
//! entradas NOVAS param (gale em andamento termina). Nunca move dinheiro.

/// Abaixo disto a banca não paga nem a entrada mais barata: alerta de banca zerada.
pub const BANK_EMPTY_CENTS: i64 = 100;
/// Depósito que rearma o bot depois de banca zerada.
pub const BANK_REARM_CENTS: i64 = 1_000;
/// Patrimônio a partir do qual o stop loss passa a valer (decisão do operador em 17/11/2026).
pub const FLOOR_ARM_CENTS: i64 = 10_000;
/// Fração do melhor patrimônio que o piso protege.
pub const FLOOR_PCT: i64 = 80;

/// Por que o executor está parado pelo vigia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    BankEmpty,
}

/// O que o loop deve fazer com a leitura de agora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardEvent {
    None,
    /// Cortar o executor e mostrar o motivo.
    Halt { kind: Halt, message: String },
    /// Rearmar depois do depósito.
    Rearm { message: String },
    /// Piso rompido: parar de abrir entradas novas (gale em andamento termina).
    FloorBreached { message: String },
    /// Proteção ligou (patrimônio chegou a R$ 100).
    FloorArmed { message: String },
}

#[derive(Debug, Default)]
pub struct MoneyGuard {
    halted: Option<Halt>,
    peak_equity: i64,
    floor_hit: bool,
    armed: bool,
}

impl MoneyGuard {
    pub fn halted(&self) -> Option<Halt> {
        self.halted
    }

    /// Piso rompido e ainda sem `/resume`: entradas novas ficam bloqueadas.
    pub fn floor_hit(&self) -> bool {
        self.floor_hit
    }

    /// Piso atual em centavos (`None` = proteção ainda não ligada).
    pub fn floor(&self) -> Option<i64> {
        self.armed.then(|| self.peak_equity * FLOOR_PCT / 100)
    }

    /// `bank` = saldo real (0 = ainda não lido); `external` = soma dos saques/depósitos da sessão
    /// (o patrimônio do bot é `bank − external`); `floor_still_on` = a parada do piso continua
    /// ativa (o `/resume` do operador a desliga, e o piso recomeça do patrimônio de agora).
    pub fn check(&mut self, bank: i64, external: i64, floor_still_on: bool) -> GuardEvent {
        if self.halted == Some(Halt::BankEmpty) {
            if bank >= BANK_REARM_CENTS {
                self.halted = None;
                return GuardEvent::Rearm {
                    message: format!("banca reabastecida (R$ {:.2}) — bot rearmado", bank as f64 / 100.0),
                };
            }
            return GuardEvent::None;
        }
        if bank > 0 && bank < BANK_EMPTY_CENTS {
            self.halted = Some(Halt::BankEmpty);
            return GuardEvent::Halt {
                kind: Halt::BankEmpty,
                message: format!(
                    "BANCA ZERADA — saldo R$ {:.2}. Executor parado; deposite para rearmar.",
                    bank as f64 / 100.0
                ),
            };
        }
        if bank <= 0 {
            return GuardEvent::None;
        }
        let equity = bank - external;
        if self.floor_hit && !floor_still_on {
            // Operador retomou: o piso recomeça do patrimônio de agora.
            self.floor_hit = false;
            self.peak_equity = equity;
        }
        self.peak_equity = self.peak_equity.max(equity);
        if !self.armed && self.peak_equity >= FLOOR_ARM_CENTS {
            self.armed = true;
            return GuardEvent::FloorArmed {
                message: format!(
                    "banca chegou a R$ {:.2} — stop loss ligado: piso de R$ {:.2} (80% do melhor valor)",
                    self.peak_equity as f64 / 100.0,
                    (self.peak_equity * FLOOR_PCT / 100) as f64 / 100.0
                ),
            };
        }
        if let Some(floor) = self.floor() {
            if !self.floor_hit && equity < floor {
                self.floor_hit = true;
                return GuardEvent::FloorBreached {
                    message: format!(
                        "STOP LOSS — banca R$ {:.2} abaixo do piso R$ {:.2} (80% do melhor, R$ {:.2}). Sem entradas novas; gale em andamento termina. /resume para voltar.",
                        equity as f64 / 100.0,
                        floor as f64 / 100.0,
                        self.peak_equity as f64 / 100.0
                    ),
                };
            }
        }
        GuardEvent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abaixo_de_100_nao_tem_stop_loss() {
        let mut g = MoneyGuard::default();
        for bank in [4_000, 9_000, 2_000, 600] {
            assert_eq!(g.check(bank, 0, false), GuardEvent::None, "banca {bank}");
        }
        assert_eq!(g.floor(), None);
    }

    #[test]
    fn piso_liga_em_100_acompanha_o_melhor_e_nunca_desce() {
        let mut g = MoneyGuard::default();
        assert!(matches!(g.check(10_000, 0, false), GuardEvent::FloorArmed { .. }));
        assert_eq!(g.floor(), Some(8_000));
        assert_eq!(g.check(15_000, 0, false), GuardEvent::None);
        assert_eq!(g.floor(), Some(12_000), "subiu para 150: piso 120");
        assert_eq!(g.check(13_000, 0, false), GuardEvent::None, "130 ainda acima de 120");
        assert_eq!(g.floor(), Some(12_000), "piso não desce");
        assert!(matches!(g.check(11_900, 0, false), GuardEvent::FloorBreached { .. }));
        assert_eq!(g.check(11_000, 0, true), GuardEvent::None, "avisa uma vez só");
    }

    #[test]
    fn saque_nao_dispara_o_stop() {
        let mut g = MoneyGuard::default();
        let _ = g.check(15_000, 0, false);
        // operador sacou R$ 100: saldo 50, mas o patrimônio do bot segue 150
        assert_eq!(g.check(5_000, -10_000, false), GuardEvent::None);
    }

    #[test]
    fn resume_recomeca_o_piso_de_agora() {
        let mut g = MoneyGuard::default();
        let _ = g.check(10_000, 0, false);
        assert!(matches!(g.check(7_900, 0, false), GuardEvent::FloorBreached { .. }));
        // /resume: piso volta a 80% de 79
        assert_eq!(g.check(7_900, 0, false), GuardEvent::None);
        assert_eq!(g.floor(), Some(6_320));
    }

    #[test]
    fn banca_zerada_para_e_so_rearma_com_deposito() {
        let mut g = MoneyGuard::default();
        assert!(matches!(g.check(17, 0, false), GuardEvent::Halt { kind: Halt::BankEmpty, .. }));
        assert_eq!(g.check(17, 0, false), GuardEvent::None);
        assert_eq!(g.check(500, 0, false), GuardEvent::None, "R$ 5 não rearma");
        assert!(matches!(g.check(4_017, 4_000, false), GuardEvent::Rearm { .. }));
    }

    #[test]
    fn saldo_nao_lido_nao_e_banca_zerada() {
        let mut g = MoneyGuard::default();
        assert_eq!(g.check(0, 0, false), GuardEvent::None);
    }
}
