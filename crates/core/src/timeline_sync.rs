// SPDX-License-Identifier: Apache-2.0
//! Reconciliação da fita de números da Playtech (`history_line`, 12 mais recentes) com a tabela `spins`.
//! Detecta quantos giros novos entraram no topo da fita e grava apenas os ausentes, sempre com `spin_id` único.
//! É a autoridade de coerência da TIMELINE: se a fita e o banco divergirem, o banco se alinha à mesa.

use monster_store::{insert_spin, NewSpin, SpinSource, SqlitePool, StoreError};

/// Quantidade máxima de números exibidos na fita horizontal da Playtech.
pub const MAX_STRIP: usize = 12;

/// Uma rodada real leva 40-60 s: o mesmo número não sai duas vezes dentro desta janela.
/// É a trava entre os dois caminhos que gravam giros (fita `live-` e observer `rid-`).
pub const SAME_SPIN_WINDOW_MS: i64 = 35_000;

/// Observer vivo = gravou um giro `rid-` neste intervalo. Enquanto ele vive, é a autoridade
/// da TIMELINE e a fita não grava nada (a fita relia o mesmo giro ~36 s depois e o duplicava).
pub const OBSERVER_ALIVE_MS: i64 = 120_000;

/// `true` se o observer (`rid-`) gravou algum giro nos últimos `OBSERVER_ALIVE_MS`.
pub fn observer_alive(pool: &SqlitePool, now_ms: i64) -> Result<bool, StoreError> {
    pool.with_conn(|conn| {
        let hits: i64 = conn.query_row(
            "SELECT COUNT(1) FROM spins WHERE spin_id LIKE 'rid-%' AND ts >= ?1",
            [now_ms - OBSERVER_ALIVE_MS],
            |r| r.get(0),
        )?;
        Ok(hits > 0)
    })
    .map_err(StoreError::Database)
}

/// Resultado de uma sincronização da fita com o banco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StripSyncReport {
    /// Giros novos efetivamente gravados em `spins`.
    pub inserted: usize,
    /// `true` quando a fita não pode ser correlacionada com o histórico e foi regravada como baseline.
    pub rebuilt: bool,
}

/// Lê os `limit` giros mais recentes do banco em ordem decrescente de recência (`[0]` = último sorteio).
pub fn read_newest_numbers(pool: &SqlitePool, limit: usize) -> Result<Vec<u8>, StoreError> {
    // `limit` é constante interna (nunca vem de entrada externa), então é seguro embutir no SQL.
    let sql = format!(
        "SELECT number FROM spins ORDER BY ts DESC, id DESC LIMIT {}",
        limit.min(10_000)
    );
    pool.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::with_capacity(limit);
        for row in rows {
            let n = row?;
            out.push(u8::try_from(n.clamp(0, 36)).unwrap_or(0));
        }
        Ok(out)
    })
    .map_err(StoreError::Database)
}

/// Verifica se o banco já tem esse número gravado dentro de `tolerance_ms` do instante informado.
/// Evita que a mesma rodada entre duas vezes (WS + fita DOM) na TIMELINE.
pub fn has_recent_number(
    pool: &SqlitePool,
    number: u8,
    ts: i64,
    tolerance_ms: i64,
) -> Result<bool, StoreError> {
    let sql = format!(
        "SELECT COUNT(1) FROM spins WHERE number = {} AND ABS(ts - {}) <= {}",
        number.min(36),
        ts,
        tolerance_ms.max(0)
    );
    pool.with_conn(|conn| {
        let hits: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(hits > 0)
    })
    .map_err(StoreError::Database)
}

/// Lê até `limit` giros em ordem cronológica (mais antigo primeiro) para alimentar `HistoryBuffer`.
pub fn load_chronological_history(pool: &SqlitePool, limit: usize) -> Vec<u8> {
    match read_newest_numbers(pool, limit) {
        Ok(mut newest) => {
            newest.reverse();
            newest
        }
        Err(_) => Vec::new(),
    }
}

/// Descarta números fora de `0..=36` (frame corrompido ou leitura parcial do DOM).
pub fn sanitize_strip(strip: &[u8]) -> Vec<u8> {
    strip
        .iter()
        .copied()
        .filter(|&n| n <= 36)
        .take(MAX_STRIP)
        .collect()
}

/// Calcula quantos giros do topo de `strip` ainda não existem em `newest` (`0` = já em sincronia).
///
/// A fita é ordenada do mais recente (`[0]`) para o mais antigo, assim como `newest`.
/// Se `newest[0]` aparece em `strip[p]` e o restante do trecho coincide, então exatamente
/// `p` giros novos chegaram à mesa. Sem sobreposição confiável devolve `None`: a fita NÃO é
/// regravada (era isso que duplicava 12 giros a cada reconexão e inventava "repetições").
pub fn overlap_new_spins(strip: &[u8], newest: &[u8]) -> Option<usize> {
    if strip.len() < 4 {
        return Some(0);
    }
    if newest.is_empty() {
        return Some(strip.len());
    }
    for p in 0..strip.len() {
        let overlap = (strip.len() - p).min(newest.len());
        if overlap < 3 {
            continue;
        }
        if strip[p..p + overlap] == newest[..overlap] {
            return Some(p);
        }
    }
    None
}

/// Compatibilidade: quantos giros do topo seriam novos (`strip.len()` quando não há sobreposição).
pub fn count_new_spins(strip: &[u8], newest: &[u8]) -> usize {
    if strip.len() < 4 {
        return 0;
    }
    if newest.is_empty() {
        return strip.len();
    }
    for p in 0..strip.len() {
        let overlap = (strip.len() - p).min(newest.len());
        if overlap < 3 {
            continue;
        }
        if strip[p..p + overlap] == newest[..overlap] {
            return p;
        }
    }
    strip.len()
}

/// Sincroniza a fita ao vivo com o banco, gravando somente os giros ausentes (idempotente por `spin_id`).
///
/// `game_code` entra no `spin_id` para auditoria; `now_ms` ancora a ordem temporal dos giros recuperados.
pub fn sync_strip_to_db(
    pool: &SqlitePool,
    strip: &[u8],
    game_code: &str,
    now_ms: i64,
) -> Result<StripSyncReport, StoreError> {
    let strip = sanitize_strip(strip);
    if strip.len() < 4 {
        return Ok(StripSyncReport::default());
    }
    // Observer vivo grava cada giro no instante real: a fita só preenche quando ele cai.
    if observer_alive(pool, now_ms)? {
        return Ok(StripSyncReport::default());
    }
    let newest = read_newest_numbers(pool, MAX_STRIP)?;
    // Sem sobreposição com histórico existente (reconexão, fita reordenada pelo SPA): grava no
    // máximo o giro do topo — e só se ele não entrou nos últimos 35 s. Nunca regrava a fita.
    let (new_count, rebuilt) = match overlap_new_spins(&strip, &newest) {
        Some(n) => (n, newest.is_empty() && n == strip.len()),
        None => (1, false),
    };
    if new_count == 0 {
        return Ok(StripSyncReport::default());
    }

    let gc = if game_code.is_empty() {
        "table"
    } else {
        game_code
    };
    // Decide tudo antes de gravar: assim números repetidos DENTRO da mesma fita continuam válidos,
    // enquanto uma rodada já registrada pelo WS não é gravada de novo.
    let mut batch: Vec<(u8, i64, String)> = Vec::new();
    for idx in (0..new_count).rev() {
        let number = strip[idx];
        let ts = now_ms - (idx as i64) * 1_000;
        if has_recent_number(pool, number, ts, SAME_SPIN_WINDOW_MS)? {
            continue;
        }
        batch.push((
            number,
            ts,
            format!("live-{gc}-{now_ms}-{idx}-{number}"),
        ));
    }

    let mut inserted = 0usize;
    for (number, ts, spin_id) in batch {
        let spin = NewSpin {
            spin_id,
            number,
            ts,
            source: SpinSource::Mo,
            received_at: now_ms,
            latency_ms: Some(0),
        };
        match insert_spin(pool, &spin) {
            Ok(_) => inserted += 1,
            // Já gravado por outro caminho (WS/observer): idempotência por `spin_id`.
            Err(StoreError::ConstraintViolation(_)) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(StripSyncReport { inserted, rebuilt })
}

#[cfg(test)]
mod tests {
    use super::*;
    use monster_store::init_pool;

    #[test]
    fn empty_db_recebe_a_fita_inteira() {
        let pool = init_pool(":memory:").expect("pool");
        let strip = [5u8, 9, 20, 31, 2, 14];
        assert_eq!(count_new_spins(&strip, &[]), strip.len());
        let report = sync_strip_to_db(&pool, &strip, "gc-1", 1_730_000_000_000).expect("sync");
        assert_eq!(report.inserted, strip.len());
        assert!(report.rebuilt);
        let newest = read_newest_numbers(&pool, MAX_STRIP).expect("read");
        assert_eq!(newest, strip.to_vec());
    }

    #[test]
    fn um_giro_novo_grava_apenas_um_registro() {
        let pool = init_pool(":memory:").expect("pool");
        let base = [9u8, 20, 31, 2, 14, 7];
        sync_strip_to_db(&pool, &base, "gc-2", 1_730_000_000_000).expect("baseline");
        let strip = [5u8, 9, 20, 31, 2, 14];
        assert_eq!(count_new_spins(&strip, &base), 1);
        let report = sync_strip_to_db(&pool, &strip, "gc-3", 1_730_000_060_000).expect("sync");
        assert_eq!(report.inserted, 1);
        assert!(!report.rebuilt);
        let newest = read_newest_numbers(&pool, MAX_STRIP).expect("read");
        // O giro novo entra no topo e os anteriores continuam abaixo, sem duplicar o histórico.
        assert_eq!(&newest[..strip.len()], &strip[..]);
        assert_eq!(newest.len(), strip.len() + 1);
    }

    #[test]
    fn fita_identica_nao_duplica_nada() {
        let pool = init_pool(":memory:").expect("pool");
        let strip = [12u8, 12, 3, 8, 26, 1];
        sync_strip_to_db(&pool, &strip, "gc-4", 1_730_000_000_000).expect("baseline");
        assert_eq!(count_new_spins(&strip, &strip), 0);
        let report = sync_strip_to_db(&pool, &strip, "gc-5", 1_730_000_090_000).expect("sync");
        assert_eq!(report.inserted, 0);
        assert_eq!(read_newest_numbers(&pool, MAX_STRIP).expect("read").len(), strip.len());
    }

    #[test]
    fn mesmo_numero_repetido_no_topo_e_contado_pela_sobreposicao() {
        let newest = [5u8, 9, 20, 31, 2, 14];
        let strip = [5u8, 5, 9, 20, 31, 2];
        // Ambiguidade resolvida pelo menor número de giros novos (nunca duplica histórico).
        assert_eq!(count_new_spins(&strip, &newest), 1);
    }

    #[test]
    fn fita_com_numero_repetido_preserva_todos_os_giros() {
        let pool = init_pool(":memory:").expect("pool");
        // Fita real da mesa (mais recente primeiro): `20` e `4` repetem dentro da MESMA fita.
        // Uma coluna só fica "repetindo" porque esses giros existem: descartar repetição
        // encurta o streak e o matcher nunca chega ao gatilho.
        let strip = [20u8, 11, 20, 5, 13, 9, 11, 6, 23, 4, 24, 4];
        let report = sync_strip_to_db(&pool, &strip, "gc-rep", 1_730_000_000_000).expect("sync");
        assert_eq!(
            report.inserted,
            strip.len(),
            "nenhum giro da fita pode ser descartado por repetir número"
        );
        let newest = read_newest_numbers(&pool, MAX_STRIP).expect("read");
        assert_eq!(newest, strip.to_vec());
    }

    #[test]
    fn fita_com_repeticao_preserva_o_streak_de_coluna_para_o_matcher() {
        let pool = init_pool(":memory:").expect("pool");
        let strip = [20u8, 11, 20, 5, 13, 9, 11, 6, 23, 4, 24, 4];
        sync_strip_to_db(&pool, &strip, "gc-strk", 1_730_000_000_000).expect("sync");
        let recent = read_newest_numbers(&pool, MAX_STRIP).expect("read");
        // Coluna real (`(n-1)%3+1`): 20 e 11 e 5 são coluna 2. A coluna 2 repete 4x no topo.
        let streak = recent
            .iter()
            .map(|&n| (n - 1) % 3 + 1)
            .take_while(|&c| c == 2)
            .count();
        assert_eq!(streak, 4, "streak da coluna 2 chega ao gatilho de 4 repetições");
    }

    #[test]
    fn fita_sem_correlacao_nao_e_regravada() {
        let newest = [1u8, 2, 3, 4, 5, 6];
        let strip = [30u8, 31, 32, 33, 34, 35];
        assert_eq!(overlap_new_spins(&strip, &newest), None);
    }

    #[test]
    fn reconexao_sem_sobreposicao_grava_so_o_topo_e_nunca_a_fita() {
        let pool = init_pool(":memory:").expect("pool");
        let t0 = 1_730_000_000_000;
        sync_strip_to_db(&pool, &[1u8, 2, 3, 4, 5, 6], "gc", t0).expect("baseline");
        // Reconexão: a fita volta embaralhada/sem encaixe com o banco.
        let strip = [30u8, 31, 32, 33, 34, 35, 36, 7, 8, 9, 10, 11];
        let r = sync_strip_to_db(&pool, &strip, "gc", t0 + 50_000).expect("sync");
        assert_eq!(r.inserted, 1, "só o giro do topo pode entrar");
        // A mesma fita relida segundos depois não grava nada (topo já está nos últimos 35 s).
        let r2 = sync_strip_to_db(&pool, &strip, "gc", t0 + 52_000).expect("sync");
        assert_eq!(r2.inserted, 0);
        assert_eq!(read_newest_numbers(&pool, 50).expect("read").len(), 7);
    }

    #[test]
    fn observer_vivo_impede_a_fita_de_regravar_o_mesmo_giro_36s_depois() {
        // Caso real de 13/11 08:17: rid grava o 10, a fita relê o 10 no topo 36 s depois.
        let pool = init_pool(":memory:").expect("pool");
        let t0 = 1_730_000_000_000;
        for (i, n) in [13u8, 28, 20, 25].iter().enumerate() {
            insert_spin(&pool, &NewSpin {
                spin_id: format!("rid-{i}"), number: *n, ts: t0 + i as i64 * 39_000,
                source: SpinSource::Mo, received_at: t0, latency_ms: Some(0),
            }).expect("rid");
        }
        insert_spin(&pool, &NewSpin {
            spin_id: "rid-x".into(), number: 10, ts: t0 + 4 * 39_000,
            source: SpinSource::Mo, received_at: t0, latency_ms: Some(0),
        }).expect("rid");
        let r = sync_strip_to_db(&pool, &[10u8, 25, 20, 28, 13, 7], "gc", t0 + 4 * 39_000 + 36_000).expect("sync");
        assert_eq!(r.inserted, 0, "observer vivo: a fita não grava");
        assert_eq!(read_newest_numbers(&pool, 50).expect("read").len(), 5);
    }

    #[test]
    fn giro_ja_gravado_pelo_observer_nao_duplica_pela_fita() {
        let pool = init_pool(":memory:").expect("pool");
        let t0 = 1_730_000_000_000;
        sync_strip_to_db(&pool, &[9u8, 20, 31, 2, 14, 7], "gc", t0).expect("baseline");
        // Observer (rid-) grava o 17 no instante real; a fita chega 20 s depois com o 17 no topo.
        insert_spin(&pool, &NewSpin {
            spin_id: "rid-x".into(), number: 17, ts: t0 + 45_000,
            source: SpinSource::Mo, received_at: t0 + 45_000, latency_ms: Some(0),
        }).expect("rid");
        let r = sync_strip_to_db(&pool, &[17u8, 9, 20, 31, 2, 14], "gc", t0 + 65_000).expect("sync");
        assert_eq!(r.inserted, 0);
    }

    #[test]
    fn fita_sanitizada_descarta_fora_de_faixa_e_excedente() {
        let strip = [1u8, 200, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];
        let clean = sanitize_strip(&strip);
        assert_eq!(clean.len(), MAX_STRIP);
        assert!(clean.iter().all(|&n| n <= 36));
        assert_eq!(clean[0], 1);
        assert_eq!(clean[1], 3);
    }
}
