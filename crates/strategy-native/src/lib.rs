// SPDX-License-Identifier: Apache-2.0
//! Matchers nativos de estratégia em Rust para avaliação determinística em sub-milissegundos.
//! Separa de forma matematicamente estrita Coluna Real (`2 to 1`: `(n-1)%3+1`) de Dúzia (`1st/2nd/3rd 12`: `(n-1)/12+1`).
//! Inclui quebra de coluna, quebra de dúzia, cor/alto-baixo/par-ímpar (6x), viés de setor, Fibonacci lag e cluster terminal.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Cobertura de aposta conforme contrato §5.2 (`col|dozen|straight|terminal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cover {
    /// Coluna real `2 to 1` do pano (fileira 1: `1,4..34`, fileira 2: `2,5..35`, fileira 3: `3,6..36`).
    Col(u8),
    /// Alias de compatibilidade para coluna física `2 to 1` (`1..=3`).
    Col2To1(u8),
    /// Dúzia 1 (`1st 12`: `1..=12`), Dúzia 2 (`2nd 12`: `13..=24`) ou Dúzia 3 (`3rd 12`: `25..=36`).
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

/// Retorna a Coluna Real `2 to 1` (`1..=3`) de um número `1..=36`, ou `0` para `0`/inválido:
/// - Coluna 1: `1, 4, 7, 10, 13, 16, 19, 22, 25, 28, 31, 34`
/// - Coluna 2: `2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35`
/// - Coluna 3: `3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36`
#[inline]
pub const fn column_of(n: u8) -> u8 {
    if n == 0 || n > 36 {
        0
    } else {
        (n - 1) % 3 + 1
    }
}

/// Retorna a Dúzia Real (`1..=3`) de um número `1..=36`, ou `0` para `0`/inválido:
/// - Dúzia 1 (`1st 12`): `1..=12`
/// - Dúzia 2 (`2nd 12`): `13..=24`
/// - Dúzia 3 (`3rd 12`): `25..=36`
#[inline]
pub const fn dozen_of(n: u8) -> u8 {
    if n == 0 || n > 36 {
        0
    } else {
        (n - 1) / 12 + 1
    }
}

impl Cover {
    /// Chave canônica para persistência em `fills.cover` e atuação no sidecar CDP (`click.go`).
    pub fn canonical_key(self) -> String {
        match self {
            Self::Col(c) | Self::Col2To1(c) => format!("col:{c}"),
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

    /// Rótulo humano claro para exibição na FUI de Operação (`COLUNA 2`, `DÚZIA 2`, etc.).
    pub fn display_label(self) -> String {
        match self {
            Self::Col(c) | Self::Col2To1(c) => format!("COLUNA {c}"),
            Self::Dozen(d) => format!("DÚZIA {d}"),
            Self::Red => "VERMELHO".to_string(),
            Self::Black => "PRETO".to_string(),
            Self::Low => "BAIXOS (1-18)".to_string(),
            Self::High => "ALTOS (19-36)".to_string(),
            Self::Even => "PAR (EVEN)".to_string(),
            Self::Odd => "ÍMPAR (ODD)".to_string(),
            Self::Straight(n) => format!("PLENO {n}"),
            Self::Terminal(t) => format!("TERMINAL {t}"),
        }
    }

    /// Todos os números `0..=36` que esta cobertura cobre, em ordem crescente.
    ///
    /// É a base do cálculo de exposição em cobertura de número: `Terminal(0)` devolve
    /// `[0, 10, 20, 30]` (4 pockets), `Terminal(7)` devolve `[7, 17, 27]` (3 pockets).
    pub fn numbers(self) -> Vec<u8> {
        (0..=36u8).filter(|&n| self.matches_number(n)).collect()
    }

    /// Verifica se o número sorteado (`0..=36`) vence esta cobertura.
    pub const fn matches_number(self, number: u8) -> bool {
        if number > 36 {
            return false;
        }
        match self {
            Self::Col(c) | Self::Col2To1(c) => column_of(number) == c && c != 0,
            Self::Dozen(d) => dozen_of(number) == d && d != 0,
            Self::Red => number != 0 && is_red_number(number),
            Self::Black => number != 0 && !is_red_number(number),
            Self::Low => number >= 1 && number <= 18,
            Self::High => number >= 19 && number <= 36,
            Self::Even => number != 0 && number % 2 == 0,
            Self::Odd => number != 0 && number % 2 != 0,
            Self::Straight(n) => number == n,
            // Terminal 0 cobre 0, 10, 20 e 30: o `0` É o final 0, nunca pode ser excluído.
            Self::Terminal(t) => (number % 10) == t && t <= 9,
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

/// `matcher.permanencia_col`: dispara em streak `min..=max` (`4..=6`) da Coluna Real `2 to 1` (`(n-1)%3+1`).
/// Emite pré-alerta de observação em 3 repetições (`0.50`) e entra a partir de 4 repetições cobrindo as outras 2 colunas.
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
        let alvo = column_of(*w.first()?);
        if alvo == 0 {
            return None;
        }
        let streak = w
            .iter()
            .copied()
            .map(column_of)
            .take_while(|&c| c == alvo)
            .count();
        // Pré-alerta de observação: uma repetição antes do gatilho, sempre `0.50` (< `min_confidence`).
        if self.min >= 3 && streak + 1 == self.min {
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: other_two_cols(alvo),
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
            cover: other_two_cols(alvo),
            confidence,
        })
    }
}

/// Alias compatível para `QuebraColuna` com janela estendida (`4..=8`).
pub type QuebraColunaReal = QuebraColuna;

/// `matcher.permanencia_doz`: dispara em streak `min..=max` (`4..=6`) da Dúzia Real (`1st/2nd/3rd 12`: `(n-1)/12+1`).
/// Emite pré-alerta em 3 repetições (`0.50`) e entra a partir de 4 repetições cobrindo as outras 2 dúzias.
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
        let alvo = dozen_of(*w.first()?);
        if alvo == 0 {
            return None;
        }
        let streak = w
            .iter()
            .copied()
            .map(dozen_of)
            .take_while(|&d| d == alvo)
            .count();
        if self.min >= 3 && streak + 1 == self.min {
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

/// `matcher.cluster_terminal`: matcher de momentum baseado na repetição do dígito terminal (entrando a partir de 4 hits).
#[derive(Debug, Clone, Copy)]
pub struct ClusterTerminal {
    /// Tamanho da sub-janela inspecionada.
    pub lookback: usize,
    /// Repetições mínimas do terminal atual na janela para entrada (`4`).
    pub min_hits: usize,
}

impl Default for ClusterTerminal {
    fn default() -> Self {
        Self {
            lookback: 12,
            min_hits: 4,
        }
    }
}

impl Matcher for ClusterTerminal {
    fn id(&self) -> &'static str {
        "cluster_terminal"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        if latest == 0 || w.len() < 3 {
            return None;
        }
        let term = latest % 10;
        let slice = &w[..w.len().min(self.lookback)];
        let hits = slice
            .iter()
            .copied()
            .filter(|&n| n != 0 && (n % 10) == term)
            .count();
        let col_latest = column_of(latest);
        let second_col = if col_latest == 3 { 1 } else { col_latest + 1 };
        if hits == 3 && self.min_hits == 4 {
            return Some(BetIntent {
                v: 1,
                strategy_id: self.id().to_string(),
                cover: vec![Cover::Col(col_latest), Cover::Col(second_col)],
                confidence: 0.50,
            });
        }
        if hits < self.min_hits {
            return None;
        }
        let confidence = (0.64 + 0.07 * ((hits - self.min_hits) as f64)).min(0.90);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![Cover::Col(col_latest), Cover::Col(second_col)],
            confidence,
        })
    }
}

/// `matcher.fibonacci_lag`: verifica ressonância de Coluna Real (`2 to 1`) nos lags de Fibonacci (0, 1, 2, 4).
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
        let c0 = column_of(w[0]);
        if c0 == 0 {
            return None;
        }
        if column_of(w[1]) == c0 && column_of(w[2]) == c0 && column_of(w[4]) == c0 {
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

/// `matcher.setor_vies`: detecta sobre-representação (>= 55%) de uma Coluna Real (`2 to 1`) em janela de 20 giros.
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
            let c = column_of(n) as usize;
            if (1..=3).contains(&c) {
                counts[c] += 1;
                non_zero += 1;
            }
        }
        if non_zero < 15 {
            return None;
        }
        let latest_col = column_of(w[0]) as usize;
        if !(1..=3).contains(&latest_col) {
            return None;
        }
        let ratio = (counts[latest_col] as f64) / (non_zero as f64);
        if ratio < 0.55 {
            return None;
        }
        let confidence = (0.58 + (ratio - 0.55) * 0.80).min(0.92);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_cols(latest_col as u8),
            confidence,
        })
    }
}

/// `matcher.setor_vies_duzia`: detecta sobre-representação (>= 55%) de uma Dúzia Real (`1st/2nd/3rd 12`) em janela de 20 giros.
#[derive(Debug, Clone, Copy)]
pub struct SetorViesDuzia {
    /// Janela mínima para avaliação de viés.
    pub min_window: usize,
}

impl Default for SetorViesDuzia {
    fn default() -> Self {
        Self { min_window: 20 }
    }
}

impl Matcher for SetorViesDuzia {
    fn id(&self) -> &'static str {
        "setor_vies_duzia"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        if w.len() < self.min_window {
            return None;
        }
        let slice = &w[..self.min_window];
        let mut counts = [0usize; 4];
        let mut non_zero = 0usize;
        for &n in slice {
            let d = dozen_of(n) as usize;
            if (1..=3).contains(&d) {
                counts[d] += 1;
                non_zero += 1;
            }
        }
        if non_zero < 15 {
            return None;
        }
        let latest_doz = dozen_of(w[0]) as usize;
        if !(1..=3).contains(&latest_doz) {
            return None;
        }
        let ratio = (counts[latest_doz] as f64) / (non_zero as f64);
        if ratio < 0.55 {
            return None;
        }
        let confidence = (0.58 + (ratio - 0.55) * 0.80).min(0.92);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_dozens(latest_doz as u8),
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
        let first_even = first % 2 == 0;
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n != 0 && n <= 36 && (n % 2 == 0) == first_even)
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

/// Família de terminais (`cavalo`) observada na timeline: `1-4-7`, `2-5-8` e `3-6-9`.
///
/// A partição vem do resto do dígito por 3, que é como as famílias se separam na grade
/// (`1,4,7` são as colunas 1/2/3 da mesma classe de resto). O final `0` não pertence a
/// nenhuma família: ele é **curinga** e entra em todas as coberturas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinalFamily {
    /// Finais `1, 4, 7`.
    OneFourSeven,
    /// Finais `2, 5, 8`.
    TwoFiveEight,
    /// Finais `3, 6, 9`.
    ThreeSixNine,
}

impl FinalFamily {
    /// Os três finais da família, em ordem crescente.
    pub const fn finals(self) -> [u8; 3] {
        match self {
            Self::OneFourSeven => [1, 4, 7],
            Self::TwoFiveEight => [2, 5, 8],
            Self::ThreeSixNine => [3, 6, 9],
        }
    }

    /// Família do número em `0..=36`, ou `None` para `0` (curinga) e fora de faixa.
    pub const fn of_number(n: u8) -> Option<Self> {
        if n > 36 || n % 10 == 0 {
            return None;
        }
        Some(match (n % 10) % 3 {
            1 => Self::OneFourSeven,
            2 => Self::TwoFiveEight,
            _ => Self::ThreeSixNine,
        })
    }

    /// Se o número pertence à família (o `0` nunca pertence: ele é curinga).
    pub const fn contains(self, n: u8) -> bool {
        matches!(Self::of_number(n), Some(f) if f as u8 == self as u8)
    }

    /// Rótulo curto para a FUI e para o stream de decisões.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OneFourSeven => "1-4-7",
            Self::TwoFiveEight => "2-5-8",
            Self::ThreeSixNine => "3-6-9",
        }
    }
}

/// `matcher.cluster_final`: momentum de família de terminais (`1-4-7` / `2-5-8` / `3-6-9`).
///
/// A mesa fica horas numa família e só troca quando para de entregá-la; por isso a entrada
/// exige **persistência** (streak) e **dominância na janela** (share), e não um hit isolado.
/// A cobertura é o terminal `0` (curinga) mais os três finais da família ativa, sempre em
/// números plenos — nunca em coluna, que é o erro do `cluster_terminal`.
#[derive(Debug, Clone, Copy)]
pub struct ClusterFinal {
    /// Janela usada para medir dominância.
    pub window: usize,
    /// Persistência mínima (spins seguidos da família, contando `0` como continuação).
    pub min_streak: usize,
    /// Fração mínima dos spins não-zero da janela que a família precisa ter.
    pub min_share: f64,
    /// Se inclui `Terminal(0)` em toda entrada (default do operador: sim).
    pub include_zero: bool,
}

impl Default for ClusterFinal {
    fn default() -> Self {
        Self {
            window: 20,
            min_streak: 4,
            min_share: 0.55,
            include_zero: true,
        }
    }
}

impl Matcher for ClusterFinal {
    fn id(&self) -> &'static str {
        "cluster_final"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        if latest > 36 || w.len() < self.min_streak {
            return None;
        }

        let family = w
            .iter()
            .copied()
            .find_map(FinalFamily::of_number)?;

        // `0` é curinga: não quebra a sequência da família dona da mesa.
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n <= 36 && (n % 10 == 0 || family.contains(n)))
            .count();
        if streak < self.min_streak {
            return None;
        }

        let slice = &w[..w.len().min(self.window)];
        let mut hits = 0usize;
        let mut non_zero = 0usize;
        for &n in slice {
            if n == 0 || n > 36 {
                continue;
            }
            non_zero += 1;
            if family.contains(n) {
                hits += 1;
            }
        }
        if non_zero == 0 {
            return None;
        }
        let share = hits as f64 / non_zero as f64;
        if share < self.min_share {
            return None;
        }

        let mut cover = Vec::with_capacity(4);
        if self.include_zero {
            cover.push(Cover::Terminal(0));
        }
        for f in family.finals() {
            cover.push(Cover::Terminal(f));
        }

        let confidence = (0.55 + 0.08 * ((streak - self.min_streak) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover,
            confidence,
        })
    }
}

/// Números distintos cobertos por uma lista de coberturas de número (`Straight`/`Terminal`).
///
/// Devolve `None` quando a lista mistura número com coluna/dúzia/cor: são geometrias de
/// pagamento diferentes e o plano precisa ser compilado por um caminho só (§6 vs `36 − k`).
pub fn number_coverage(covers: &[Cover]) -> Option<Vec<u8>> {
    if covers.is_empty() {
        return None;
    }
    let mut numbers = Vec::new();
    for c in covers {
        match c {
            Cover::Straight(_) | Cover::Terminal(_) => {
                for n in c.numbers() {
                    if !numbers.contains(&n) {
                        numbers.push(n);
                    }
                }
            }
            _ => return None,
        }
    }
    numbers.sort_unstable();
    Some(numbers)
}

/// Faixa de repetições (`min..=max`) que dispara um matcher de sequência.
///
/// O operador ajusta isto pela dashboard (`POST /api/tuning`) conforme o que a mesa
/// está entregando: se a coluna quebra sempre na 4ª repetição, o gatilho desce; se a
/// mesa passa meia hora em 6x, o gatilho sobe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreakRange {
    /// Repetições mínimas para entrar (pré-alerta em `min - 1`, sem aposta).
    pub min: usize,
    /// Repetições máximas: acima disso a sequência é longa demais e o matcher cala.
    pub max: usize,
}

impl StreakRange {
    /// Cria a faixa `min..=max`.
    pub const fn new(min: usize, max: usize) -> Self {
        Self { min, max }
    }

    /// Rejeita faixas que o motor não consegue operar (janela da mesa é de 20 giros).
    pub fn validate(&self, label: &str) -> Result<(), String> {
        if self.min < 2 {
            return Err(format!("{label}: mínimo precisa ser 2 ou mais"));
        }
        if self.max < self.min {
            return Err(format!(
                "{label}: máximo ({}) é menor que o mínimo ({})",
                self.max, self.min
            ));
        }
        if self.max > 20 {
            return Err(format!(
                "{label}: máximo {} não cabe na janela de 20 giros",
                self.max
            ));
        }
        Ok(())
    }
}

/// Ajuste operacional dos cinco matchers de sequência (coluna, dúzia, cor, alto/baixo, par/ímpar).
///
/// É o único ponto de configuração que a dashboard altera em tempo real; os demais matchers
/// (setor, Fibonacci, cluster) têm parâmetros estatísticos e não faixas de repetição.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceTuning {
    /// Repetição da coluna real `2 to 1` (padrão `4..=6`).
    pub coluna: StreakRange,
    /// Repetição da dúzia `1st/2nd/3rd 12` (padrão `4..=6`).
    pub duzia: StreakRange,
    /// Repetição de Vermelho/Preto (padrão `6..=12`).
    pub cor: StreakRange,
    /// Repetição de Baixos/Altos (padrão `6..=12`).
    pub alto_baixo: StreakRange,
    /// Repetição de Par/Ímpar (padrão `6..=12`).
    pub par_impar: StreakRange,
}

impl Default for SequenceTuning {
    fn default() -> Self {
        Self {
            coluna: StreakRange::new(4, 6),
            duzia: StreakRange::new(4, 6),
            cor: StreakRange::new(6, 12),
            alto_baixo: StreakRange::new(6, 12),
            par_impar: StreakRange::new(6, 12),
        }
    }
}

impl SequenceTuning {
    /// Valida todas as faixas antes de aplicar (a dashboard mostra a mensagem de erro).
    pub fn validate(&self) -> Result<(), String> {
        self.coluna.validate("coluna")?;
        self.duzia.validate("dúzia")?;
        self.cor.validate("cor")?;
        self.alto_baixo.validate("alto/baixo")?;
        self.par_impar.validate("par/ímpar")?;
        Ok(())
    }

    /// Monta o conjunto completo de matchers com estas faixas aplicadas.
    pub fn matchers(&self) -> Vec<Arc<dyn Matcher>> {
        vec![
            Arc::new(QuebraColuna {
                min: self.coluna.min,
                max: self.coluna.max,
            }),
            Arc::new(QuebraDuzia {
                min: self.duzia.min,
                max: self.duzia.max,
            }),
            Arc::new(QuebraCor {
                min: self.cor.min,
                max: self.cor.max,
            }),
            Arc::new(QuebraAltoBaixo {
                min: self.alto_baixo.min,
                max: self.alto_baixo.max,
            }),
            Arc::new(QuebraParImpar {
                min: self.par_impar.min,
                max: self.par_impar.max,
            }),
            Arc::new(SetorVies::default()),
            Arc::new(SetorViesDuzia::default()),
            Arc::new(FibonacciLag),
            Arc::new(ClusterTerminal::default()),
            Arc::new(ClusterFinal::default()),
        ]
    }
}

/// Constrói o conjunto padrão completo de matchers nativos do projeto MONSTER:
/// - Colunas (`2 to 1`) e Dúzias (`1st/2nd/3rd 12`): pré-alerta em `min - 1` (`0.50`) e entrada a partir de `min` (`>= 0.66`)
/// - Vermelho/Preto, Baixo/Alto e Par/Ímpar: pré-alerta em `min - 1` (`0.50`) e entrada a partir de `min` (`>= 0.68`)
/// - Viés de Setor Coluna (`setor_vies`), Viés de Setor Dúzia (`setor_vies_duzia`), Fibonacci Lag e Cluster Terminal.
pub fn default_matchers() -> Vec<Arc<dyn Matcher>> {
    SequenceTuning::default().matchers()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quebra_coluna_triggers_on_true_column_streak_4_to_6_and_covers_other_two() {
        let matcher = QuebraColuna::default();
        // Streak de 3 na Coluna 1 (1, 4, 7): pré-alerta de observação (conf 0.50 < 0.55)
        let pre3 = matcher.evaluate(&[1, 4, 7, 20]).expect("streak 3 pre-alert");
        assert!((pre3.confidence - 0.50).abs() < 1e-9);

        // Streak de 4 na Coluna 1 (1, 4, 7, 10): dispara entrada (conf 0.68) cobrindo Col(2) e Col(3)
        let intent4 = matcher.evaluate(&[1, 4, 7, 10, 20]).expect("streak 4 must trigger");
        assert_eq!(intent4.strategy_id, "quebra_permanencia_coluna");
        assert_eq!(intent4.cover, vec![Cover::Col(2), Cover::Col(3)]);
        assert!((intent4.confidence - 0.68).abs() < 1e-9);

        // Streak de 6 na Coluna 2 (2, 5, 8, 11, 14, 17): dispara cobrindo Col(1) e Col(3)
        let intent6 = matcher
            .evaluate(&[2, 5, 8, 11, 14, 17, 1])
            .expect("streak 6 must trigger");
        assert_eq!(intent6.cover, vec![Cover::Col(1), Cover::Col(3)]);
        assert!((intent6.confidence - 0.82).abs() < 1e-9);

        // Streak de 7 excede max=6: não dispara
        assert!(matcher.evaluate(&[2, 5, 8, 11, 14, 17, 20, 1]).is_none());
    }

    #[test]
    fn tuning_antecipa_o_gatilho_da_coluna_e_da_duzia() {
        let tuning = SequenceTuning {
            coluna: StreakRange::new(3, 5),
            duzia: StreakRange::new(3, 5),
            ..SequenceTuning::default()
        };
        assert!(tuning.validate().is_ok());
        let matchers = tuning.matchers();
        let col = matchers
            .iter()
            .find(|m| m.id() == "quebra_permanencia_coluna")
            .expect("matcher de coluna presente");

        // Com min=3: streak 2 é pré-alerta (0.50, nunca aposta) e streak 3 já entra.
        // [1, 4] são coluna 1 e 20 é coluna 2: o streak é 2.
        let pre = col.evaluate(&[1, 4, 20]).expect("streak 2 pre-alert");
        assert!((pre.confidence - 0.50).abs() < 1e-9);
        let entry = col.evaluate(&[1, 4, 7, 20]).expect("streak 3 entra");
        assert!((entry.confidence - 0.68).abs() < 1e-9);
        assert_eq!(entry.cover, vec![Cover::Col(2), Cover::Col(3)]);

        // O teto ajustado corta em 5: streak 6 deixa de disparar.
        let wide = col.evaluate(&[1, 4, 7, 10, 13, 20]).expect("streak 5 entra");
        assert!((wide.confidence - 0.82).abs() < 1e-9);
        assert!(col.evaluate(&[1, 4, 7, 10, 13, 16, 20]).is_none());
    }

    #[test]
    fn tuning_rejeita_faixas_impossiveis() {
        let inverted = SequenceTuning {
            cor: StreakRange::new(9, 6),
            ..SequenceTuning::default()
        };
        assert!(inverted.validate().is_err(), "max < min precisa ser rejeitado");
        let too_long = SequenceTuning {
            alto_baixo: StreakRange::new(4, 21),
            ..SequenceTuning::default()
        };
        assert!(too_long.validate().is_err(), "max acima da janela de 20 giros");
        let too_small = SequenceTuning {
            par_impar: StreakRange::new(1, 6),
            ..SequenceTuning::default()
        };
        assert!(too_small.validate().is_err(), "min 1 não é sequência");
    }

    #[test]
    fn terminal_zero_covers_zero_ten_twenty_thirty() {
        let t0 = Cover::Terminal(0);
        assert_eq!(t0.numbers(), vec![0, 10, 20, 30]);
        assert!(t0.matches_number(0), "o 0 é o próprio final 0");
        assert_eq!(Cover::Terminal(7).numbers(), vec![7, 17, 27]);
        assert_eq!(Cover::Terminal(1).numbers(), vec![1, 11, 21, 31]);
    }

    #[test]
    fn final_family_partition_matches_operator_clusters() {
        assert_eq!(FinalFamily::of_number(1), Some(FinalFamily::OneFourSeven));
        assert_eq!(FinalFamily::of_number(4), Some(FinalFamily::OneFourSeven));
        assert_eq!(FinalFamily::of_number(7), Some(FinalFamily::OneFourSeven));
        assert_eq!(FinalFamily::of_number(32), Some(FinalFamily::TwoFiveEight));
        assert_eq!(FinalFamily::of_number(9), Some(FinalFamily::ThreeSixNine));
        assert_eq!(FinalFamily::of_number(36), Some(FinalFamily::ThreeSixNine));
        // 0 e todo número terminado em 0 são curingas, não pertencem a família.
        assert_eq!(FinalFamily::of_number(0), None);
        assert_eq!(FinalFamily::of_number(30), None);
        assert_eq!(FinalFamily::of_number(37), None);
    }

    #[test]
    fn cluster_final_triggers_on_family_persistence_and_covers_numbers_not_columns() {
        let m = ClusterFinal::default();
        // 1, 7, 4 da família 1-4-7 com um 20 (final 0) no meio: o 0 não quebra a família.
        // Janela real de 20 spins da timeline, com 1-4-7 dominando.
        let w = [
            1, 4, 0, 7, 20, 13, 22, 1, 4, 7, 31, 16, 25, 7, 4, 1, 10, 28, 7, 4,
        ];
        let intent = m
            .evaluate(&w)
            .expect("cluster 1-4-7 persistente deve disparar");
        assert_eq!(intent.strategy_id, "cluster_final");
        assert_eq!(
            intent.cover,
            vec![
                Cover::Terminal(0),
                Cover::Terminal(1),
                Cover::Terminal(4),
                Cover::Terminal(7)
            ]
        );
        // A cobertura é de NÚMEROS: nada de coluna no cover.
        assert!(intent.cover.iter().all(|c| matches!(c, Cover::Terminal(_))));
        assert_eq!(number_coverage(&intent.cover).map(|v| v.len()), Some(15));
        // streak = 5 (1,4,0,7,20) -> 0.55 + 0.08 * (5-4)
        assert!((intent.confidence - 0.63).abs() < 1e-9);
    }

    #[test]
    fn cluster_final_ignores_alternating_families() {
        let m = ClusterFinal::default();
        // Troca de família a cada spin: nenhuma recorrência para seguir.
        assert!(m.evaluate(&[1, 2, 7, 8, 4, 5]).is_none());
        // Persistência curta (3, sem curinga) não basta.
        assert!(m.evaluate(&[1, 4, 7, 32, 22, 13]).is_none());
    }

    #[test]
    fn cluster_final_requires_dominance_in_the_window() {
        let m = ClusterFinal::default();
        // 4 seguidos de 1-4-7, mas a janela de 20 é dominada por 2-5-8 (share baixo).
        let w = [
            1, 4, 7, 11, 2, 5, 8, 20, 2, 5, 8, 29, 2, 5, 8, 20, 2, 5, 8, 32,
        ];
        assert!(m.evaluate(&w).is_none());
    }

    #[test]
    fn number_coverage_rejects_mixed_geometry_and_dedupes() {
        assert_eq!(number_coverage(&[]), None);
        assert_eq!(
            number_coverage(&[Cover::Col(1), Cover::Terminal(3)]),
            None,
            "coluna + terminal é geometria de pagamento mista"
        );
        let dupes = number_coverage(&[Cover::Terminal(1), Cover::Straight(11)])
            .expect("terminal 1 + pleno 11");
        assert_eq!(dupes, vec![1, 11, 21, 31]);
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
