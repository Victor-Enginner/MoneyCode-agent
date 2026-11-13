// SPDX-License-Identifier: Apache-2.0
//! Leitura estatística dos terminais da timeline (`2,3,6,9`) para a FUI.
//! Filtra blocos de replay de fixture do banco e devolve a distribuição real dos finais.
//! Leitura pura: nada aqui decide aposta, altera escada ou toca no executor.

use monster_ui::{FamilyRadarDto, FamilyShareDto, TerminalHitDto, TerminalRadarDto};

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

/// Probabilidade esperada de uma coluna ou dúzia (`12` pockets de `37`).
const EXPECTED_DOZEN: f64 = 12.0 / 37.0;

/// Probabilidade esperada de uma cor (`18` pockets de `37`).
const EXPECTED_COLOR: f64 = 18.0 / 37.0;

fn share_of(hits: usize, spins: usize, expected: f64) -> (f64, f64) {
    if spins == 0 {
        return (0.0, 0.0);
    }
    let share = hits as f64 / spins as f64;
    let sigma = (share - expected) / (expected * (1.0 - expected) / spins as f64).sqrt();
    (share, sigma)
}

/// Placar das famílias de fora (`COLUNAS`, `DÚZIAS`, `CORES`) sobre a mesma janela real.
///
/// Serve para a pergunta simétrica à dos terminais: "essa coluna está batendo mesmo?".
/// `0` fica fora de todas as contas, como no pano. Leitura pura, sem decisão de aposta.
pub fn family_radars(chronological: &[u8], window: usize) -> Vec<FamilyRadarDto> {
    let start = real_tail_start(chronological);
    let tail = &chronological[start..];
    let slice = if tail.len() > window {
        &tail[tail.len() - window..]
    } else {
        tail
    };

    let mut columns = [0usize; 3];
    let mut dozens = [0usize; 3];
    let mut colors = [0usize; 2];
    let mut spins = 0usize;
    for &n in slice {
        if n == 0 || n > 36 {
            continue;
        }
        spins += 1;
        columns[usize::from(monster_strategy_native::column_of(n)) - 1] += 1;
        dozens[usize::from(monster_strategy_native::dozen_of(n)) - 1] += 1;
        colors[usize::from(!monster_strategy_native::is_red_number(n))] += 1;
    }

    let column_opts = (0..3)
        .map(|i| {
            let (share, sigma) = share_of(columns[i], spins, EXPECTED_DOZEN);
            FamilyShareDto {
                label: format!("C{}", i + 1),
                hits: columns[i],
                share_pct: share * 100.0,
                sigma,
            }
        })
        .collect::<Vec<_>>();
    let dozen_opts = (0..3)
        .map(|i| {
            let (share, sigma) = share_of(dozens[i], spins, EXPECTED_DOZEN);
            FamilyShareDto {
                label: format!("D{}", i + 1),
                hits: dozens[i],
                share_pct: share * 100.0,
                sigma,
            }
        })
        .collect::<Vec<_>>();
    let color_opts = ["VERMELHO", "PRETO"]
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let (share, sigma) = share_of(colors[i], spins, EXPECTED_COLOR);
            FamilyShareDto {
                label: (*label).to_string(),
                hits: colors[i],
                share_pct: share * 100.0,
                sigma,
            }
        })
        .collect::<Vec<_>>();

    // Série atual: quantos giros seguidos repetem a última coluna/dúzia/cor.
    let latest = tail.iter().rev().copied().find(|&n| n <= 36).unwrap_or(0);
    let streak_key = |n: u8| -> Option<u8> {
        if n == 0 || n > 36 {
            None
        } else {
            Some(n)
        }
    };
    let mut column_streak = 0usize;
    let mut dozen_streak = 0usize;
    let mut color_streak = 0usize;
    if let Some(last) = streak_key(latest) {
        column_streak = tail
            .iter()
            .rev()
            .take_while(|&&n| n != 0 && monster_strategy_native::column_of(n) == monster_strategy_native::column_of(last))
            .count();
        dozen_streak = tail
            .iter()
            .rev()
            .take_while(|&&n| n != 0 && monster_strategy_native::dozen_of(n) == monster_strategy_native::dozen_of(last))
            .count();
        color_streak = tail
            .iter()
            .rev()
            .take_while(|&&n| {
                n != 0
                    && monster_strategy_native::is_red_number(n)
                        == monster_strategy_native::is_red_number(last)
            })
            .count();
    }

    vec![
        FamilyRadarDto {
            category: "COLUNAS".to_string(),
            spins,
            streak_label: if column_streak > 0 {
                format!("C{}", monster_strategy_native::column_of(latest))
            } else {
                "—".to_string()
            },
            streak: column_streak,
            expected_pct: EXPECTED_DOZEN * 100.0,
            options: column_opts,
        },
        FamilyRadarDto {
            category: "DÚZIAS".to_string(),
            spins,
            streak_label: if dozen_streak > 0 {
                format!("D{}", monster_strategy_native::dozen_of(latest))
            } else {
                "—".to_string()
            },
            streak: dozen_streak,
            expected_pct: EXPECTED_DOZEN * 100.0,
            options: dozen_opts,
        },
        FamilyRadarDto {
            category: "CORES".to_string(),
            spins,
            streak_label: if color_streak > 0 {
                if monster_strategy_native::is_red_number(latest) {
                    "VERMELHO".to_string()
                } else {
                    "PRETO".to_string()
                }
            } else {
                "—".to_string()
            },
            streak: color_streak,
            expected_pct: EXPECTED_COLOR * 100.0,
            options: color_opts,
        },
    ]
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
    fn familias_medem_coluna_duzia_e_cor_sem_contar_o_zero() {
        // 0 fica fora; 1..9 entrega 3 números por coluna (1-4-7, 2-5-8, 3-6-9).
        let fita = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let radars = family_radars(&fita, 100);
        assert_eq!(radars.len(), 3);

        assert_eq!(radars[0].category, "COLUNAS");
        assert_eq!(radars[0].spins, 9, "o 0 não entra no denominador");
        let cols: Vec<usize> = radars[0].options.iter().map(|o| o.hits).collect();
        assert_eq!(cols, vec![3, 3, 3]);
        assert_eq!(radars[0].options[0].label, "C1");
        assert!((radars[0].expected_pct - EXPECTED_DOZEN * 100.0).abs() < 1e-9);

        assert_eq!(radars[1].category, "DÚZIAS");
        assert_eq!(radars[1].options.iter().map(|o| o.hits).sum::<usize>(), 9);

        assert_eq!(radars[2].category, "CORES");
        assert!((radars[2].expected_pct - EXPECTED_COLOR * 100.0).abs() < 1e-9);
        assert_eq!(
            radars[2].options.iter().map(|o| o.hits).sum::<usize>(),
            9
        );

        // Último giro é 9 (C3, D1, vermelho): série de 1 em cada família.
        assert_eq!(radars[0].streak_label, "C3");
        assert_eq!(radars[0].streak, 1);
        assert_eq!(radars[2].streak_label, "VERMELHO");
    }

    #[test]
    fn serie_de_coluna_para_no_zero() {
        let fita = [1u8, 4, 7, 0, 10];
        let radars = family_radars(&fita, 100);
        // 10 e 0: o zero não pertence a coluna, então a série reinicia no 10.
        assert_eq!(radars[0].streak, 1);
        assert_eq!(radars[0].spins, 4);
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
