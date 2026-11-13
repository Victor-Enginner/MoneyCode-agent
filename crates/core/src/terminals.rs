// SPDX-License-Identifier: Apache-2.0
//! Leitura estatística dos terminais da timeline (`2,3,6,9`) para a FUI.
//! Filtra blocos de replay de fixture do banco e devolve a distribuição real dos finais.
//! Leitura pura: nada aqui decide aposta, altera escada ou toca no executor.

use monster_ui::{TerminalHitDto, TerminalRadarDto};

/// Finais que o operador acompanha — a mesma lista do matcher `terminal_quente`.
pub const OPERATOR_TERMINALS: [u8; 4] = [2, 3, 6, 9];

/// Probabilidade esperada de um giro cair em `2, 3, 6` ou `9` (`15` pockets de `37`).
const EXPECTED_SHARE: f64 = 15.0 / 37.0;

/// Tamanho do grama usado para detectar replay: a mesa ao vivo não repete 6 números
/// iguais em sequência (a chance é `1/37^6`), um fixture em ciclo repete o tempo todo.
const REPLAY_GRAM: usize = 6;

/// Codifica um grama de números em base 37 — biunívoco, então não há colisão de hash
/// capaz de cortar giro real por engano.
fn gram_key(gram: &[u8]) -> u64 {
    gram.iter()
        .fold(0u64, |acc, &n| acc * 37 + u64::from(n.min(36)))
}

/// Índice a partir do qual a fita deixa de ser replay de fixture.
///
/// Varre a fita cronológica e, sempre que um grama de 6 números reaparece, corta a cauda
/// depois da primeira ocorrência. A cauda resultante é a única parte que vale para
/// estatística: o resto é sequência gravada em teste, repetida em ciclo.
pub fn real_tail_start(chronological: &[u8]) -> usize {
    if chronological.len() < REPLAY_GRAM {
        return 0;
    }
    let mut seen: std::collections::HashMap<u64, usize> =
        std::collections::HashMap::with_capacity(chronological.len());
    let mut cut = 0usize;
    for i in 0..=chronological.len() - REPLAY_GRAM {
        let key = gram_key(&chronological[i..i + REPLAY_GRAM]);
        if let Some(&previous) = seen.get(&key) {
            // Corta depois da ÚLTIMA ocorrência do grama repetido: o bloco de fixture
            // inteiro fica fora, não só a primeira volta dele.
            cut = cut.max(previous + REPLAY_GRAM).max(i + REPLAY_GRAM);
        }
        seen.insert(key, i);
    }
    cut
}

/// Monta o placar dos terminais sobre os últimos `window` giros **reais** da fita.
///
/// `min_streak`/`min_share` são os mesmos parâmetros do matcher `terminal_quente`, para o
/// painel mostrar exatamente quando o gatilho está armado (`streak = min_streak - 1`) e
/// quando ele entraria (`streak >= min_streak` e share acima do mínimo).
pub fn terminal_radar(
    chronological: &[u8],
    window: usize,
    terminals: [u8; 4],
    min_streak: usize,
    min_share: f64,
) -> TerminalRadarDto {
    let start = real_tail_start(chronological);
    let tail = &chronological[start..];
    let slice = if tail.len() > window {
        &tail[tail.len() - window..]
    } else {
        tail
    };

    let mut counts = [0usize; 10];
    let mut hits = 0usize;
    for &n in slice {
        if n > 36 || !terminals.contains(&(n % 10)) {
            continue;
        }
        hits += 1;
        counts[(n % 10) as usize] += 1;
    }

    let spins = slice.len();
    let share = if spins > 0 {
        hits as f64 / spins as f64
    } else {
        0.0
    };
    let sigma = if spins > 0 {
        (share - EXPECTED_SHARE) / (EXPECTED_SHARE * (1.0 - EXPECTED_SHARE) / spins as f64).sqrt()
    } else {
        0.0
    };

    // Série atual da família: `0` não tem final e não quebra a contagem.
    let streak = tail
        .iter()
        .rev()
        .take_while(|&&n| n % 10 == 0 || terminals.contains(&(n % 10)))
        .count();

    let mut top: Vec<TerminalHitDto> = terminals
        .iter()
        .map(|&t| TerminalHitDto {
            terminal: t,
            hits: counts[usize::from(t)],
        })
        .collect();
    top.sort_by_key(|h| (std::cmp::Reverse(h.hits), h.terminal));

    TerminalRadarDto {
        window,
        spins,
        hits,
        share_pct: share * 100.0,
        expected_pct: EXPECTED_SHARE * 100.0,
        sigma,
        streak,
        armed: spins >= min_streak && streak + 1 == min_streak,
        firing: spins >= min_streak && streak >= min_streak && share >= min_share,
        replay_dropped: start,
        top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_de_fixture_e_cortado_da_estatistica() {
        // Bloco em ciclo (replay) seguido de cauda real.
        let ciclo = [12u8, 29, 13, 19, 9, 26, 14, 25, 23, 31, 8];
        let mut fita: Vec<u8> = ciclo.iter().copied().cycle().take(44).collect();
        let inicio_real = fita.len();
        fita.extend_from_slice(&[3, 6, 9, 2, 33, 16, 4, 7, 22, 1]);
        let cut = real_tail_start(&fita);
        assert_eq!(cut, inicio_real, "replay precisa sair da conta");
        let radar = terminal_radar(&fita, 100, OPERATOR_TERMINALS, 3, 0.44);
        assert_eq!(radar.spins, 10);
        assert_eq!(radar.replay_dropped, inicio_real);
    }

    #[test]
    fn cauda_real_sem_repeticao_nao_e_cortada() {
        let fita = [3u8, 6, 9, 2, 33, 16, 4, 7, 22, 1, 5, 8, 11, 20];
        assert_eq!(real_tail_start(&fita), 0);
        let radar = terminal_radar(&fita, 100, OPERATOR_TERMINALS, 3, 0.44);
        assert_eq!(radar.spins, 14);
        assert_eq!(radar.replay_dropped, 0);
    }

    #[test]
    fn placar_conta_finais_serie_e_armacao_do_gatilho() {
        // 8 giros: 2,3,6,9,5,3,6,9 — o 5 corta a série, o resto é família.
        let fita = [2u8, 3, 6, 9, 5, 3, 6, 9];
        let radar = terminal_radar(&fita, 100, OPERATOR_TERMINALS, 3, 0.44);
        assert_eq!(radar.spins, 8);
        assert_eq!(radar.hits, 7);
        assert!((radar.share_pct - 87.5).abs() < 1e-9);
        assert!((radar.expected_pct - EXPECTED_SHARE * 100.0).abs() < 1e-9);
        assert!(radar.sigma > 1.0, "7/8 contra 40,5% é desvio grande");
        assert_eq!(radar.streak, 3);
        assert!(radar.firing, "serie 3 com share alto: o gatilho entraria");
        assert!(!radar.armed);
        assert_eq!(radar.top[0].terminal, 3);
        assert_eq!(radar.top[0].hits, 2);

        // Duas seguidas: pré-alerta armado, ainda sem entrada.
        // Cronológico termina no mais novo: 5, 3, 6 -> série de 2 (3 e 6).
        let pre = terminal_radar(&[5u8, 3, 6], 100, OPERATOR_TERMINALS, 3, 0.44);
        assert_eq!(pre.streak, 2);
        assert!(pre.armed);
        assert!(!pre.firing);
    }

    #[test]
    fn fita_vazia_nao_inventa_numero() {
        let radar = terminal_radar(&[], 100, OPERATOR_TERMINALS, 3, 0.44);
        assert_eq!(radar.spins, 0);
        assert_eq!(radar.hits, 0);
        assert_eq!(radar.sigma, 0.0);
        assert_eq!(radar.streak, 0);
        assert!(radar.top.iter().all(|h| h.hits == 0));
    }
}
