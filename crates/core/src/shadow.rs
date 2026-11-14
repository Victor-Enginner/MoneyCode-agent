// SPDX-License-Identifier: Apache-2.0
//! Estratégias em observação (sombra): detectam o gatilho e simulam a escada de número,
//! SEM apostar. Medem ao vivo os scripts do operador antes de qualquer dinheiro real.
//! Fonte dos números: corpus/cards/script_*.yaml (teste confere que batem).

/// Lucro alvo por ciclo na escada de número (centavos).
pub const SHADOW_TARGET_CENTS: u64 = 10;
/// Ficha mínima por número (centavos).
pub const SHADOW_CHIP_CENTS: u64 = 10;
/// Degraus máximos da escada simulada.
pub const SHADOW_MAX_STEPS: u8 = 5;

/// Gatilho do cartão.
#[derive(Debug, Clone, Copy)]
pub enum ShadowTrigger {
    /// Último giro é um destes números.
    LastSpinIn(&'static [u8]),
    /// Os `n` últimos giros terminam num destes finais.
    LastFinalsIn { n: usize, finals: &'static [u8] },
}

/// Cartão em observação.
#[derive(Debug, Clone, Copy)]
pub struct ShadowCard {
    pub id: &'static str,
    pub nome: &'static str,
    pub trigger: ShadowTrigger,
    /// Entrada + proteções (cada número com a mesma ficha).
    pub pockets: &'static [u8],
}

/// Os três scripts que o operador pediu para observar (14/11/2026).
pub const SHADOW_CARDS: [ShadowCard; 3] = [
    ShadowCard {
        id: "script_espelhos_gemeos",
        nome: "Espelhos & Gêmeos",
        trigger: ShadowTrigger::LastSpinIn(&[11, 12, 13, 20, 21, 22, 23, 31, 32, 33]),
        pockets: &[0, 2, 3, 4, 8, 9, 10, 11, 12, 13, 20, 21, 22, 23, 26, 27, 30, 31, 32, 33, 35, 36],
    },
    ShadowCard {
        id: "script_familia_369",
        nome: "Família 3-6-9",
        trigger: ShadowTrigger::LastFinalsIn { n: 1, finals: &[3, 6, 9] },
        pockets: &[0, 3, 6, 8, 9, 13, 15, 16, 18, 19, 22, 23, 26, 27, 29, 31, 32, 33, 35, 36],
    },
    ShadowCard {
        id: "script_familia_057",
        nome: "Família 0-5-7",
        trigger: ShadowTrigger::LastFinalsIn { n: 1, finals: &[0, 5, 7] },
        pockets: &[0, 5, 7, 8, 10, 11, 12, 13, 15, 17, 20, 23, 25, 26, 27, 28, 30, 33, 35, 36],
    },
];

impl ShadowCard {
    /// `recent[0]` = giro mais recente.
    pub fn triggered(&self, recent: &[u8]) -> bool {
        match self.trigger {
            ShadowTrigger::LastSpinIn(nums) => recent.first().is_some_and(|n| nums.contains(n)),
            ShadowTrigger::LastFinalsIn { n, finals } => {
                recent.len() >= n && recent[..n].iter().all(|x| finals.contains(&(x % 10)))
            }
        }
    }

    /// Ficha por número no degrau, recuperando `spent` + lucro alvo (arredonda para cima).
    pub fn chip_for(&self, spent: u64) -> u64 {
        let k = self.pockets.len() as u64;
        let net = 36u64.saturating_sub(k).max(1);
        let x = (spent + SHADOW_TARGET_CENTS).div_ceil(net);
        x.div_ceil(SHADOW_CHIP_CENTS) * SHADOW_CHIP_CENTS
    }
}

/// Fim de um ciclo simulado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowSettle {
    pub id: &'static str,
    pub nome: &'static str,
    pub won: bool,
    /// Degrau em que acabou (ganhou nele ou estourou nele).
    pub step: u8,
    pub net_cents: i64,
    pub number: u8,
    /// `true` quando o degrau seguinte passaria da trava de exposição da banca.
    pub blocked_by_exposure: bool,
}

#[derive(Debug, Clone, Default)]
struct Cycle {
    step: u8,
    spent: u64,
    chip: u64,
}

/// Placar de um cartão.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowStats {
    pub cycles: u32,
    pub wins: u32,
    pub blows: u32,
    pub net_cents: i64,
    pub worst_cents: i64,
    pub by_step: [u32; SHADOW_MAX_STEPS as usize],
}

impl ShadowStats {
    pub fn add(&mut self, s: &ShadowSettle) {
        self.cycles += 1;
        if s.won {
            self.wins += 1;
            if let Some(slot) = self.by_step.get_mut(usize::from(s.step.saturating_sub(1))) {
                *slot += 1;
            }
        } else {
            self.blows += 1;
        }
        self.net_cents += s.net_cents;
        self.worst_cents = self.worst_cents.min(s.net_cents);
    }
}

/// Estado de um cartão em observação.
#[derive(Debug, Clone)]
pub struct ShadowRunner {
    pub card: ShadowCard,
    cycle: Option<Cycle>,
    pub stats: ShadowStats,
}

impl ShadowRunner {
    pub fn new(card: ShadowCard) -> Self {
        Self { card, cycle: None, stats: ShadowStats::default() }
    }

    /// Degrau em jogo (`0` = aguardando gatilho) e ficha por número dele.
    pub fn open(&self) -> (u8, u64) {
        self.cycle.as_ref().map_or((0, 0), |c| (c.step, c.chip))
    }

    /// Giro novo (`recent[0]` = este giro). `exposure_cap_cents` = trava de exposição da banca.
    /// Resolve o ciclo aberto e, se livre, abre outro quando o gatilho bate.
    pub fn on_spin(&mut self, recent: &[u8], exposure_cap_cents: u64) -> Option<ShadowSettle> {
        let number = *recent.first()?;
        let k = self.card.pockets.len() as u64;
        let mut settled = None;
        if let Some(mut c) = self.cycle.take() {
            if self.card.pockets.contains(&number) {
                settled = Some(self.settle(true, c.step, 36 * c.chip as i64 - c.spent as i64, number, false));
            } else if c.step >= SHADOW_MAX_STEPS {
                settled = Some(self.settle(false, c.step, -(c.spent as i64), number, false));
            } else {
                let chip = self.card.chip_for(c.spent);
                if chip * k > exposure_cap_cents {
                    settled = Some(self.settle(false, c.step, -(c.spent as i64), number, true));
                } else {
                    c.step += 1;
                    c.chip = chip;
                    c.spent += chip * k;
                    self.cycle = Some(c);
                    return None;
                }
            }
        }
        if self.cycle.is_none() && self.card.triggered(recent) {
            let chip = self.card.chip_for(0);
            if chip * k <= exposure_cap_cents {
                self.cycle = Some(Cycle { step: 1, spent: chip * k, chip });
            }
        }
        settled
    }

    fn settle(&mut self, won: bool, step: u8, net: i64, number: u8, blocked: bool) -> ShadowSettle {
        let s = ShadowSettle {
            id: self.card.id,
            nome: self.card.nome,
            won,
            step,
            net_cents: net,
            number,
            blocked_by_exposure: blocked,
        };
        self.stats.add(&s);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEM_TRAVA: u64 = u64::MAX;

    fn runner(id: &str) -> ShadowRunner {
        ShadowRunner::new(*SHADOW_CARDS.iter().find(|c| c.id == id).expect("cartão"))
    }

    #[test]
    fn numeros_batem_com_os_cartoes_yaml() {
        for card in SHADOW_CARDS {
            let path = format!("{}/../../corpus/cards/{}.yaml", env!("CARGO_MANIFEST_DIR"), card.id);
            let text = std::fs::read_to_string(&path).expect("cartão yaml");
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with("pockets:"))
                .expect("pockets");
            let inside = line.split('[').nth(1).and_then(|r| r.split(']').next()).expect("lista");
            let nums: Vec<u8> = inside.split(',').map(|n| n.trim().parse().expect("número")).collect();
            assert_eq!(nums, card.pockets.to_vec(), "{} diverge do YAML", card.id);
        }
    }

    #[test]
    fn escada_de_20_numeros_segue_a_tabela() {
        let r = runner("script_familia_057");
        let mut spent = 0;
        let mut chips = vec![];
        for _ in 0..5 {
            let x = r.card.chip_for(spent);
            chips.push(x);
            spent += x * 20;
        }
        assert_eq!(chips, vec![10, 20, 40, 90, 210]);
        assert_eq!(spent, 7400);
    }

    #[test]
    fn espelho_dispara_e_acerto_no_degrau_2_paga_o_ciclo() {
        let mut r = runner("script_espelhos_gemeos");
        assert_eq!(r.on_spin(&[21, 5], SEM_TRAVA), None);
        assert_eq!(r.open(), (1, 10));
        // 1 não está na cobertura: degrau 2
        assert_eq!(r.on_spin(&[1, 21], SEM_TRAVA), None);
        assert_eq!(r.open(), (2, 20));
        let s = r.on_spin(&[32, 1, 21], SEM_TRAVA).expect("liquidou");
        assert!(s.won);
        assert_eq!(s.step, 2);
        // 22 números (com 0 e 20): degrau 1 = 22 x 10c, degrau 2 = 22 x 20c
        assert_eq!(s.net_cents, 36 * 20 - (22 * 10 + 22 * 20));
        assert_eq!(r.stats.wins, 1);
    }

    #[test]
    fn familia_dispara_com_um_final_da_familia() {
        let r = runner("script_familia_369");
        assert!(r.card.triggered(&[13, 4]));
        assert!(!r.card.triggered(&[14, 13]));
        let z = runner("script_familia_057");
        assert!(z.card.triggered(&[0]));
        assert!(!z.card.triggered(&[18]));
    }

    #[test]
    fn cinco_erros_estouram_o_ciclo() {
        let mut r = runner("script_familia_369");
        let _ = r.on_spin(&[3], SEM_TRAVA);
        // números fora da cobertura da 3-6-9
        for n in [1u8, 2, 4, 5] {
            assert_eq!(r.on_spin(&[n], SEM_TRAVA), None);
        }
        let s = r.on_spin(&[7], SEM_TRAVA).expect("estouro");
        assert!(!s.won);
        assert_eq!(s.step, 5);
        // 20 números (com o 0): 200 + 400 + 800 + 1800 + 4200
        assert_eq!(s.net_cents, -7400);
    }

    #[test]
    fn trava_de_exposicao_encerra_o_ciclo_antes_do_degrau_caro() {
        let mut r = runner("script_familia_057");
        let _ = r.on_spin(&[30], 3_850);
        for n in [1u8, 2, 3] {
            let _ = r.on_spin(&[n], 3_850);
        }
        // degrau 4 custaria 1800, ok; degrau 5 custaria 4200 > 3850: para no 4
        let s = r.on_spin(&[4], 3_850).expect("trava");
        assert!(s.blocked_by_exposure);
        assert_eq!(s.step, 4);
        assert_eq!(s.net_cents, -3200);
    }
}
