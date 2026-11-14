// SPDX-License-Identifier: Apache-2.0
//! Modo cautela: freio de risco que restringe as entradas a coluna e dúzia.
//! Liga com gale recorrente, escada no degrau 4 ou sequência longa de 1:1 na sessão.
//! Não prevê a mesa (giros são independentes): só reduz a exposição quando a banca apanha.

use std::collections::VecDeque;

/// Estratégias de coluna/dúzia que continuam entrando com o modo cautela ligado.
pub const CAUTION_ALLOWED: [&str; 4] = [
    "quebra_permanencia_coluna",
    "quebra_permanencia_duzia",
    "quebra_numero_repetido",
    "fibonacci_lag",
];
/// Ciclos recentes considerados no gatilho de gale recorrente.
pub const CYCLE_WINDOW: usize = 5;
/// Ciclos com gale (de `CYCLE_WINDOW`) que ligam o modo.
pub const GALE_CYCLES_TRIGGER: usize = 3;
/// Degrau da escada que liga o modo.
pub const DEEP_STEP_TRIGGER: u8 = 4;
/// Sequência de cor, alto/baixo ou par/ímpar na sessão que liga o modo.
pub const LONG_RUN_TRIGGER: usize = 8;
/// Acertos de primeira seguidos que desligam o modo.
pub const FIRST_HITS_TO_EXIT: u8 = 2;
/// Giros sem nenhum gatilho que desligam o modo.
pub const QUIET_SPINS_TO_EXIT: u32 = 20;

/// Família 1:1 (nome, lado do número).
type Family = (&'static str, fn(u8) -> bool);

const RED: [u8; 18] = [1, 3, 5, 7, 9, 12, 14, 16, 18, 19, 21, 23, 25, 27, 30, 32, 34, 36];

/// Maior sequência de cor, alto/baixo ou par/ímpar que termina no giro mais recente
/// (`recent[0]` = último). O zero quebra todas.
pub fn current_even_money_run(recent: &[u8]) -> (usize, &'static str) {
    let families: [Family; 3] = [
        ("cor", |n| RED.contains(&n)),
        ("alto/baixo", |n| n > 18),
        ("par/ímpar", |n| n % 2 == 0),
    ];
    let mut best = (0usize, "cor");
    for (name, f) in families {
        let Some(&first) = recent.first() else { break };
        if first == 0 {
            break;
        }
        let side = f(first);
        let run = recent.iter().take_while(|&&n| n != 0 && f(n) == side).count();
        if run > best.0 {
            best = (run, name);
        }
    }
    best
}

/// Estado do modo cautela da sessão.
#[derive(Debug, Default, Clone)]
pub struct Caution {
    active: bool,
    reason: String,
    cycles: VecDeque<bool>,
    first_hits: u8,
    quiet_spins: u32,
}

/// Mudança de estado para avisar painel e Telegram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CautionChange {
    None,
    On(String),
    Off(String),
}

impl Caution {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Com o modo ligado, só coluna e dúzia entram.
    pub fn allows(&self, strategy_id: &str) -> bool {
        !self.active || CAUTION_ALLOWED.contains(&strategy_id)
    }

    fn turn_on(&mut self, reason: String) -> CautionChange {
        self.quiet_spins = 0;
        self.first_hits = 0;
        if self.active {
            self.reason = reason;
            return CautionChange::None;
        }
        self.active = true;
        self.reason.clone_from(&reason);
        CautionChange::On(reason)
    }

    fn turn_off(&mut self, why: &str) -> CautionChange {
        self.active = false;
        self.reason.clear();
        self.first_hits = 0;
        self.quiet_spins = 0;
        self.cycles.clear();
        CautionChange::Off(why.to_string())
    }

    /// Ciclo de escada encerrado: `steps_used` = degrau em que ganhou (1 = de primeira),
    /// ou o último degrau no estouro (`won = false`).
    pub fn on_cycle_end(&mut self, steps_used: u8, won: bool) -> CautionChange {
        let needed_gale = steps_used > 1 || !won;
        self.cycles.push_back(needed_gale);
        while self.cycles.len() > CYCLE_WINDOW {
            self.cycles.pop_front();
        }
        if self.active {
            if won && steps_used == 1 {
                self.first_hits = self.first_hits.saturating_add(1);
                if self.first_hits >= FIRST_HITS_TO_EXIT {
                    return self.turn_off("2 acertos de primeira seguidos");
                }
            } else {
                self.first_hits = 0;
            }
        }
        let gales = self.cycles.iter().filter(|&&g| g).count();
        if gales >= GALE_CYCLES_TRIGGER {
            return self.turn_on(format!("{gales} dos últimos {} ciclos precisaram de gale", self.cycles.len()));
        }
        CautionChange::None
    }

    /// Escada avançou para `next_step` (próximo degrau a jogar).
    pub fn on_ladder_step(&mut self, next_step: u8) -> CautionChange {
        if next_step >= DEEP_STEP_TRIGGER {
            return self.turn_on(format!("escada chegou ao degrau {next_step}"));
        }
        CautionChange::None
    }

    /// Giro novo da sessão (`recent[0]` = último). Gatilho de sequência longa e saída por calma.
    pub fn on_spin(&mut self, recent: &[u8]) -> CautionChange {
        let (run, family) = current_even_money_run(recent);
        if run >= LONG_RUN_TRIGGER {
            return self.turn_on(format!("sequência de {run} em {family}"));
        }
        if self.active {
            self.quiet_spins = self.quiet_spins.saturating_add(1);
            if self.quiet_spins >= QUIET_SPINS_TO_EXIT {
                return self.turn_off("20 giros sem gatilho de cautela");
            }
        }
        CautionChange::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tres_ciclos_com_gale_em_cinco_ligam_e_so_coluna_duzia_entram() {
        let mut c = Caution::default();
        assert_eq!(c.on_cycle_end(2, true), CautionChange::None);
        assert_eq!(c.on_cycle_end(1, true), CautionChange::None);
        assert_eq!(c.on_cycle_end(3, true), CautionChange::None);
        assert!(matches!(c.on_cycle_end(2, true), CautionChange::On(_)));
        assert!(c.allows("quebra_permanencia_coluna"));
        assert!(c.allows("quebra_permanencia_duzia"));
        assert!(!c.allows("invictor_conservadora"));
        assert!(!c.allows("cluster_terminal"));
        assert!(!c.allows("quebra_cor_6x"));
    }

    #[test]
    fn dois_acertos_de_primeira_desligam() {
        let mut c = Caution::default();
        assert!(matches!(c.on_ladder_step(4), CautionChange::On(_)));
        assert_eq!(c.on_cycle_end(1, true), CautionChange::None);
        assert!(matches!(c.on_cycle_end(1, true), CautionChange::Off(_)));
        assert!(c.allows("invictor_conservadora"));
    }

    #[test]
    fn acerto_de_primeira_interrompido_por_gale_recomeca_a_contagem() {
        let mut c = Caution::default();
        let _ = c.on_ladder_step(4);
        let _ = c.on_cycle_end(1, true);
        let _ = c.on_cycle_end(2, true);
        assert_eq!(c.on_cycle_end(1, true), CautionChange::None);
        assert!(c.active());
    }

    #[test]
    fn degrau_3_nao_liga_degrau_4_liga() {
        let mut c = Caution::default();
        assert_eq!(c.on_ladder_step(3), CautionChange::None);
        assert!(matches!(c.on_ladder_step(4), CautionChange::On(r) if r.contains("degrau 4")));
    }

    #[test]
    fn sequencia_de_8_pretos_liga_e_20_giros_calmos_desligam() {
        let mut c = Caution::default();
        // 8 pretos seguidos alternando alto/baixo e par/ímpar (mais recente primeiro)
        let pretos = [2u8, 29, 4, 31, 6, 33, 8, 35, 1];
        assert!(matches!(c.on_spin(&pretos), CautionChange::On(r) if r.contains("cor")));
        let calmo = [1u8, 2, 0, 3];
        for _ in 0..19 {
            assert_eq!(c.on_spin(&calmo), CautionChange::None);
        }
        assert!(matches!(c.on_spin(&calmo), CautionChange::Off(_)));
    }

    #[test]
    fn sequencia_de_7_nao_liga_e_zero_quebra() {
        assert_eq!(current_even_money_run(&[2, 29, 4, 31, 6, 33, 8, 1]), (7, "cor"));
        assert_eq!(current_even_money_run(&[0, 2, 4]).0, 0);
        let mut c = Caution::default();
        assert_eq!(c.on_spin(&[2, 29, 4, 31, 6, 33, 8, 1]), CautionChange::None);
    }

    #[test]
    fn estouro_conta_como_ciclo_com_gale() {
        let mut c = Caution::default();
        let _ = c.on_cycle_end(5, false);
        let _ = c.on_cycle_end(5, false);
        assert!(matches!(c.on_cycle_end(5, false), CautionChange::On(_)));
    }
}
