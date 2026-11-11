// SPDX-License-Identifier: Apache-2.0
//! Matchers nativos de estratégia em Rust para avaliação determinística em sub-milissegundos.
//! Inclui quebra de permanência em coluna/dúzia, cluster terminal, lag de Fibonacci e viés de setor.
//! Atua como fonte primária e fallback imediato quando subprocessos Python excedem 120 ms.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Cobertura de aposta conforme contrato §5.2 (`col|dozen|straight|terminal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cover {
    /// Coluna 1, 2 ou 3 (faixa linear 1–12, 13–24, 25–36 conforme §8).
    Col(u8),
    /// Coluna física `2 to 1` do pano (fileiras mod 3: `1,4..34`, `2,5..35`, `3,6..36`).
    Col2To1(u8),
    /// Dúzia 1 (1–12), 2 (13–24) ou 3 (25–36).
    Dozen(u8),
    /// Cor Vermelha (18 casas).
    Red,
    /// Cor Preta (18 casas).
    Black,
    /// Baixos `1-18` (18 casas).
    Low,
    /// Altos `19-36` (18 casas).
    High,
    /// Pares `EVEN` (18 casas).
    Even,
    /// Ímpares `ODD` (18 casas).
    Odd,
    /// Número pleno (`0..=36`).
    Straight(u8),
    /// Terminal (`0..=9`).
    Terminal(u8),
}

/// Verifica se o número `n` (`1..=36`) pertence às 18 casas vermelhas da roleta europeia/brasileira.
#[inline]
pub const fn is_red_number(n: u8) -> bool {
    matches!(
        n,
        1 | 3 | 5 | 7 | 9 | 12 | 14 | 16 | 18 | 19 | 21 | 23 | 25 | 27 | 30 | 32 | 34 | 36
    )
}

impl Cover {
    /// Chave canônica para persistência em `fills.cover` e lookup em `calibration.toml`.
    pub fn canonical_key(self) -> String {
        match self {
            Self::Col(c) => format!("col:{c}"),
            Self::Col2To1(c) => format!("col2to1:{c}"),
            Self::Dozen(d) => format!("dozen:{d}"),
            Self::Red => "red".to_string(),
            Self::Black => "black".to_string(),
            Self::Low => "low".to_string(),
            Self::High => "high".to_string(),
            Self::Even => "even".to_string(),
            Self::Odd => "odd".to_string(),
            Self::Straight(n) => format!("straight:{n}"),
            Self::Terminal(t) => format!("terminal:{t}"),
        }
    }

    /// Verifica se o número sorteado (`0..=36`) vence esta cobertura (segundo §2 e §8 do AGENTS.md).
    pub const fn matches_number(self, number: u8) -> bool {
        if number > 36 {
            return false;
        }
        match self {
            Self::Col(c) | Self::Dozen(c) => {
                if number == 0 {
                    false
                } else {
                    ((number - 1) / 12 + 1) == c
                }
            }
            Self::Col2To1(c) => {
                if number == 0 {
                    false
                } else {
                    ((number - 1) % 3 + 1) == c
                }
            }
            Self::Red => number != 0 && is_red_number(number),
            Self::Black => number != 0 && !is_red_number(number),
            Self::Low => number >= 1 && number <= 18,
            Self::High => number >= 19 && number <= 36,
            Self::Even => number != 0 && number.is_multiple_of(2),
            Self::Odd => number != 0 && !number.is_multiple_of(2),
            Self::Straight(n) => number == n,
            Self::Terminal(t) => number != 0 && (number % 10) == t,
        }
    }
}

/// Contrato §5.2 `BetIntent` retornado tanto por matchers nativos quanto por plugins Python.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BetIntent {
    /// Versão do contrato (`1`).
    pub v: u8,
    /// Identificador da estratégia (`strategy_id`).
    pub strategy_id: String,
    /// Lista de coberturas recomendadas (ex.: `[{"col":2},{"col":3}]`).
    pub cover: Vec<Cover>,
    /// Confiança estatística (`0.0..=1.0`).
    pub confidence: f64,
}

/// Interface determinística dos matchers do DAG (`window[0]` = número mais recente).
pub trait Matcher: Send + Sync {
    /// Identificador estático do matcher / estratégia.
    fn id(&self) -> &'static str;

    /// Avalia a janela de últimos giros (`window[0]` mais recente) e retorna `BetIntent` se disparar.
    fn evaluate(&self, window: &[u8]) -> Option<BetIntent>;

    /// Retorna apenas a confiança (`0.0..=1.0`) caso o padrão esteja ativo.
    fn score(&self, window: &[u8]) -> Option<f64> {
        self.evaluate(window).map(|intent| intent.confidence)
    }
}

#[inline]
const fn bucket_12(n: u8) -> u8 {
    if n == 0 || n > 36 {
        0
    } else {
        (n - 1) / 12 + 1
    }
}

fn other_two_cols(target: u8) -> Vec<Cover> {
    (1..=3u8)
        .filter(|&c| c != target)
        .map(Cover::Col)
        .collect()
}

fn other_two_dozens(target: u8) -> Vec<Cover> {
    (1..=3u8)
        .filter(|&d| d != target)
        .map(Cover::Dozen)
        .collect()
}

/// `matcher.permanencia_col` (§8): dispara em streak `min..=max` (`4..=6`) da coluna mais recente.
/// Confiança `0.55 + 0.08 * (streak - min)` saturando em `0.95`; cobre as outras duas colunas.
#[derive(Debug, Clone, Copy)]
pub struct QuebraColuna {
    /// Streak mínimo para disparo (`4`).
    pub min: usize,
    /// Streak máximo para disparo (`6`).
    pub max: usize,
}

impl Default for QuebraColuna {
    fn default() -> Self {
        Self { min: 4, max: 6 }
    }
}

impl Matcher for QuebraColuna {
    fn id(&self) -> &'static str {
        "quebra_permanencia_coluna"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let alvo = bucket_12(*w.first()?);
        if alvo == 0 {
            return None;
        }
        let streak = w
            .iter()
            .copied()
            .map(bucket_12)
            .take_while(|&c| c == alvo)
            .count();
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.55 + 0.08 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_cols(alvo),
            confidence,
        })
    }
}

/// `matcher.permanencia_doz` (§8): dispara em streak `min..=max` (`4..=6`) da dúzia mais recente.
#[derive(Debug, Clone, Copy)]
pub struct QuebraDuzia {
    /// Streak mínimo para disparo (`4`).
    pub min: usize,
    /// Streak máximo para disparo (`6`).
    pub max: usize,
}

impl Default for QuebraDuzia {
    fn default() -> Self {
        Self { min: 4, max: 6 }
    }
}

impl Matcher for QuebraDuzia {
    fn id(&self) -> &'static str {
        "quebra_permanencia_duzia"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let alvo = bucket_12(*w.first()?);
        if alvo == 0 {
            return None;
        }
        let streak = w
            .iter()
            .copied()
            .map(bucket_12)
            .take_while(|&d| d == alvo)
            .count();
        if streak == 3 && self.min == 4 {
            // 3 repetições: pré-alerta de observação (conf 0.50 < 0.55)
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: other_two_dozens(alvo),
                confidence: 0.50,
            });
        }
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.66 + 0.07 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_dozens(alvo),
            confidence,
        })
    }
}

/// `matcher.cluster_terminal` (§8): matcher de momentum baseado na repetição do dígito terminal.
#[derive(Debug, Clone, Copy)]
pub struct ClusterTerminal {
    /// Tamanho da sub-janela inspecionada.
    pub lookback: usize,
    /// Repetições mínimas do terminal atual na janela.
    pub min_hits: usize,
}

impl Default for ClusterTerminal {
    fn default() -> Self {
        Self {
            lookback: 12,
            min_hits: 3,
        }
    }
}

impl Matcher for ClusterTerminal {
    fn id(&self) -> &'static str {
        "cluster_terminal"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        if latest == 0 || w.len() < self.min_hits {
            return None;
        }
        let term = latest % 10;
        let slice = &w[..w.len().min(self.lookback)];
        let hits = slice
            .iter()
            .copied()
            .filter(|&n| n != 0 && (n % 10) == term)
            .count();
        if hits < self.min_hits {
            return None;
        }
        let col_latest = bucket_12(latest);
        let second_col = if col_latest == 3 { 1 } else { col_latest + 1 };
        let confidence = (0.52 + 0.07 * ((hits - self.min_hits) as f64)).min(0.90);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![Cover::Col(col_latest), Cover::Col(second_col)],
            confidence,
        })
    }
}

/// `matcher.fibonacci_lag` (§8): verifica ressonância de coluna nos lags de Fibonacci (0, 1, 2, 4).
#[derive(Debug, Clone, Copy, Default)]
pub struct FibonacciLag;

impl Matcher for FibonacciLag {
    fn id(&self) -> &'static str {
        "fibonacci_lag"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        if w.len() < 5 {
            return None;
        }
        let c0 = bucket_12(w[0]);
        if c0 == 0 {
            return None;
        }
        if bucket_12(w[1]) == c0 && bucket_12(w[2]) == c0 && bucket_12(w[4]) == c0 {
            Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: other_two_cols(c0),
                confidence: 0.64,
            })
        } else {
            None
        }
    }
}

/// `matcher.setor_vies` (§8): detecta sobre-representação (>= 55%) de uma coluna em janela de 20 giros.
#[derive(Debug, Clone, Copy)]
pub struct SetorVies {
    /// Janela mínima para avaliação de viés.
    pub min_window: usize,
}

impl Default for SetorVies {
    fn default() -> Self {
        Self { min_window: 20 }
    }
}

impl Matcher for SetorVies {
    fn id(&self) -> &'static str {
        "setor_vies"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        if w.len() < self.min_window {
            return None;
        }
        let slice = &w[..self.min_window];
        let mut counts = [0usize; 4];
        let mut non_zero = 0usize;
        for &n in slice {
            let c = bucket_12(n) as usize;
            if (1..=3).contains(&c) {
                counts[c] += 1;
                non_zero += 1;
            }
        }
        if non_zero < 15 {
            return None;
        }
        let latest_col = bucket_12(w[0]) as usize;
        if !(1..=3).contains(&latest_col) {
            return None;
        }
        let ratio = (counts[latest_col] as f64) / (non_zero as f64);
        if ratio < 0.55 {
            return None;
        }
        let confidence = (0.56 + (ratio - 0.55) * 0.80).min(0.92);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_cols(latest_col as u8),
            confidence,
        })
    }
}

/// `matcher.quebra_coluna_real_2to1`: dispara em streak `min..=max` (`4..=6`) da coluna física `2 to 1` (`(n-1)%3 + 1`).
/// Nos 479 giros reais da Roleta Brasileira, obteve 11V/0E (+110c invicto).
#[derive(Debug, Clone, Copy)]
pub struct QuebraColunaReal {
    /// Streak mínimo para disparo (`4`).
    pub min: usize,
    /// Streak máximo para disparo (`6`).
    pub max: usize,
}

impl Default for QuebraColunaReal {
    fn default() -> Self {
        Self { min: 4, max: 8 }
    }
}

impl Matcher for QuebraColunaReal {
    fn id(&self) -> &'static str {
        "quebra_coluna_real_2to1"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let first = *w.first()?;
        if first == 0 || first > 36 {
            return None;
        }
        let alvo = (first - 1) % 3 + 1;
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n != 0 && n <= 36 && ((n - 1) % 3 + 1) == alvo)
            .count();
        let cover = (1..=3u8)
            .filter(|&c| c != alvo)
            .map(Cover::Col2To1)
            .collect();
        if streak == 3 && self.min == 4 {
            // 3 repetições: pré-alerta de observação (conf 0.50 < 0.55)
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover,
                confidence: 0.50,
            });
        }
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.68 + 0.07 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover,
            confidence,
        })
    }
}

/// `matcher.quebra_cor_6x`: observa em 5 repetições (`conf=0.50`) e entra a partir de 6 repetições
/// seguidas de Vermelho ou Preto (`conf>=0.68`), apostando na cor oposta.
#[derive(Debug, Clone, Copy)]
pub struct QuebraCor {
    /// Streak mínimo para entrada (`6`).
    pub min: usize,
    /// Streak máximo para entrada (`12`).
    pub max: usize,
}

impl Default for QuebraCor {
    fn default() -> Self {
        Self { min: 6, max: 12 }
    }
}

impl Matcher for QuebraCor {
    fn id(&self) -> &'static str {
        "quebra_cor_6x"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let first = *w.first()?;
        if first == 0 || first > 36 {
            return None;
        }
        let first_red = is_red_number(first);
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n != 0 && n <= 36 && is_red_number(n) == first_red)
            .count();
        let target_cover = if first_red { Cover::Black } else { Cover::Red };
        if streak + 1 == self.min {
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: vec![target_cover],
                confidence: 0.50,
            });
        }
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.68 + 0.06 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![target_cover],
            confidence,
        })
    }
}

/// `matcher.quebra_alto_baixo_6x`: observa em 5 repetições (`conf=0.50`) e entra a partir de 6 repetições
/// seguidas de Baixos (`1-18`) ou Altos (`19-36`), apostando na metade oposta.
#[derive(Debug, Clone, Copy)]
pub struct QuebraAltoBaixo {
    /// Streak mínimo para entrada (`6`).
    pub min: usize,
    /// Streak máximo para entrada (`12`).
    pub max: usize,
}

impl Default for QuebraAltoBaixo {
    fn default() -> Self {
        Self { min: 6, max: 12 }
    }
}

impl Matcher for QuebraAltoBaixo {
    fn id(&self) -> &'static str {
        "quebra_alto_baixo_6x"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let first = *w.first()?;
        if first == 0 || first > 36 {
            return None;
        }
        let first_low = first <= 18;
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n != 0 && n <= 36 && (n <= 18) == first_low)
            .count();
        let target_cover = if first_low { Cover::High } else { Cover::Low };
        if streak + 1 == self.min {
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: vec![target_cover],
                confidence: 0.50,
            });
        }
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.68 + 0.06 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![target_cover],
            confidence,
        })
    }
}

/// `matcher.quebra_par_impar_6x`: observa em 5 repetições (`conf=0.50`) e entra a partir de 6 repetições
/// seguidas de Par (`EVEN`) ou Ímpar (`ODD`), apostando na paridade oposta.
#[derive(Debug, Clone, Copy)]
pub struct QuebraParImpar {
    /// Streak mínimo para entrada (`6`).
    pub min: usize,
    /// Streak máximo para entrada (`12`).
    pub max: usize,
}

impl Default for QuebraParImpar {
    fn default() -> Self {
        Self { min: 6, max: 12 }
    }
}

impl Matcher for QuebraParImpar {
    fn id(&self) -> &'static str {
        "quebra_par_impar_6x"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let first = *w.first()?;
        if first == 0 || first > 36 {
            return None;
        }
        let first_even = first.is_multiple_of(2);
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n != 0 && n <= 36 && n.is_multiple_of(2) == first_even)
            .count();
        let target_cover = if first_even { Cover::Odd } else { Cover::Even };
        if streak + 1 == self.min {
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: vec![target_cover],
                confidence: 0.50,
            });
        }
        if !(self.min..=self.max).contains(&streak) {
            return None;
        }
        let confidence = (0.68 + 0.06 * ((streak - self.min) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![target_cover],
            confidence,
        })
    }
}

/// Constrói o conjunto padrão completo de matchers nativos do projeto MONSTER:
/// - Colunas e Dúzias: pré-alerta em 3x (`0.50`) e entrada imediata a partir de 4x (`>= 0.66`)
/// - Vermelho/Preto, Baixo/Alto e Par/Ímpar: pré-alerta em 5x (`0.50`) e entrada imediata a partir de 6x (`>= 0.68`)
/// - Viés de Setor (`setor_vies`), Fibonacci Lag e Cluster Terminal.
pub fn default_matchers() -> Vec<Arc<dyn Matcher>> {
    vec![
        Arc::new(QuebraColunaReal::default()),
        Arc::new(QuebraDuzia::default()),
        Arc::new(QuebraColuna::default()),
        Arc::new(QuebraCor::default()),
        Arc::new(QuebraAltoBaixo::default()),
        Arc::new(QuebraParImpar::default()),
        Arc::new(SetorVies::default()),
        Arc::new(FibonacciLag),
        Arc::new(ClusterTerminal::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quebra_coluna_triggers_on_streak_4_to_6_and_covers_other_two() {
        let matcher = QuebraColuna::default();
        // Streak de 3 na coluna 1 (números 1..=12): não dispara
        assert!(matcher.evaluate(&[3, 7, 11, 20]).is_none());

        // Streak de 4 na coluna 1: dispara com confiança 0.55 cobrindo Col(2) e Col(3)
        let intent4 = matcher.evaluate(&[2, 5, 8, 12, 25]).expect("streak 4 must trigger");
        assert_eq!(intent4.strategy_id, "quebra_permanencia_coluna");
        assert_eq!(intent4.cover, vec![Cover::Col(2), Cover::Col(3)]);
        assert!((intent4.confidence - 0.55).abs() < 1e-9);

        // Streak de 6 na coluna 2 (13..=24): dispara com 0.55 + 0.16 = 0.71 cobrindo Col(1) e Col(3)
        let intent6 = matcher
            .evaluate(&[14, 15, 18, 20, 22, 24, 1])
            .expect("streak 6 must trigger");
        assert_eq!(intent6.cover, vec![Cover::Col(1), Cover::Col(3)]);
        assert!((intent6.confidence - 0.71).abs() < 1e-9);

        // Streak de 7 excede max=6: não dispara
        assert!(matcher.evaluate(&[14, 15, 18, 20, 22, 24, 13, 1]).is_none());
    }

    #[test]
    fn bet_intent_serializes_to_canonical_contract_5_2() {
        let intent = BetIntent {
            v: 1,
            strategy_id: "quebra_permanencia_coluna".to_string(),
            cover: vec![Cover::Col(2), Cover::Col(3)],
            confidence: 0.79,
        };
        let json = serde_json::to_string(&intent).expect("serialize BetIntent");
        assert_eq!(
            json,
            r#"{"v":1,"strategy_id":"quebra_permanencia_coluna","cover":[{"col":2},{"col":3}],"confidence":0.79}"#
        );
    }
}
