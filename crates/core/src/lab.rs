// SPDX-License-Identifier: Apache-2.0
//! Laboratório de hipóteses: testa cada padrão/modelo contra o acaso em giros reais deduplicados.
//! Divisão temporal treino (60%) / validação (20%) / teste oculto (20%); o veredito usa só o teste,
//! com correção de Holm para múltiplas hipóteses. Nada aqui decide aposta: é medição, fora do DAG.

use monster_store::{SqlitePool, StoreError};
use monster_strategy_native::{Cover, Matcher};
use serde::Serialize;
use std::sync::Arc;

/// Intervalo mínimo entre giros reais: rajadas abaixo disso são a fita regravada.
pub const MIN_REAL_GAP_MS: i64 = 20_000;
/// Janela de contexto que cada hipótese enxerga antes de apostar.
pub const CONTEXT: usize = 30;
/// Apostas mínimas no teste oculto para emitir veredito.
pub const MIN_TEST_BETS: usize = 50;
/// Nível de significância global (corrigido por Holm).
pub const ALPHA: f64 = 0.05;

/// Giros reais em ordem cronológica (mais antigo primeiro), sem rajadas de fita regravada.
pub fn load_clean_spins(pool: &SqlitePool) -> Result<(usize, Vec<u8>), StoreError> {
    let rows: Vec<(i64, i64)> = pool
        .with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT number, ts FROM spins ORDER BY ts ASC, id ASC")?;
            let it = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            it.collect::<Result<Vec<_>, _>>()
        })
        .map_err(StoreError::Database)?;
    Ok((rows.len(), dedupe(&rows)))
}

/// Mantém um giro só se ele vier `MIN_REAL_GAP_MS` depois do último giro mantido.
pub fn dedupe(rows: &[(i64, i64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len());
    let mut last: Option<i64> = None;
    for &(n, ts) in rows {
        if !(0..=36).contains(&n) {
            continue;
        }
        if last.is_none_or(|l| ts - l >= MIN_REAL_GAP_MS) {
            out.push(n as u8);
            last = Some(ts);
        }
    }
    out
}

/// Uma hipótese: dado o passado (cronológico, último = mais recente), quais números apostar.
pub enum Hypothesis {
    /// Matcher nativo do DAG (as estratégias que já operam).
    Matcher(Arc<dyn Matcher>),
    /// Os `k` terminais mais frequentes nos últimos `w` giros ("ondas" de terminal).
    HotTerminal { w: usize, k: usize },
    /// Os 5 números mais frequentes no TREINO (procura viés físico da roda).
    FrequencyBias,
    /// Cadeia de Markov de coluna: P(coluna seguinte | coluna atual) aprendida no TREINO.
    MarkovColumn,
    /// "Puxada": os 2 finais que mais saíram logo depois do final atual, aprendidos no TREINO.
    MarkovTerminal,
    /// Final mais forte (maior z) nos últimos `w` giros, ponderado pelo tamanho (4 ou 3 números).
    TerminalForte { w: usize },
}

/// Quantos números tem cada final: 0–6 têm 4 (0,10,20,30 …), 7–9 têm 3.
pub const TERMINAL_SIZE: [u32; 10] = [4, 4, 4, 4, 4, 4, 4, 3, 3, 3];

fn terminal_numbers(ts: &[u8]) -> Vec<u8> {
    (0..=36u8).filter(|n| ts.contains(&(n % 10))).collect()
}

impl Hypothesis {
    fn id(&self) -> String {
        match self {
            Self::Matcher(m) => m.id().to_string(),
            Self::HotTerminal { w, k } => format!("ml_terminal_quente_w{w}_top{k}"),
            Self::FrequencyBias => "ml_vies_frequencia_top5".to_string(),
            Self::MarkovColumn => "ml_markov_coluna".to_string(),
            Self::MarkovTerminal => "ml_puxada_terminal_top2".to_string(),
            Self::TerminalForte { w } => format!("ml_final_forte_w{w}"),
        }
    }

    fn explain(&self) -> &'static str {
        match self {
            Self::Matcher(_) => "Estratégia do MONSTER. Aposta só quando o padrão dispara; mede se acerta mais que a fração de casas que cobre.",
            Self::HotTerminal { .. } => "Segue o terminal que mais saiu recentemente. Se as 'ondas' fossem reais, acertaria mais que o acaso.",
            Self::FrequencyBias => "Aprende no treino quais 5 números mais saíram e aposta neles depois. Só ganha se a roda tiver viés físico.",
            Self::MarkovColumn => "Aprende no treino qual coluna costuma vir depois de cada coluna. Só ganha se um giro depender do anterior.",
            Self::MarkovTerminal => "Puxada medida: aprende no treino quais 2 finais mais saem depois de cada final e aposta neles. É a versão testável de 'número que puxa número'.",
            Self::TerminalForte { .. } => "Aposta no final que mais está saindo acima do esperado (maior z) na janela longa. Só ganha se a mesa tiver final realmente viciado.",
        }
    }
}

/// Modelos aprendidos no treino (vazios para hipóteses que não treinam).
#[derive(Default)]
struct Trained {
    top5: Vec<u8>,
    markov: [[u32; 4]; 4],
    markov_t: [[u32; 10]; 10],
}

fn column(n: u8) -> usize {
    if n == 0 { 0 } else { usize::from((n - 1) % 3 + 1) }
}

fn train(train: &[u8]) -> Trained {
    let mut freq = [0u32; 37];
    for &n in train {
        freq[usize::from(n)] += 1;
    }
    let mut idx: Vec<u8> = (0..=36).collect();
    idx.sort_by_key(|&n| (std::cmp::Reverse(freq[usize::from(n)]), n));
    let mut markov = [[0u32; 4]; 4];
    for w in train.windows(2) {
        markov[column(w[0])][column(w[1])] += 1;
    }
    let mut markov_t = [[0u32; 10]; 10];
    for w in train.windows(2) {
        markov_t[usize::from(w[0] % 10)][usize::from(w[1] % 10)] += 1;
    }
    Trained { top5: idx[..5].to_vec(), markov, markov_t }
}

fn covered(covers: &[Cover]) -> Vec<u8> {
    let mut v: Vec<u8> = (0..=36u8).filter(|&n| covers.iter().any(|c| c.matches_number(n))).collect();
    v.dedup();
    v
}

fn bet(h: &Hypothesis, past: &[u8], t: &Trained) -> Option<Vec<u8>> {
    match h {
        Hypothesis::Matcher(m) => {
            let window: Vec<u8> = past.iter().rev().take(CONTEXT).copied().collect();
            let intent = m.evaluate(&window)?;
            if intent.confidence < 0.55 {
                return None;
            }
            let nums = covered(&intent.cover);
            (!nums.is_empty() && nums.len() < 37).then_some(nums)
        }
        Hypothesis::HotTerminal { w, k } => {
            let recent = &past[past.len().saturating_sub(*w)..];
            let mut cnt = [0u32; 10];
            for &n in recent {
                cnt[usize::from(n % 10)] += 1;
            }
            let mut ts: Vec<u8> = (0..10).collect();
            ts.sort_by_key(|&d| (std::cmp::Reverse(cnt[usize::from(d)]), d));
            let top = &ts[..*k];
            Some((0..=36u8).filter(|n| top.contains(&(n % 10))).collect())
        }
        Hypothesis::FrequencyBias => Some(t.top5.clone()),
        Hypothesis::MarkovColumn => {
            let prev = column(*past.last()?);
            if prev == 0 {
                return None;
            }
            let row = t.markov[prev];
            if row[1..].iter().sum::<u32>() < 30 {
                return None; // pouco treino para essa coluna: não aposta
            }
            let best = (1..=3).max_by_key(|&c| (row[c], std::cmp::Reverse(c)))?;
            Some((1..=36u8).filter(|&n| column(n) == best).collect())
        }
        Hypothesis::MarkovTerminal => {
            let a = usize::from(*past.last()? % 10);
            let row = t.markov_t[a];
            if row.iter().sum::<u32>() < 20 {
                return None;
            }
            // Ranqueia pelo excesso sobre o esperado (count / tamanho do final), não pela contagem crua.
            let mut ts: Vec<u8> = (0..10).collect();
            ts.sort_by(|&x, &y| {
                let fx = f64::from(row[usize::from(x)]) / f64::from(TERMINAL_SIZE[usize::from(x)]);
                let fy = f64::from(row[usize::from(y)]) / f64::from(TERMINAL_SIZE[usize::from(y)]);
                fy.partial_cmp(&fx).unwrap_or(std::cmp::Ordering::Equal).then(x.cmp(&y))
            });
            Some(terminal_numbers(&ts[..2]))
        }
        Hypothesis::TerminalForte { w } => {
            if past.len() < *w {
                return None;
            }
            let recent = &past[past.len() - *w..];
            let n = recent.len() as f64;
            let best = (0..10u8)
                .map(|d| {
                    let k = recent.iter().filter(|&&x| x % 10 == d).count() as f64;
                    let p = f64::from(TERMINAL_SIZE[usize::from(d)]) / 37.0;
                    ((k - n * p) / (n * p * (1.0 - p)).sqrt(), d)
                })
                .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))?;
            (best.0 >= 2.0).then(|| terminal_numbers(&[best.1]))
        }
    }
}

/// Resultado de uma hipótese numa fatia (treino, validação ou teste).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SplitStats {
    /// Apostas feitas (giros em que a hipótese disparou).
    pub bets: usize,
    /// Apostas certas.
    pub hits: usize,
    /// Taxa de acerto observada (%).
    pub hit_pct: f64,
    /// Taxa esperada pelo acaso, dada a quantidade de casas cobertas (%).
    pub chance_pct: f64,
    /// Desvio em sigmas contra o acaso (positivo = acerta mais que o acaso).
    pub z: f64,
    /// Retorno médio por unidade apostada (%): roleta honesta dá −2,70%.
    pub ev_pct: f64,
}

#[derive(Default)]
struct Acc {
    bets: usize,
    hits: usize,
    exp: f64,
    var: f64,
    ret: f64,
}

impl Acc {
    fn add(&mut self, nums: &[u8], next: u8) {
        let k = nums.len() as f64;
        let p0 = k / 37.0;
        let hit = nums.contains(&next);
        self.bets += 1;
        self.hits += usize::from(hit);
        self.exp += p0;
        self.var += p0 * (1.0 - p0);
        // Todo pagamento da roleta devolve 36/k por unidade quando acerta (pleno 36x, coluna 3x...).
        self.ret += if hit { 36.0 / k } else { 0.0 };
    }

    fn stats(&self) -> SplitStats {
        if self.bets == 0 {
            return SplitStats::default();
        }
        let n = self.bets as f64;
        SplitStats {
            bets: self.bets,
            hits: self.hits,
            hit_pct: 100.0 * self.hits as f64 / n,
            chance_pct: 100.0 * self.exp / n,
            z: if self.var > 0.0 { (self.hits as f64 - self.exp) / self.var.sqrt() } else { 0.0 },
            ev_pct: 100.0 * (self.ret / n - 1.0),
        }
    }
}

/// Uma linha do laboratório.
#[derive(Debug, Clone, Serialize)]
pub struct LabRow {
    /// Identificador da hipótese.
    pub id: String,
    /// O que a hipótese testa, em português simples.
    pub explicacao: String,
    /// Treino (60%): onde os modelos aprendem.
    pub treino: SplitStats,
    /// Validação (20%): onde você ajusta.
    pub validacao: SplitStats,
    /// Teste oculto (20%): só ele decide o veredito.
    pub teste: SplitStats,
    /// p-valor unilateral no teste (chance de o resultado ser sorte).
    pub p_teste: f64,
    /// `APROVADO`, `REPROVADO` ou `DADOS INSUFICIENTES`.
    pub veredito: String,
    /// Por que recebeu o veredito.
    pub motivo: String,
}

/// Relatório completo do laboratório.
#[derive(Debug, Clone, Serialize)]
pub struct LabReport {
    /// Giros gravados no banco.
    pub giros_brutos: usize,
    /// Giros reais após remover rajadas de fita regravada.
    pub giros_limpos: usize,
    /// Qui-quadrado das 37 casas nos giros limpos (df = 36; acima de 51 = desvio a 5%).
    pub qui2_casas: f64,
    /// Giros recomendados para detectar viés de ~1% numa casa.
    pub giros_recomendados: usize,
    /// Hipóteses testadas com veredito.
    pub linhas: Vec<LabRow>,
    /// Giros reais limpos (cronológico, até 5000) para a aba Física & Estatística.
    pub serie: Vec<u8>,
}

/// P(Z ≥ z) da normal padrão (aproximação de Abramowitz–Stegun 7.1.26, erro < 1,5e-7).
pub fn p_one_sided(z: f64) -> f64 {
    let x = z / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    let erf = if x >= 0.0 { y } else { -y };
    0.5 * (1.0 - erf)
}

/// Roda todas as hipóteses sobre os giros limpos e aplica Holm no teste oculto.
pub fn run_lab(giros_brutos: usize, spins: &[u8], hyps: &[Hypothesis]) -> LabReport {
    let n = spins.len();
    let cut_train = n * 60 / 100;
    let cut_val = n * 80 / 100;
    let trained = train(&spins[..cut_train]);

    let mut rows: Vec<LabRow> = hyps
        .iter()
        .map(|h| {
            let (mut tr, mut va, mut te) = (Acc::default(), Acc::default(), Acc::default());
            for i in CONTEXT..n {
                let Some(nums) = bet(h, &spins[..i], &trained) else { continue };
                let acc = if i < cut_train { &mut tr } else if i < cut_val { &mut va } else { &mut te };
                acc.add(&nums, spins[i]);
            }
            let teste = te.stats();
            LabRow {
                id: h.id(),
                explicacao: h.explain().to_string(),
                treino: tr.stats(),
                validacao: va.stats(),
                p_teste: p_one_sided(teste.z),
                teste,
                veredito: String::new(),
                motivo: String::new(),
            }
        })
        .collect();

    // Holm: ordena os p-valores das hipóteses com dados suficientes e exige p ≤ α/(m−i).
    let mut order: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].teste.bets >= MIN_TEST_BETS).collect();
    order.sort_by(|&a, &b| rows[a].p_teste.partial_cmp(&rows[b].p_teste).unwrap_or(std::cmp::Ordering::Equal));
    let m = order.len();
    let mut still = true;
    for (rank, &i) in order.iter().enumerate() {
        let limit = ALPHA / (m - rank) as f64;
        let sig = still && rows[i].p_teste <= limit;
        still = sig;
        let r = &mut rows[i];
        if sig && r.teste.ev_pct > 0.0 && r.validacao.z > 0.0 {
            r.veredito = "APROVADO".into();
            r.motivo = format!(
                "Acertou {:.1}% contra {:.1}% do acaso no teste oculto (p={:.4}, limite Holm {:.4}) e o EV ficou positivo.",
                r.teste.hit_pct, r.teste.chance_pct, r.p_teste, limit
            );
        } else {
            r.veredito = "REPROVADO".into();
            r.motivo = if !sig {
                format!(
                    "No teste oculto acertou {:.1}% contra {:.1}% do acaso (p={:.3}). Diferença compatível com sorte (limite Holm {:.4}).",
                    r.teste.hit_pct, r.teste.chance_pct, r.p_teste, limit
                )
            } else {
                "Significativo, mas o EV no teste não ficou positivo ou a validação discordou.".into()
            };
        }
    }
    for r in rows.iter_mut().filter(|r| r.veredito.is_empty()) {
        r.veredito = "DADOS INSUFICIENTES".into();
        r.motivo = format!(
            "Só {} apostas no teste oculto; o mínimo para julgar é {}. Deixe o MONSTER coletando mais giros.",
            r.teste.bets, MIN_TEST_BETS
        );
    }

    let mut freq = [0f64; 37];
    for &s in spins {
        freq[usize::from(s)] += 1.0;
    }
    let e = n as f64 / 37.0;
    let qui2 = if n > 0 { freq.iter().map(|o| (o - e).powi(2) / e).sum() } else { 0.0 };

    LabReport {
        giros_brutos,
        giros_limpos: n,
        qui2_casas: qui2,
        giros_recomendados: 10_000,
        linhas: rows,
        serie: spins[spins.len().saturating_sub(5_000)..].to_vec(),
    }
}

/// Conjunto padrão: todos os matchers ligados + os modelos de ML do laboratório.
pub fn default_hypotheses(matchers: &[Arc<dyn Matcher>]) -> Vec<Hypothesis> {
    let mut v: Vec<Hypothesis> = matchers.iter().cloned().map(Hypothesis::Matcher).collect();
    v.push(Hypothesis::HotTerminal { w: 20, k: 1 });
    v.push(Hypothesis::HotTerminal { w: 20, k: 2 });
    v.push(Hypothesis::FrequencyBias);
    v.push(Hypothesis::MarkovColumn);
    v.push(Hypothesis::MarkovTerminal);
    v.push(Hypothesis::TerminalForte { w: 200 });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> u8 {
        *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((*seed >> 33) % 37) as u8
    }

    #[test]
    fn rajada_de_fita_regravada_e_removida() {
        let rows = [(5, 0), (9, 1_000), (12, 2_000), (7, 45_000), (7, 46_000), (30, 90_000)];
        assert_eq!(dedupe(&rows), vec![5, 7, 30]);
    }

    #[test]
    fn roleta_justa_nao_aprova_nenhum_modelo() {
        let mut s = 42u64;
        let spins: Vec<u8> = (0..5_000).map(|_| lcg(&mut s)).collect();
        let hyps = vec![
            Hypothesis::HotTerminal { w: 20, k: 1 },
            Hypothesis::HotTerminal { w: 20, k: 2 },
            Hypothesis::FrequencyBias,
            Hypothesis::MarkovColumn,
        ];
        let r = run_lab(5_000, &spins, &hyps);
        assert!(r.linhas.iter().all(|l| l.veredito != "APROVADO"), "acaso puro não pode ser aprovado");
        // O EV de 1000 apostas é ruidoso (top5 paga 7,2x: desvio ~7,8%), por isso o veredito
        // olha o teste de hipótese e não o EV sozinho. Aqui só exige que não seja absurdo.
        for l in &r.linhas {
            assert!(l.teste.ev_pct < 30.0, "{} ev {}", l.id, l.teste.ev_pct);
        }
    }

    #[test]
    fn roda_viciada_e_detectada_pelo_vies_de_frequencia() {
        let mut s = 7u64;
        // O número 17 sai 4x mais que o normal: viés físico forte.
        let spins: Vec<u8> = (0..6_000)
            .map(|i| if i % 12 == 0 { 17 } else { lcg(&mut s) })
            .collect();
        let r = run_lab(6_000, &spins, &[Hypothesis::FrequencyBias]);
        assert_eq!(r.linhas[0].veredito, "APROVADO", "{:?}", r.linhas[0].motivo);
    }

    #[test]
    fn puxada_real_e_detectada_e_puxada_inventada_nao() {
        // Mesa onde o final 7 é seguido pelo final 4 metade das vezes: puxada verdadeira.
        let mut s = 11u64;
        let mut spins: Vec<u8> = Vec::new();
        for _ in 0..6_000 {
            let prev = spins.last().copied();
            let n = if prev.is_some_and(|p| p % 10 == 7) && lcg(&mut s) % 2 == 0 { 14 } else { lcg(&mut s) };
            spins.push(n);
        }
        let r = run_lab(6_000, &spins, &[Hypothesis::MarkovTerminal]);
        assert_eq!(r.linhas[0].veredito, "APROVADO", "{}", r.linhas[0].motivo);
        // Roleta justa: nenhuma puxada aprovada.
        let mut s2 = 99u64;
        let fair: Vec<u8> = (0..6_000).map(|_| lcg(&mut s2)).collect();
        let r2 = run_lab(6_000, &fair, &[Hypothesis::MarkovTerminal, Hypothesis::TerminalForte { w: 200 }]);
        assert!(r2.linhas.iter().all(|l| l.veredito != "APROVADO"));
    }

    #[test]
    fn p_valor_normal_bate_com_tabela() {
        assert!((p_one_sided(0.0) - 0.5).abs() < 1e-6);
        assert!((p_one_sided(1.645) - 0.05).abs() < 1e-3);
        assert!((p_one_sided(2.326) - 0.01).abs() < 1e-3);
    }

    #[test]
    fn poucos_dados_nao_dao_veredito() {
        let spins: Vec<u8> = (0..60).map(|i| (i * 7 % 37) as u8).collect();
        let r = run_lab(60, &spins, &[Hypothesis::FrequencyBias]);
        assert_eq!(r.linhas[0].veredito, "DADOS INSUFICIENTES");
    }
}
