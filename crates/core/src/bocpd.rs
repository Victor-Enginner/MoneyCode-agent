// SPDX-License-Identifier: Apache-2.0
//! Detecção bayesiana online de mudança (BOCPD) sobre as famílias de finais.
//!
//! A regra de streak olha só o fim da fila: "4 finais da mesma família" tanto faz se o bloco
//! começou há 6 giros ou há 60. Este módulo responde a pergunta que o streak não faz —
//! *há quanto tempo o segmento atual começou e qual a chance de a mesa ter trocado de família
//! agora* — mantendo a posterior sobre o comprimento do segmento (Adams & MacKay, 2007,
//! `arXiv:0710.3742`).
//!
//! Implementação em replay puro: cada giro reprocessa a história recente do zero, então reinício
//! do processo ou reordenação de spins não deixam estado divergente. Custo `O(max_run)` por giro.
//!
//! O final `0` (e o número `0`) é curinga: não pertence a família, não entra na contagem e não
//! reinicia segmento — do mesmo jeito que no matcher.

use serde::{Deserialize, Serialize};

/// Parâmetros do BOCPD e limiares do veto de regime.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BocpdCfg {
    /// Hazard `1/H`: probabilidade a priori de troca de família a cada giro (`H` ≈ duração do bloco).
    pub hazard: f64,
    /// Maior comprimento de segmento rastreado (truncamento da posterior).
    pub max_run: usize,
    /// Concentração do prior Dirichlet por família (`alpha = 1` = uniforme).
    pub alpha: f64,
    /// Observações mínimas para o veto valer (história curta não bloqueia nada).
    pub min_observations: usize,
    /// P(troca no último giro) a partir da qual o regime é considerado trocado.
    ///
    /// Medido no replay: mesa estável fica em ~0,02 (o próprio hazard) e uma troca real de
    /// família com 40 giros de segmento dá ~0,37. `0,20` é folga acima do ruído e continua
    /// pegando o pico — que dura um giro só, porque no seguinte a posterior já migra.
    pub max_p_change: f64,
    /// Fração mínima da família-alvo na preditiva do segmento atual.
    pub min_family_share: f64,
}

impl Default for BocpdCfg {
    fn default() -> Self {
        Self {
            // Bloco típico de ~18 giros: mais curto que a média de permanência relatada na mesa.
            hazard: 1.0 / 18.0,
            max_run: 120,
            alpha: 1.0,
            min_observations: 20,
            max_p_change: 0.20,
            min_family_share: 0.40,
        }
    }
}

/// Motivo do veto de regime sobre uma entrada de cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VetoReason {
    /// A posterior indica que a família trocou no último giro (ou muito recentemente).
    JustChanged,
    /// O segmento atual não sustenta mais a família que o matcher quer cobrir.
    FamilyFaded,
}

/// Resultado do BOCPD para a história analisada.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BocpdReport {
    /// Giros com família definida usados na posterior (curingas não contam).
    pub observations: usize,
    /// `P(o último ponto de mudança é o giro mais recente | história)`.
    pub p_change: f64,
    /// Comprimento do segmento mais provável (idade do regime atual em giros).
    pub map_run_length: usize,
    /// Preditiva `P(próximo final | família f)` sob o segmento mais provável.
    pub family_share: [f64; 3],
    /// Família mais provável no segmento atual (`final % 3`: `0` = 3,6,9 · `1` = 1,4,7 · `2` = 2,5,8).
    pub map_family: Option<usize>,
}

/// Família de um número pelo seu final (`final % 3`), ou `None` para o curinga.
///
/// `0`, `10`, `20`, `30` e o zero têm final `0` e não pertencem a família nenhuma.
pub fn family_of_number(number: u8) -> Option<usize> {
    if number > 36 {
        return None;
    }
    let final_ = number % 10;
    if number == 0 || final_ == 0 {
        None
    } else {
        Some(usize::from(final_ % 3))
    }
}

/// Roda o BOCPD sobre `history_oldest_first` (mais antigo primeiro) e devolve a posterior final.
pub fn analyze_chronological(history_oldest_first: &[u8], cfg: &BocpdCfg) -> BocpdReport {
    let cap = cfg.max_run.max(1);
    let hazard = cfg.hazard.clamp(1e-6, 0.5);
    let alpha = cfg.alpha.max(1e-6);

    // Truncagem de memória: o que está além de `max_run + 40` não muda a preditiva relevante.
    let keep = cap.saturating_add(40);
    let history = if history_oldest_first.len() > keep {
        &history_oldest_first[history_oldest_first.len() - keep..]
    } else {
        history_oldest_first
    };

    let mut run = vec![0.0f64; cap + 1];
    let mut counts = vec![[0.0f64; 3]; cap + 1];
    run[0] = 1.0;
    let mut observations = 0usize;
    let mut new_run = vec![0.0f64; cap + 1];
    let mut new_counts = vec![[0.0f64; 3]; cap + 1];

    for &number in history {
        let Some(family) = family_of_number(number) else {
            continue;
        };
        observations += 1;

        new_run.iter_mut().for_each(|v| *v = 0.0);
        let mut change_mass = 0.0f64;
        for r in 0..=cap {
            let weight = run[r];
            if weight <= 0.0 {
                continue;
            }
            let total = counts[r][0] + counts[r][1] + counts[r][2];
            let predictive = (counts[r][family] + alpha) / (total + 3.0 * alpha);
            if r < cap {
                new_run[r + 1] += weight * predictive * (1.0 - hazard);
            }
            change_mass += weight * hazard / 3.0;
        }
        new_run[0] = change_mass;

        let sum: f64 = new_run.iter().sum();
        if sum > 0.0 {
            new_run.iter_mut().for_each(|v| *v /= sum);
        }
        std::mem::swap(&mut run, &mut new_run);

        // O segmento de comprimento `r + 1` herda as contagens de `r` mais a observação atual.
        new_counts[0] = [0.0f64; 3];
        new_counts[1..=cap].copy_from_slice(&counts[..cap]);
        for segment in new_counts[1..=cap].iter_mut() {
            segment[family] += 1.0;
        }
        std::mem::swap(&mut counts, &mut new_counts);
    }

    let map_run_length = run
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
        .unwrap_or(0);

    let segment = &counts[map_run_length];
    let total = segment[0] + segment[1] + segment[2];
    let mut family_share = [1.0 / 3.0; 3];
    for f in 0..3 {
        family_share[f] = (segment[f] + alpha) / (total + 3.0 * alpha);
    }
    let map_family = if total > 0.0 {
        family_share
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(idx, _)| idx)
    } else {
        None
    };

    BocpdReport {
        observations,
        p_change: run[0],
        map_run_length,
        family_share,
        map_family,
    }
}

impl BocpdReport {
    /// Diz se o regime veta cobrir `family` agora, e por quê.
    ///
    /// O veto é conservador por construção: só vale com história suficiente (`min_observations`).
    /// `JustChanged` pega o giro da virada; `FamilyFaded` é o bloqueio durável — é ele que impede
    /// um streak de 4 numa mesa alternada (família ~1/3 da preditiva) de virar entrada.
    pub fn veto(&self, family: usize, cfg: &BocpdCfg) -> Option<VetoReason> {
        if self.observations < cfg.min_observations {
            return None;
        }
        if self.p_change >= cfg.max_p_change {
            return Some(VetoReason::JustChanged);
        }
        if family >= 3 {
            return None;
        }
        // O regime aponta uma família e a cobertura é de outra: o sinal está fora de fase.
        if matches!(self.map_family, Some(map) if map != family) {
            return Some(VetoReason::FamilyFaded);
        }
        if self.family_share[family] < cfg.min_family_share {
            return Some(VetoReason::FamilyFaded);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Família 1 (finais 1,4,7) → números que só têm esses finais.
    const FAM1: [u8; 6] = [1, 4, 7, 11, 14, 17];
    /// Família 0 (finais 3,6,9).
    const FAM0: [u8; 6] = [3, 6, 9, 13, 16, 19];

    fn build(prefix: &[u8], suffix: &[u8], n: usize) -> Vec<u8> {
        prefix
            .iter()
            .copied()
            .cycle()
            .take(n)
            .chain(suffix.iter().copied())
            .collect()
    }

    #[test]
    fn family_mapping_keeps_zero_as_wildcard() {
        assert_eq!(family_of_number(0), None);
        assert_eq!(family_of_number(10), None);
        assert_eq!(family_of_number(20), None);
        assert_eq!(family_of_number(30), None);
        assert_eq!(family_of_number(1), Some(1));
        assert_eq!(family_of_number(4), Some(1));
        assert_eq!(family_of_number(7), Some(1));
        assert_eq!(family_of_number(2), Some(2));
        assert_eq!(family_of_number(5), Some(2));
        assert_eq!(family_of_number(8), Some(2));
        assert_eq!(family_of_number(3), Some(0));
        assert_eq!(family_of_number(6), Some(0));
        assert_eq!(family_of_number(9), Some(0));
        assert_eq!(family_of_number(36), Some(0));
        assert_eq!(family_of_number(37), None);
    }

    #[test]
    fn steady_family_has_one_long_segment() {
        let cfg = BocpdCfg::default();
        let hist = build(&FAM1, &[], 60);
        let rep = analyze_chronological(&hist, &cfg);
        assert_eq!(rep.observations, 60);
        assert_eq!(rep.map_run_length, 60, "família constante = um segmento só");
        assert_eq!(rep.map_family, Some(1));
        assert!(
            rep.p_change < 0.1,
            "sem troca, p_change fica no chão: {}",
            rep.p_change
        );
        assert!(rep.family_share[1] > 0.8);
        assert_eq!(rep.veto(1, &cfg), None, "família estável não leva veto");
    }

    #[test]
    fn family_switch_spikes_change_probability_then_rebases() {
        let cfg = BocpdCfg::default();
        let before = analyze_chronological(&build(&FAM1, &[], 40), &cfg);
        let at_switch = analyze_chronological(&build(&FAM1, &FAM0[..1], 40), &cfg);
        assert!(
            at_switch.p_change > before.p_change * 3.0,
            "troca de família deve inflar p_change: {:.3} -> {:.3}",
            before.p_change,
            at_switch.p_change
        );
        assert!(at_switch.p_change >= cfg.max_p_change);
        assert_eq!(at_switch.veto(0, &cfg), Some(VetoReason::JustChanged));

        // Cinco giros depois a posterior já reancorou na família nova.
        let rebased = analyze_chronological(&build(&FAM1, &FAM0[..5], 40), &cfg);
        assert_eq!(rebased.map_family, Some(0));
        assert_eq!(rebased.map_run_length, 4, "segmento novo = 5 giros");
        assert_eq!(rebased.veto(0, &cfg), None);
        assert_eq!(rebased.veto(1, &cfg), Some(VetoReason::FamilyFaded));
    }

    #[test]
    fn three_family_rotation_has_no_family_to_cover() {
        // Rotação 1-4-7 → 3-6-9 → 2-5-8 (uma de cada): nenhuma passa de 1/3.
        let cfg = BocpdCfg::default();
        let mut hist = Vec::new();
        for i in 0..60u8 {
            hist.push(match i % 3 {
                0 => FAM1[0],
                1 => FAM0[0],
                _ => 5,
            });
        }
        let rep = analyze_chronological(&hist, &cfg);
        assert!(rep.family_share.iter().all(|s| *s < cfg.min_family_share));
        assert_eq!(rep.veto(1, &cfg), Some(VetoReason::FamilyFaded));
        assert_eq!(rep.veto(0, &cfg), Some(VetoReason::FamilyFaded));
    }

    #[test]
    fn two_family_alternation_is_left_to_the_dominance_rule() {
        // Alternando 1-4-7 e 3-6-9 o segmento é genuinamente misto (~48/48): o BOCPD não vê
        // troca de distribuição, e quem barra a entrada é a dominância ≥ 55% do próprio matcher.
        let cfg = BocpdCfg::default();
        let mut hist = Vec::new();
        for _ in 0..20 {
            hist.push(FAM1[0]);
            hist.push(FAM0[0]);
        }
        hist.extend_from_slice(&FAM1[..5]);
        let rep = analyze_chronological(&hist, &cfg);
        assert!(rep.family_share[1] > cfg.min_family_share);
        assert_eq!(rep.veto(1, &cfg), None);
    }

    #[test]
    fn short_history_never_vetoes() {
        let cfg = BocpdCfg::default();
        let rep = analyze_chronological(&[1, 4, 7, 3], &cfg);
        assert!(rep.observations < cfg.min_observations);
        assert_eq!(rep.veto(0, &cfg), None);
        assert_eq!(rep.veto(1, &cfg), None);
    }

    #[test]
    fn wildcard_zero_does_not_restart_or_count() {
        let cfg = BocpdCfg::default();
        let mut hist = build(&FAM1, &[], 30);
        hist.extend_from_slice(&[0, 10, 20, 30]);
        hist.extend_from_slice(&FAM1[..5]);
        let with_zero = analyze_chronological(&hist, &cfg);
        let without_zero = analyze_chronological(&build(&FAM1, &[], 35), &cfg);
        assert_eq!(with_zero.observations, without_zero.observations);
        assert!(
            with_zero.map_run_length >= 34,
            "curinga não quebra segmento"
        );
        assert!((with_zero.family_share[1] - without_zero.family_share[1]).abs() < 0.05);
    }

    #[test]
    fn cover_family_out_of_phase_with_regime_is_vetoed() {
        let cfg = BocpdCfg::default();
        let rep = BocpdReport {
            observations: 40,
            p_change: 0.02,
            map_run_length: 40,
            family_share: [0.90, 0.05, 0.05],
            map_family: Some(0),
        };
        // Regime em 3-6-9 não autoriza cobrir 1-4-7, mesmo com share alta da família 0.
        assert_eq!(rep.veto(1, &cfg), Some(VetoReason::FamilyFaded));
        assert_eq!(rep.veto(0, &cfg), None);
    }

    #[test]
    fn posterior_is_normalized_and_bounded() {
        let cfg = BocpdCfg::default();
        let rep = analyze_chronological(&build(&FAM1, &FAM0, 90), &cfg);
        assert!(rep.p_change >= 0.0 && rep.p_change <= 1.0);
        assert!((rep.family_share.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(rep.map_run_length <= cfg.max_run);
    }
}

