// SPDX-License-Identifier: Apache-2.0
//! Vigia de dinheiro: banca zerada = alerta geral e executor parado até o depósito.
//! Sem stop win: lucro nunca pausa o bot (decisão do operador em 14/11/2026).
//! Nunca move dinheiro: só corta o executor (kill switch) e avisa painel e Telegram.

/// Abaixo disto a banca não paga nem a entrada mais barata: alerta de banca zerada.
pub const BANK_EMPTY_CENTS: i64 = 100;
/// Depósito que rearma o bot depois de banca zerada.
pub const BANK_REARM_CENTS: i64 = 1_000;

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
}

#[derive(Debug, Default)]
pub struct MoneyGuard {
    halted: Option<Halt>,
}

impl MoneyGuard {
    pub fn halted(&self) -> Option<Halt> {
        self.halted
    }

    /// `bank` = saldo real em centavos (0 = ainda não lido).
    pub fn check(&mut self, bank: i64) -> GuardEvent {
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
        GuardEvent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucro_nunca_para_o_bot() {
        let mut g = MoneyGuard::default();
        for bank in [4_000, 9_000, 20_000, 100_000] {
            assert_eq!(g.check(bank), GuardEvent::None);
        }
    }

    #[test]
    fn banca_zerada_para_e_so_rearma_com_deposito() {
        let mut g = MoneyGuard::default();
        assert!(matches!(g.check(17), GuardEvent::Halt { kind: Halt::BankEmpty, .. }));
        assert_eq!(g.check(17), GuardEvent::None);
        assert_eq!(g.check(500), GuardEvent::None, "R$ 5 não rearma");
        assert!(matches!(g.check(4_017), GuardEvent::Rearm { .. }));
        assert_eq!(g.check(4_017), GuardEvent::None);
    }

    #[test]
    fn saldo_nao_lido_nao_e_banca_zerada() {
        let mut g = MoneyGuard::default();
        assert_eq!(g.check(0), GuardEvent::None);
    }
}
