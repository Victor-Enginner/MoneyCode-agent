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
        if hits < self.min_hits {
            return None;
        }
        // Final repetido é jogada de NÚMERO (os plenos daquele final), nunca de coluna:
        // a versão antiga apostava na coluna do último giro + a seguinte, sem relação
        // nenhuma com o final, e ainda disparava com 3 hits. Regra do operador (13/11).
        let confidence = (0.64 + 0.07 * ((hits - self.min_hits) as f64)).min(0.90);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: vec![Cover::Terminal(term)],
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

/// `matcher.quebra_numero_repetido`: o mesmo número pleno duas vezes seguidas quebra nas
/// colunas que **não** o contêm.
///
/// Regra do operador, medida na base real: em 21 repetições exatas, 15 quebraram fora da coluna
/// do número repetido (71% contra os 65% que duas colunas já entregam por si). É gatilho de
/// permanência — mesma geometria de fora (2 colunas), mesma escada ×3 do §6.
#[derive(Debug, Clone, Copy)]
pub struct QuebraNumeroRepetido {
    /// Repetições exatas exigidas para disparar (`2`).
    pub min_repeat: usize,
}

impl Default for QuebraNumeroRepetido {
    fn default() -> Self {
        Self { min_repeat: 2 }
    }
}

impl Matcher for QuebraNumeroRepetido {
    fn id(&self) -> &'static str {
        "quebra_numero_repetido"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        let col = column_of(latest);
        // `0` não pertence a coluna nenhuma: não existe "coluna do número" para quebrar.
        if col == 0 {
            return None;
        }
        let repeats = w.iter().copied().take_while(|&n| n == latest).count();
        if repeats < self.min_repeat.max(2) {
            return None;
        }
        let confidence = (0.60 + 0.05 * ((repeats - self.min_repeat) as f64)).min(0.80);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover: other_two_cols(col),
            confidence,
        })
    }
}

/// `matcher.terminal_quente`: entrada **por número** nos finais que o operador segue (`2,3,6,9`).
///
/// Diferente do `cluster_final`, que cobre a família inteira (`0` + 3 finais = 15 números), aqui
/// a cobertura é limitada aos `max_terminals` finais mais frequentes da janela: no máximo 8
/// plenos. A mesa tem ficha mínima de 10¢ e 15 plenos custam 150¢ por rodada — três vezes a
/// entrada de fora — então a cobertura precisa ser estreita para a escada caber no §6.
#[derive(Debug, Clone, Copy)]
pub struct TerminalQuente {
    /// Janela de dominância (`20`).
    pub window: usize,
    /// Finais que o operador opera (`2,3,6,9`).
    pub terminals: [u8; 4],
    /// Quantos finais cobrir por entrada (`2` → até 8 plenos).
    pub max_terminals: usize,
    /// Persistência mínima da família, contando `0` como curinga (`3`).
    pub min_streak: usize,
    /// Fração mínima de `2,3,6,9` entre os giros não-zero da janela (`0.44`).
    pub min_share: f64,
}

impl Default for TerminalQuente {
    fn default() -> Self {
        Self {
            window: 20,
            terminals: [2, 3, 6, 9],
            max_terminals: 2,
            min_streak: 3,
            min_share: 0.44,
        }
    }
}

impl Matcher for TerminalQuente {
    fn id(&self) -> &'static str {
        "terminal_quente"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        if latest > 36 || w.len() < self.min_streak {
            return None;
        }
        let in_set = |n: u8| n <= 36 && self.terminals.contains(&(n % 10));
        // `0` não tem final definido: conta como continuação, nunca como quebra.
        let streak = w
            .iter()
            .copied()
            .take_while(|&n| n % 10 == 0 || in_set(n))
            .count();
        if streak < self.min_streak {
            return None;
        }

        let slice = &w[..w.len().min(self.window)];
        let mut counts = [0usize; 10];
        let mut non_zero = 0usize;
        let mut hits = 0usize;
        for &n in slice {
            if n == 0 || n > 36 {
                continue;
            }
            non_zero += 1;
            if in_set(n) {
                hits += 1;
                counts[(n % 10) as usize] += 1;
            }
        }
        if non_zero == 0 {
            return None;
        }
        let share = hits as f64 / non_zero as f64;
        if share < self.min_share {
            return None;
        }

        // Finais mais frequentes da janela; empate resolve pelo menor final (determinístico).
        let mut ranked = self.terminals.to_vec();
        ranked.sort_by_key(|&t| (std::cmp::Reverse(counts[t as usize]), t));
        ranked.truncate(self.max_terminals.clamp(1, self.terminals.len()));
        let cover = ranked.into_iter().map(Cover::Terminal).collect();

        let confidence = (0.55 + 0.07 * ((streak - self.min_streak) as f64)).min(0.95);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover,
            confidence,
        })
    }
}

/// Mapa de terminais da Estratégia Invictor: múltiplos `2,4,6,8`, quebrantes `1,3,5,7,9`
/// e protetivo `0` (sempre coberto). `0` como número sorteado é neutro: não abre nem quebra
/// sequência de múltiplos.
pub mod invictor {
    /// Finais múltiplos.
    pub const MULTIPLOS: [u8; 4] = [2, 4, 6, 8];
    /// Finais quebrantes.
    pub const QUEBRANTES: [u8; 5] = [1, 3, 5, 7, 9];

    /// Final é múltiplo (`2,4,6,8`).
    pub const fn is_multiplo(n: u8) -> bool {
        n <= 36 && matches!(n % 10, 2 | 4 | 6 | 8)
    }

    /// Final é quebrante (`1,3,5,7,9`).
    pub const fn is_quebrante(n: u8) -> bool {
        n <= 36 && n % 2 == 1
    }

    /// Quebrantes ordenados do mais "devente" (há mais giros sem sair) ao menos devente.
    /// Final ausente da janela inteira vale `len` giros; empate resolve pelo menor final.
    pub fn quebrantes_por_atraso(w: &[u8]) -> Vec<u8> {
        let mut v: Vec<(usize, u8)> = QUEBRANTES
            .iter()
            .map(|&t| {
                let gap = w
                    .iter()
                    .position(|&n| n <= 36 && n % 10 == t)
                    .unwrap_or(w.len());
                (gap, t)
            })
            .collect();
        v.sort_by_key(|&(gap, t)| (std::cmp::Reverse(gap), t));
        v.into_iter().map(|(_, t)| t).collect()
    }
}

fn push_terminal(cover: &mut Vec<Cover>, t: u8) {
    if !cover.contains(&Cover::Terminal(t)) {
        cover.push(Cover::Terminal(t));
    }
}

/// `matcher.invictor_conservadora`: gatilho **quebrante seguido de múltiplo** (`w[1]`
/// quebrante, `w[0]` múltiplo). Cobre: protetivo `0`, o último quebrante que saiu, o
/// quebrante mais devente da janela e o múltiplo que acabou de sair — até 4 finais.
#[derive(Debug, Clone, Copy)]
pub struct InvictorConservadora {
    /// Janela para medir o quebrante mais devente (`20`).
    pub window: usize,
}

impl Default for InvictorConservadora {
    fn default() -> Self {
        Self { window: 20 }
    }
}

impl Matcher for InvictorConservadora {
    fn id(&self) -> &'static str {
        "invictor_conservadora"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let (&latest, &prev) = (w.first()?, w.get(1)?);
        if latest == 0 || !invictor::is_multiplo(latest) || !invictor::is_quebrante(prev) {
            return None;
        }
        let slice = &w[..w.len().min(self.window)];
        let mut cover = vec![Cover::Terminal(0)];
        push_terminal(&mut cover, prev % 10);
        if let Some(&devente) = invictor::quebrantes_por_atraso(slice)
            .iter()
            .find(|&&t| t != prev % 10)
        {
            push_terminal(&mut cover, devente);
        }
        push_terminal(&mut cover, latest % 10);
        Some(BetIntent {
            v: 1,
            strategy_id: self.id().to_string(),
            cover,
            confidence: 0.60,
        })
    }
}

/// Gatilho de um script do operador (corpus/cards/script_*.yaml).
#[derive(Debug, Clone, Copy)]
pub enum ScriptTrigger {
    /// Último giro é um destes números ("quando sair qualquer espelho ou gêmeo").
    LastSpinIn(&'static [u8]),
    /// Os `n` últimos giros terminam num destes finais (família ativa).
    LastFinalsIn { n: usize, finals: &'static [u8] },
}

/// Script do operador em live: gatilho + entrada + proteções, todos em pleno com a mesma
/// ficha da escada de número. Máximo de degraus próprio (`SCRIPT_MAX_STEPS` no core).
#[derive(Debug, Clone, Copy)]
pub struct OperatorScript {
    pub id: &'static str,
    pub trigger: ScriptTrigger,
    /// Entrada + proteções do cartão.
    pub pockets: &'static [u8],
}

/// Os três scripts liberados pelo operador em 14/11/2026 (cartões em corpus/cards).
pub const OPERATOR_SCRIPTS: [OperatorScript; 3] = [
    OperatorScript {
        id: "script_espelhos_gemeos",
        trigger: ScriptTrigger::LastSpinIn(&[11, 12, 13, 21, 22, 23, 31, 32, 33]),
        pockets: &[2, 3, 4, 8, 9, 10, 11, 12, 13, 21, 22, 23, 26, 27, 30, 31, 32, 33, 35, 36],
    },
    OperatorScript {
        id: "script_familia_369",
        trigger: ScriptTrigger::LastFinalsIn { n: 2, finals: &[3, 6, 9] },
        pockets: &[3, 6, 8, 9, 13, 15, 16, 18, 19, 22, 23, 26, 27, 29, 31, 32, 33, 35, 36],
    },
    OperatorScript {
        id: "script_familia_057",
        trigger: ScriptTrigger::LastFinalsIn { n: 2, finals: &[0, 5, 7] },
        pockets: &[0, 5, 7, 8, 10, 11, 12, 13, 15, 17, 20, 23, 25, 26, 27, 28, 30, 33, 35, 36],
    },
];

impl Matcher for OperatorScript {
    fn id(&self) -> &'static str {
        self.id
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let fired = match self.trigger {
            ScriptTrigger::LastSpinIn(nums) => nums.contains(w.first()?),
            ScriptTrigger::LastFinalsIn { n, finals } => {
                w.len() >= n && w[..n].iter().all(|x| finals.contains(&(x % 10)))
            }
        };
        if !fired {
            return None;
        }
        Some(BetIntent {
            v: 1,
            strategy_id: self.id.to_string(),
            cover: self.pockets.iter().map(|&n| Cover::Straight(n)).collect(),
            confidence: 0.60,
        })
    }
}

/// `matcher.invictor_arrojada`: sequência de múltiplos no topo da timeline (`0` neutro).
/// Padrão **Curto** (`2..=3`) ou **Alongado** (`5..=8`) → entra só nos quebrantes, os
/// `max_quebrantes` mais deventes, mais o protetivo `0`. Run de 4 ou > 8 não dispara.
#[derive(Debug, Clone, Copy)]
pub struct InvictorArrojada {
    /// Janela para medir atraso dos quebrantes (`20`).
    pub window: usize,
    /// Padrão curto (`2..=3`).
    pub curto: StreakRange,
    /// Padrão alongado (`5..=8`).
    pub alongado: StreakRange,
    /// Quantos quebrantes cobrir (`3` → 10 plenos + 4 do zero).
    pub max_quebrantes: usize,
}

impl Default for InvictorArrojada {
    fn default() -> Self {
        Self {
            window: 20,
            curto: StreakRange::new(2, 3),
            alongado: StreakRange::new(5, 8),
            max_quebrantes: 3,
        }
    }
}

impl Matcher for InvictorArrojada {
    fn id(&self) -> &'static str {
        "invictor_arrojada"
    }

    fn evaluate(&self, w: &[u8]) -> Option<BetIntent> {
        let latest = *w.first()?;
        if !invictor::is_multiplo(latest) || latest == 0 {
            return None;
        }
        let run = w
            .iter()
            .copied()
            .take_while(|&n| n == 0 || invictor::is_multiplo(n))
            .filter(|&n| n != 0)
            .count();
        let in_range = |r: StreakRange| run >= r.min && run <= r.max;
        let confidence = if in_range(self.alongado) {
            0.62
        } else if in_range(self.curto) {
            0.56
        } else {
            return None;
        };
        let slice = &w[..w.len().min(self.window)];
        let mut cover = vec![Cover::Terminal(0)];
        for t in invictor::quebrantes_por_atraso(slice)
            .into_iter()
            .take(self.max_quebrantes.clamp(1, 5))
        {
            push_terminal(&mut cover, t);
        }
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
            Arc::new(QuebraNumeroRepetido::default()),
            Arc::new(TerminalQuente::default()),
            Arc::new(InvictorConservadora::default()),
            Arc::new(InvictorArrojada::default()),
            Arc::new(OPERATOR_SCRIPTS[0]),
            Arc::new(OPERATOR_SCRIPTS[1]),
            Arc::new(OPERATOR_SCRIPTS[2]),
        ]
    }
}

/// Ids de todos os matchers nativos, na ordem em que o DAG os avalia.
///
/// É a allowlist do interruptor da dashboard: desligar exige conhecer o nome
/// exato, então erro de digitação vira rejeição e não matcher silenciosamente ligado.
pub const MATCHER_IDS: [&str; 17] = [
    "quebra_permanencia_coluna",
    "quebra_permanencia_duzia",
    "quebra_cor_6x",
    "quebra_alto_baixo_6x",
    "quebra_par_impar_6x",
    "setor_vies",
    "setor_vies_duzia",
    "fibonacci_lag",
    "cluster_terminal",
    "cluster_final",
    "quebra_numero_repetido",
    "terminal_quente",
    "invictor_conservadora",
    "invictor_arrojada",
    "script_espelhos_gemeos",
    "script_familia_369",
    "script_familia_057",
];

/// Ajuste completo da leitura de padrões: faixas de repetição + interruptores.
///
/// As faixas vêm achatadas no JSON (`coluna`, `duzia`, ...) para o arquivo ficar
/// legível; `disabled` é opcional, então um `tuning.json` antigo continua válido.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatcherTuning {
    /// Faixas `min..=max` dos cinco matchers de sequência.
    #[serde(flatten)]
    pub sequences: SequenceTuning,
    /// Ids desligados pelo operador: não emitem sinal, mas seguem no binário.
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl Default for MatcherTuning {
    /// Os dois matchers de viés de setor nascem **desligados**: medem 55% de
    /// ocupação numa janela de 20 giros, e em n=20 isso é ruído — a exigência
    /// estatística do §3 (n >= 185 e chi-quadrado significativo) não é atendida.
    /// Ligar de volta é um clique na dashboard, com o custo assumido.
    fn default() -> Self {
        Self {
            sequences: SequenceTuning::default(),
            disabled: vec!["setor_vies".to_string(), "setor_vies_duzia".to_string()],
        }
    }
}

impl MatcherTuning {
    /// Valida as faixas e recusa id desconhecido no interruptor.
    pub fn validate(&self) -> Result<(), String> {
        self.sequences.validate()?;
        for id in &self.disabled {
            if !MATCHER_IDS.contains(&id.as_str()) {
                return Err(format!("matcher desconhecido: {id}"));
            }
        }
        Ok(())
    }

    /// Matchers ligados agora (todos menos os desligados na dashboard).
    pub fn matchers(&self) -> Vec<Arc<dyn Matcher>> {
        self.sequences
            .matchers()
            .into_iter()
            .filter(|m| !self.disabled.iter().any(|d| d == m.id()))
            .collect()
    }
}

/// Constrói o conjunto padrão completo de matchers nativos do projeto MONSTER:
/// - Colunas (`2 to 1`) e Dúzias (`1st/2nd/3rd 12`): pré-alerta em `min - 1` (`0.50`) e entrada a partir de `min` (`>= 0.66`)
/// - Vermelho/Preto, Baixo/Alto e Par/Ímpar: pré-alerta em `min - 1` (`0.50`) e entrada a partir de `min` (`>= 0.68`)
/// - Viés de Setor Coluna (`setor_vies`), Viés de Setor Dúzia (`setor_vies_duzia`), Fibonacci Lag e Cluster Terminal.
///
/// Conjunto completo de matchers, sem o filtro do interruptor: é a allowlist do §8.
/// Quem decide o que está ligado em produção é `MatcherTuning::matchers()`.
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
    fn padrao_desliga_os_vies_de_setor_de_janela_curta() {
        let default = MatcherTuning::default();
        assert!(default.validate().is_ok());
        let ids: Vec<&str> = default.matchers().iter().map(|m| m.id()).collect();
        assert!(!ids.contains(&"setor_vies"), "viés de setor em 20 giros é ruído");
        assert!(!ids.contains(&"setor_vies_duzia"));
        assert_eq!(ids.len(), MATCHER_IDS.len() - 2);
        // Os de repetição continuam ligados: é o gatilho que o operador espera.
        assert!(ids.contains(&"quebra_permanencia_duzia"));
        assert!(ids.contains(&"quebra_permanencia_coluna"));
    }

    #[test]
    fn matcher_ids_cobrem_exatamente_o_conjunto_padrao() {
        let padrao: Vec<&str> = default_matchers().iter().map(|m| m.id()).collect();
        assert_eq!(padrao, MATCHER_IDS, "allowlist do interruptor fora de sincronia");
    }

    #[test]
    fn interruptor_desliga_so_o_matcher_escolhido_e_valida_id() {
        let tuning = MatcherTuning {
            sequences: SequenceTuning { coluna: StreakRange::new(3, 5), ..SequenceTuning::default() },
            disabled: vec!["cluster_terminal".to_string(), "fibonacci_lag".to_string()],
        };
        assert!(tuning.validate().is_ok());
        let ids: Vec<&str> = tuning.matchers().iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), MATCHER_IDS.len() - 2);
        assert!(!ids.contains(&"cluster_terminal"));
        assert!(!ids.contains(&"fibonacci_lag"));
        // As faixas continuam valendo para o que ficou ligado.
        let col = tuning
            .matchers()
            .into_iter()
            .find(|m| m.id() == "quebra_permanencia_coluna")
            .expect("coluna segue ligada");
        assert!(col.evaluate(&[1, 4, 7, 20]).is_some());

        let typo = MatcherTuning {
            disabled: vec!["cluster_termial".to_string()],
            ..MatcherTuning::default()
        };
        assert!(typo.validate().is_err(), "id com typo precisa ser rejeitado");
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
    fn numero_repetido_quebra_nas_colunas_que_nao_o_contem() {
        let m = QuebraNumeroRepetido::default();
        // 8 é coluna 2 (2,5,8...): a cobertura é coluna 1 + coluna 3.
        let intent = m.evaluate(&[8, 8, 5, 11]).expect("8 duas vezes dispara");
        assert_eq!(intent.strategy_id, "quebra_numero_repetido");
        assert_eq!(intent.cover, vec![Cover::Col(1), Cover::Col(3)]);
        assert!((intent.confidence - 0.60).abs() < 1e-9);

        // 34 é coluna 1: cobre 2 e 3. Três repetições sobem a confiança.
        let tri = m.evaluate(&[34, 34, 34, 1]).expect("34 três vezes dispara");
        assert_eq!(tri.cover, vec![Cover::Col(2), Cover::Col(3)]);
        assert!((tri.confidence - 0.65).abs() < 1e-9);

        // Número diferente e zero não disparam: 0 não pertence a coluna nenhuma.
        assert!(m.evaluate(&[8, 23, 5]).is_none());
        assert!(m.evaluate(&[0, 0, 5]).is_none());
        assert!(m.evaluate(&[]).is_none());
    }

    #[test]
    fn terminal_quente_cobre_no_maximo_dois_finais_em_plenos() {
        let m = TerminalQuente::default();
        // Janela real de 20 giros com 2,3,6,9 dominando (15 de 20 não-zero).
        let w = [
            23, 32, 9, 26, 13, 6, 12, 16, 2, 33, 19, 9, 22, 30, 4, 13, 36, 8, 27, 5,
        ];
        let intent = m.evaluate(&w).expect("terminais em série devem disparar");
        assert_eq!(intent.strategy_id, "terminal_quente");
        assert_eq!(intent.cover.len(), 2, "cobertura estreita: 2 finais");
        assert!(intent.cover.iter().all(|c| matches!(c, Cover::Terminal(_))));
        let numbers = number_coverage(&intent.cover).expect("cobertura de número");
        assert!(numbers.len() <= 8, "nunca 15 plenos: {}", numbers.len());
        assert_eq!(intent.cover, vec![Cover::Terminal(2), Cover::Terminal(3)]);
        // streak 14 -> 0.55 + 0.07*11 satura em 0.95
        assert!((intent.confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn terminal_quente_exige_persistencia_e_dominancia() {
        let m = TerminalQuente::default();
        // Um único final da família no giro mais novo não arma nada.
        assert!(m.evaluate(&[2, 5, 8, 11, 4, 7]).is_none());
        // Streak de 3, mas a janela é dominada por 5-8-11: share abaixo do mínimo.
        let mut w = vec![3u8, 6, 9];
        for _ in 0..6 {
            w.extend_from_slice(&[5, 8, 11]);
        }
        assert!(m.evaluate(&w).is_none());
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
    #[test]
    fn invictor_conservadora_quebrante_seguido_de_multiplo() {
        // w[0] mais recente: saiu 7 (quebrante) e depois 12 (múltiplo).
        let w = [12, 7, 4, 2, 8, 6, 22, 14];
        let i = InvictorConservadora::default().evaluate(&w).expect("dispara");
        assert_eq!(i.strategy_id, "invictor_conservadora");
        assert_eq!(i.cover[0], Cover::Terminal(0));
        assert_eq!(i.cover[1], Cover::Terminal(7));
        assert_eq!(i.cover[2], Cover::Terminal(1)); // 1,3,5,9 ausentes: menor final empata
        assert_eq!(i.cover[3], Cover::Terminal(2));
        assert!(InvictorConservadora::default().evaluate(&[7, 12, 3]).is_none());
        assert!(InvictorConservadora::default().evaluate(&[12, 4, 3]).is_none());
    }

    #[test]
    fn scripts_do_operador_disparam_como_no_caderno() {
        let espelhos = OPERATOR_SCRIPTS[0];
        let i = espelhos.evaluate(&[33, 33, 3]).expect("33 é gêmeo");
        assert_eq!(i.cover.len(), 20);
        assert!(i.cover.contains(&Cover::Straight(26)), "proteção 26 entra");
        assert!(espelhos.evaluate(&[34, 33]).is_none(), "só o último giro dispara");
        let f057 = OPERATOR_SCRIPTS[2];
        assert!(f057.evaluate(&[30, 10, 4]).is_some(), "30 e 10: finais 0 seguidos");
        assert!(f057.evaluate(&[30, 4]).is_none());
        let f369 = OPERATOR_SCRIPTS[1];
        assert_eq!(f369.evaluate(&[9, 33]).expect("dispara").cover.len(), 19);
        assert!(MATCHER_IDS.contains(&"script_familia_369"));
    }

    #[test]
    fn invictor_arrojada_curto_alongado_e_buraco_em_4() {
        let m = InvictorArrojada::default();
        let curto = m.evaluate(&[2, 14, 7, 9]).expect("curto 2");
        assert_eq!(curto.cover.len(), 4);
        assert_eq!(curto.cover[0], Cover::Terminal(0));
        assert!(curto.cover[1..].iter().all(|c| matches!(c, Cover::Terminal(t) if t % 2 == 1)));
        assert!(m.evaluate(&[2, 4, 6, 8, 1]).is_none()); // run 4
        assert!(m.evaluate(&[2, 4, 0, 6, 8, 12, 1]).is_some()); // 0 neutro, run 5
        assert!(m.evaluate(&[2, 7]).is_none()); // run 1
        assert!(m.evaluate(&[7, 2, 4]).is_none()); // topo é quebrante
    }

    #[test]
    fn cluster_terminal_aposta_nos_numeros_do_final_nunca_em_coluna() {
        let m = ClusterTerminal::default();
        // Fita real de 13/11: 25, 12, 36, 34, 29, 15, 5, 15 → final 5 quatro vezes.
        let w = [25, 12, 36, 34, 29, 15, 5, 15, 16, 32, 12, 36];
        let intent = m.evaluate(&w).expect("final 5 com 4 hits dispara");
        assert_eq!(intent.cover, vec![Cover::Terminal(5)]);
        assert!(intent.cover.iter().all(|c| !matches!(c, Cover::Col(_))));
    }

    #[test]
    fn cluster_terminal_nao_dispara_com_3_hits() {
        let m = ClusterTerminal::default();
        assert!(m.evaluate(&[15, 5, 12, 25, 36, 1, 2, 3]).is_none());
    }
}
