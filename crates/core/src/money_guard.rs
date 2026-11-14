// SPDX-License-Identifier: Apache-2.0
//! Vigia de dinheiro: meta de lucro (pausa para o operador sacar) e banca zerada (alerta geral).
//! Nunca move dinheiro: só corta o executor (kill switch) e avisa painel e Telegram.
//! Saque e depósito ficam com o operador; o P&L usado aqui já exclui movimentos externos.

/// Lucro que dispara a pausa para saque (centavos).
pub const PROFIT_TARGET_CENTS: i64 = 5_000;
/// Abaixo disto a banca não paga nem a entrada mais barata: alerta de banca zerada.
pub const BANK_EMPTY_CENTS: i64 = 100;
/// Depósito que rearma o bot depois de banca zerada.
pub const BANK_REARM_CENTS: i64 = 1_000;

/// Por que o executor está parado pelo vigia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    ProfitTarget,
    BankEmpty,
}

/// O que o loop deve fazer com a leitura de agora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardEvent {
    None,
    /// Cortar o executor e mostrar o motivo.
    Halt { kind: Halt, message: String },
    /// Rearmar (só depois de banca zerada + depósito).
    Rearm { message: String },
    /// O operador retomou depois da meta: nada a fazer além de registrar.
    Resumed,
}

#[derive(Debug, Default)]
pub struct MoneyGuard {
    meta_base_pnl: i64,
    halted: Option<Halt>,
}

impl MoneyGuard {
    pub fn halted(&self) -> Option<Halt> {
        self.halted
    }

    /// `bank` = saldo real (0 = ainda não lido); `pnl` = P&L real sem saque/depósito;
    /// `kill_tripped` = kill switch acionado agora (o `/resume` do operador o desarma).
    pub fn check(&mut self, bank: i64, pnl: i64, kill_tripped: bool) -> GuardEvent {
        match self.halted {
            Some(Halt::BankEmpty) => {
                if bank >= BANK_REARM_CENTS {
                    self.halted = None;
                    self.meta_base_pnl = pnl;
                    return GuardEvent::Rearm {
                        message: format!("banca reabastecida (R$ {:.2}) — bot rearmado", bank as f64 / 100.0),
                    };
                }
                return GuardEvent::None;
            }
            Some(Halt::ProfitTarget) => {
                if !kill_tripped {
                    self.halted = None;
                    return GuardEvent::Resumed;
                }
                return GuardEvent::None;
            }
            None => {}
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
        if pnl - self.meta_base_pnl >= PROFIT_TARGET_CENTS {
            self.meta_base_pnl = pnl;
            self.halted = Some(Halt::ProfitTarget);
            return GuardEvent::Halt {
                kind: Halt::ProfitTarget,
                message: format!(
                    "META ATINGIDA +R$ {:.2} — executor pausado. Faça o saque e mande /resume.",
                    PROFIT_TARGET_CENTS as f64 / 100.0
                ),
            };
        }
        GuardEvent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_de_50_pausa_e_a_proxima_meta_conta_de_onde_parou() {
        let mut g = MoneyGuard::default();
        assert_eq!(g.check(4_000, 4_990, false), GuardEvent::None);
        assert!(matches!(g.check(9_000, 5_000, false), GuardEvent::Halt { kind: Halt::ProfitTarget, .. }));
        // ainda pausado: nada
        assert_eq!(g.check(4_000, 5_000, true), GuardEvent::None);
        // operador sacou (P&L não muda) e mandou /resume
        assert_eq!(g.check(4_000, 5_000, false), GuardEvent::Resumed);
        assert_eq!(g.check(4_100, 9_900, false), GuardEvent::None, "+49 desde a meta");
        assert!(matches!(g.check(4_200, 10_000, false), GuardEvent::Halt { kind: Halt::ProfitTarget, .. }));
    }

    #[test]
    fn banca_zerada_para_e_so_rearma_com_deposito() {
        let mut g = MoneyGuard::default();
        assert!(matches!(g.check(17, -3_983, false), GuardEvent::Halt { kind: Halt::BankEmpty, .. }));
        assert_eq!(g.check(17, -3_983, true), GuardEvent::None);
        assert_eq!(g.check(500, -3_983, true), GuardEvent::None, "R$ 5 não rearma");
        assert!(matches!(g.check(4_017, -3_983, true), GuardEvent::Rearm { .. }));
        // P&L negativo antigo não dispara meta: a base foi para o ponto do rearme
        assert_eq!(g.check(4_017, -3_983, false), GuardEvent::None);
    }

    #[test]
    fn saldo_nao_lido_nao_e_banca_zerada() {
        let mut g = MoneyGuard::default();
        assert_eq!(g.check(0, 0, false), GuardEvent::None);
    }
}
