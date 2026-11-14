// SPDX-License-Identifier: Apache-2.0
//! Estatística da mesa no mesmo formato do painel "Statistics" da Playtech (100/200/500/1000).
//! Porcentagens como a mesa: cor e dúzia/coluna sobre todos os giros; par/ímpar e baixo/alto
//! sem o 0. Inclui o calor de cada script do operador (acerto observado vs esperado).

use monster_store::{SqlitePool, StoreError};
use monster_strategy_native::OPERATOR_SCRIPTS;
use serde::Serialize;

/// Janelas oferecidas pela mesa.
pub const STATS_WINDOWS: [usize; 4] = [100, 200, 500, 1000];
/// Intervalo mínimo entre giros reais gravados pelo observer (abaixo disso é regravação).
const MIN_REAL_GAP_MS: i64 = 20_000;

const RED: [u8; 18] = [1, 3, 5, 7, 9, 12, 14, 16, 18, 19, 21, 23, 25, 27, 30, 32, 34, 36];
/// Setores da roda europeia, como no painel da mesa.
pub const VOISINS: [u8; 17] = [22, 18, 29, 7, 28, 12, 35, 3, 26, 0, 32, 15, 19, 4, 21, 2, 25];
pub const TIERS: [u8; 12] = [27, 13, 36, 11, 30, 8, 23, 10, 5, 24, 16, 33];
pub const ORPHELINS: [u8; 8] = [1, 20, 14, 31, 9, 17, 34, 6];
pub const JEU_ZERO: [u8; 7] = [12, 35, 3, 26, 0, 32, 15];

/// Calor de um script: quanto a cobertura dele acertou na janela.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScriptHeat {
    pub id: String,
    pub pockets: usize,
    pub hits: u32,
    pub pct: f64,
    pub expected_pct: f64,
    /// `observado / esperado` (1.0 = neutro).
    pub ratio: f64,
}

/// Estatística da janela pedida.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TableStats {
    pub window: usize,
    /// Giros reais contados (pode ser menor que `window` se o banco tiver menos).
    pub spins: usize,
    pub counts: Vec<u32>,
    pub hot: Vec<u8>,
    pub cold: Vec<u8>,
    pub red_pct: f64,
    pub black_pct: f64,
    pub zero_pct: f64,
    pub even_pct: f64,
    pub odd_pct: f64,
    pub low_pct: f64,
    pub high_pct: f64,
    pub dozens_pct: [f64; 3],
    pub columns_pct: [f64; 3],
    pub tier_pct: f64,
    pub orphelins_pct: f64,
    pub voisins_pct: f64,
    pub jeu_zero_pct: f64,
    /// Scripts do mais quente para o mais frio.
    pub scripts: Vec<ScriptHeat>,
}

fn pct(part: u32, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        f64::from(part) * 100.0 / total as f64
    }
}

/// Calcula a estatística sobre `newest_first` (`[0]` = último giro), limitado a `window`.
pub fn compute(newest_first: &[u8], window: usize) -> TableStats {
    let spins: Vec<u8> = newest_first.iter().copied().filter(|&n| n <= 36).take(window).collect();
    let n = spins.len();
    let mut counts = vec![0u32; 37];
    for &s in &spins {
        counts[usize::from(s)] += 1;
    }
    let sum = |set: &[u8]| set.iter().map(|&x| counts[usize::from(x)]).sum::<u32>();
    let count_if = |f: &dyn Fn(u8) -> bool| spins.iter().filter(|&&s| f(s)).count() as u32;

    // Quentes: mais frequentes; empate -> o que saiu mais recentemente.
    // Frios: menos frequentes; empate -> o que está há mais tempo sem sair.
    let last_seen = |x: u8| spins.iter().position(|&s| s == x).unwrap_or(usize::MAX);
    let mut order: Vec<u8> = (0..=36).collect();
    order.sort_by(|&a, &b| {
        counts[usize::from(b)]
            .cmp(&counts[usize::from(a)])
            .then(last_seen(a).cmp(&last_seen(b)))
    });
    let hot = order.iter().take(4).copied().collect();
    let mut cold_order: Vec<u8> = (0..=36).collect();
    cold_order.sort_by(|&a, &b| {
        counts[usize::from(a)]
            .cmp(&counts[usize::from(b)])
            .then(last_seen(b).cmp(&last_seen(a)))
    });
    let cold = cold_order.iter().take(4).copied().collect();

    let red = count_if(&|s| RED.contains(&s));
    let zero = counts[0];
    let mut scripts: Vec<ScriptHeat> = OPERATOR_SCRIPTS
        .iter()
        .map(|sc| {
            let hits = sum(sc.pockets);
            let p = pct(hits, n);
            let e = sc.pockets.len() as f64 * 100.0 / 37.0;
            ScriptHeat {
                id: sc.id.to_string(),
                pockets: sc.pockets.len(),
                hits,
                pct: p,
                expected_pct: e,
                ratio: if n == 0 { 0.0 } else { p / e },
            }
        })
        .collect();
    scripts.sort_by(|a, b| b.ratio.partial_cmp(&a.ratio).unwrap_or(std::cmp::Ordering::Equal));

    TableStats {
        window,
        spins: n,
        hot,
        cold,
        red_pct: pct(red, n),
        black_pct: pct(n as u32 - red - zero, n),
        zero_pct: pct(zero, n),
        even_pct: pct(count_if(&|s| s != 0 && s % 2 == 0), n),
        odd_pct: pct(count_if(&|s| s % 2 == 1), n),
        low_pct: pct(count_if(&|s| (1..=18).contains(&s)), n),
        high_pct: pct(count_if(&|s| s >= 19), n),
        dozens_pct: [1u8, 2, 3].map(|d| pct(count_if(&|s| s != 0 && (s - 1) / 12 + 1 == d), n)),
        columns_pct: [1u8, 2, 3].map(|c| pct(count_if(&|s| s != 0 && (s - 1) % 3 + 1 == c), n)),
        tier_pct: pct(sum(&TIERS), n),
        orphelins_pct: pct(sum(&ORPHELINS), n),
        voisins_pct: pct(sum(&VOISINS), n),
        jeu_zero_pct: pct(sum(&JEU_ZERO), n),
        scripts,
        counts,
    }
}

/// Giros reais da mesa, mais recente primeiro. Mantém a base de sessão (`base-`) e os giros
/// recuperados da fita (`gap-`); descarta regravações do observer a menos de 20 s.
pub fn load_table_spins(pool: &SqlitePool, limit: usize) -> Result<Vec<u8>, StoreError> {
    let rows: Vec<(String, i64, i64)> = pool
        .with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT spin_id, number, ts FROM spins ORDER BY ts DESC, id DESC LIMIT {}",
                (limit * 2).min(20_000)
            ))?;
            let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            it.collect::<Result<Vec<_>, _>>()
        })
        .map_err(StoreError::Database)?;
    Ok(clean_newest_first(&rows, limit))
}

/// Limpeza pura (testável): ver [`load_table_spins`].
pub fn clean_newest_first(rows: &[(String, i64, i64)], limit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(limit);
    let mut last_kept: Option<i64> = None;
    for (id, n, ts) in rows {
        if !(0..=36).contains(n) {
            continue;
        }
        let recovered = id.starts_with("base-") || id.starts_with("gap-");
        if recovered || last_kept.is_none_or(|l| l - ts >= MIN_REAL_GAP_MS) {
            out.push(*n as u8);
            if !recovered {
                last_kept = Some(*ts);
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcentagens_no_formato_da_mesa() {
        // 10 giros: 0, três vermelhos baixos ímpares, seis pretos altos pares.
        let spins = [0u8, 1, 3, 5, 20, 22, 24, 26, 28, 20];
        let s = compute(&spins, 100);
        assert_eq!(s.spins, 10);
        assert!((s.zero_pct - 10.0).abs() < 1e-9);
        assert!((s.red_pct + s.black_pct + s.zero_pct - 100.0).abs() < 1e-9);
        assert!((s.odd_pct - 30.0).abs() < 1e-9, "1,3,5");
        assert!((s.even_pct - 60.0).abs() < 1e-9, "0 não conta como par");
        assert!((s.low_pct + s.high_pct - 90.0).abs() < 1e-9, "0 fora de baixo/alto");
        assert!((s.dozens_pct.iter().sum::<f64>() - 90.0).abs() < 1e-9, "0 fora das dúzias");
        assert_eq!(s.hot[0], 20, "20 saiu 2 vezes");
    }

    #[test]
    fn setores_somam_100_e_zero_e_parte_do_voisins() {
        let spins: Vec<u8> = (0..=36).collect();
        let s = compute(&spins, 100);
        assert!((s.tier_pct + s.orphelins_pct + s.voisins_pct - 100.0).abs() < 1e-9);
        assert!(JEU_ZERO.iter().all(|n| VOISINS.contains(n)));
        // roda uniforme: todo script com razão 1
        assert!(s.scripts.iter().all(|x| (x.ratio - 1.0).abs() < 1e-9));
    }

    #[test]
    fn janela_corta_nos_mais_recentes() {
        let spins = [7u8; 150];
        assert_eq!(compute(&spins, 100).spins, 100);
        assert_eq!(compute(&spins, 200).spins, 150);
    }

    #[test]
    fn script_mais_quente_vem_primeiro() {
        // só números dos espelhos saindo
        let spins = [11u8, 22, 33, 12, 21, 13, 31, 23, 32, 20];
        let s = compute(&spins, 100);
        assert_eq!(s.scripts[0].id, "script_espelhos_gemeos");
        assert!(s.scripts[0].ratio > 1.5);
    }

    #[test]
    fn limpeza_descarta_regravacao_e_mantem_base_e_gap() {
        let rows = vec![
            ("rid-3".to_string(), 5, 100_000),
            ("rid-3b".to_string(), 5, 95_000), // regravação 5 s depois: fora
            ("gap-x".to_string(), 12, 80_000),  // recuperado da fita: fica
            ("rid-2".to_string(), 9, 60_000),
            ("base-1".to_string(), 30, 59_999), // base de sessão: fica
            ("base-2".to_string(), 31, 59_998),
        ];
        assert_eq!(clean_newest_first(&rows, 10), vec![5, 12, 9, 30, 31]);
    }
}
