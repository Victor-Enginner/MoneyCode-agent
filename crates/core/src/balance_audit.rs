// SPDX-License-Identifier: Apache-2.0
//! Conferência de extrato pós-resultado (§9): saldo real da mesa contra o delta do reconciliador.
//! Usa o "total na mão" = saldo + fichas no pano, que não muda ao apostar (só ao liquidar).
//! Variação de total sem entrada do bot em jogo é movimento externo (saque, depósito, ficha manual).

/// Tempo máximo para a mesa creditar o resultado antes de declarar divergência.
pub const AUDIT_DEADLINE_MS: i64 = 25_000;
/// Tolerância de arredondamento entre o esperado e o lido.
pub const AUDIT_TOLERANCE_CENTS: i64 = 1;

/// Conferência aberta de uma entrada já reconciliada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceAudit {
    pub expected_total_cents: i64,
    pub delta_cents: i64,
    pub strategy_id: String,
    pub number: u8,
    pub deadline_ms: i64,
}

/// O que a leitura de telemetria revelou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    None,
    /// Saldo bateu com o esperado.
    AuditOk { audit: BalanceAudit, observed_total_cents: i64 },
    /// Prazo esgotou e o saldo não bateu.
    Divergent { audit: BalanceAudit, observed_total_cents: i64 },
    /// Total mudou sem entrada do bot em jogo.
    External { delta_cents: i64 },
}

/// Estado compartilhado entre o loop de decisão e a thread de telemetria.
#[derive(Debug, Default)]
pub struct BalanceWatch {
    last_total: Option<i64>,
    entry_base: Option<i64>,
    audit: Option<BalanceAudit>,
}

impl BalanceWatch {
    /// Antes dos cliques: fixa o total de partida. Com conferência anterior ainda aberta,
    /// parte do esperado dela (o crédito anterior pode não ter caído ainda).
    pub fn begin_entry(&mut self) {
        self.entry_base = self
            .audit
            .as_ref()
            .map(|a| a.expected_total_cents)
            .or(self.last_total);
    }

    /// Entrada abortada (fichas limpas): nada a conferir.
    pub fn cancel_entry(&mut self) {
        self.entry_base = None;
    }

    /// Resultado reconciliado: abre a conferência `base + delta`. Sem base lida, não confere.
    /// Retorna `true` quando abriu a conferência.
    pub fn settle(&mut self, delta_cents: i64, strategy_id: &str, number: u8, now_ms: i64) -> bool {
        let Some(base) = self.entry_base.take() else {
            return false;
        };
        self.audit = Some(BalanceAudit {
            expected_total_cents: base + delta_cents,
            delta_cents,
            strategy_id: strategy_id.to_string(),
            number,
            deadline_ms: now_ms + AUDIT_DEADLINE_MS,
        });
        true
    }

    /// Leitura de telemetria: `total = saldo + fichas no pano`.
    pub fn observe(&mut self, total_cents: i64, now_ms: i64) -> WatchEvent {
        if let Some(audit) = self.audit.take() {
            if (total_cents - audit.expected_total_cents).abs() <= AUDIT_TOLERANCE_CENTS {
                self.last_total = Some(total_cents);
                return WatchEvent::AuditOk { audit, observed_total_cents: total_cents };
            }
            if now_ms >= audit.deadline_ms {
                // Entrada seguinte partiu do esperado: rebaseia na realidade para não
                // propagar a mesma divergência para a conferência dela.
                if let Some(base) = self.entry_base.as_mut() {
                    *base += total_cents - audit.expected_total_cents;
                }
                self.last_total = Some(total_cents);
                return WatchEvent::Divergent { audit, observed_total_cents: total_cents };
            }
            self.audit = Some(audit);
            return WatchEvent::None;
        }
        let prev = self.last_total.replace(total_cents);
        if self.entry_base.is_some() {
            return WatchEvent::None;
        }
        match prev {
            Some(p) if (total_cents - p).abs() > AUDIT_TOLERANCE_CENTS => {
                WatchEvent::External { delta_cents: total_cents - p }
            }
            _ => WatchEvent::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed(total: i64) -> BalanceWatch {
        let mut w = BalanceWatch::default();
        assert_eq!(w.observe(total, 0), WatchEvent::None);
        w
    }

    #[test]
    fn derrota_confere_na_hora_porque_a_mesa_ja_debitou() {
        let mut w = armed(4500);
        w.begin_entry();
        // fichas no pano: saldo cai, pano sobe, total igual
        assert_eq!(w.observe(4500, 1_000), WatchEvent::None);
        assert!(w.settle(-40, "quebra_permanencia_coluna", 7, 2_000));
        // saldo 4460, pano 0
        match w.observe(4460, 2_500) {
            WatchEvent::AuditOk { observed_total_cents, .. } => assert_eq!(observed_total_cents, 4460),
            e => panic!("esperava AuditOk, veio {e:?}"),
        }
    }

    #[test]
    fn vitoria_espera_o_credito_atrasado() {
        let mut w = armed(4500);
        w.begin_entry();
        assert!(w.settle(20, "quebra_permanencia_coluna", 3, 10_000));
        // crédito ainda não caiu: saldo 4460 + pano 40
        assert_eq!(w.observe(4500, 11_000), WatchEvent::None);
        // pano limpo antes do crédito
        assert_eq!(w.observe(4460, 12_000), WatchEvent::None);
        assert!(matches!(w.observe(4520, 13_000), WatchEvent::AuditOk { .. }));
    }

    #[test]
    fn ficha_manual_que_perdeu_vira_divergencia() {
        let mut w = armed(4500);
        w.begin_entry();
        assert!(w.settle(-40, "quebra_permanencia_coluna", 22, 0));
        // usuário tinha 0,20 a mais no pano e perdeu: total 4440 em vez de 4460
        assert_eq!(w.observe(4440, 1_000), WatchEvent::None);
        match w.observe(4440, AUDIT_DEADLINE_MS) {
            WatchEvent::Divergent { audit, observed_total_cents } => {
                assert_eq!(audit.expected_total_cents, 4460);
                assert_eq!(observed_total_cents, 4440);
            }
            e => panic!("esperava Divergent, veio {e:?}"),
        }
    }

    #[test]
    fn saque_sem_entrada_e_movimento_externo() {
        let mut w = armed(10_000);
        assert_eq!(w.observe(4_200, 500), WatchEvent::External { delta_cents: -5_800 });
        assert_eq!(w.observe(4_200, 1_000), WatchEvent::None);
    }

    #[test]
    fn total_parado_durante_a_entrada_nao_e_movimento_externo() {
        let mut w = armed(4500);
        w.begin_entry();
        // leitura de meio de clique com saldo e pano dessincronizados não dispara nada
        assert_eq!(w.observe(4480, 100), WatchEvent::None);
        w.cancel_entry();
        assert_eq!(w.observe(4500, 200), WatchEvent::External { delta_cents: 20 });
    }

    #[test]
    fn entrada_seguinte_parte_do_esperado_e_rebaseia_na_divergencia() {
        let mut w = armed(4500);
        w.begin_entry();
        assert!(w.settle(20, "a", 1, 0));
        // nova entrada antes do crédito cair: base = 4520 (esperado)
        w.begin_entry();
        // mesa nunca creditou: 4500 no prazo -> divergência de -20
        assert!(matches!(w.observe(4500, AUDIT_DEADLINE_MS), WatchEvent::Divergent { .. }));
        // a segunda entrada perde 40: esperado 4500-40 = 4460, não 4480
        assert!(w.settle(-40, "a", 5, 30_000));
        assert!(matches!(w.observe(4460, 30_500), WatchEvent::AuditOk { .. }));
    }

    #[test]
    fn sem_saldo_lido_nao_abre_conferencia() {
        let mut w = BalanceWatch::default();
        w.begin_entry();
        assert!(!w.settle(-40, "a", 1, 0));
    }
}
