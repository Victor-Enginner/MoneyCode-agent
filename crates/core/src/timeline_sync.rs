// SPDX-License-Identifier: Apache-2.0
//! Reconciliação da fita de números da Playtech (`history_line`, 12 mais recentes) com a tabela `spins`.
//! Detecta quantos giros novos entraram no topo da fita e grava apenas os ausentes, sempre com `spin_id` único.
//! É a autoridade de coerência da TIMELINE: se a fita e o banco divergirem, o banco se alinha à mesa.

use monster_store::{insert_spin, NewSpin, SpinSource, SqlitePool, StoreError};

/// Quantidade máxima de números exibidos na fita horizontal da Playtech.
pub const MAX_STRIP: usize = 12;

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
/// `p` giros novos chegaram à mesa. Sem sobreposição confiável, a fita inteira é tratada como nova.
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
    let newest = read_newest_numbers(pool, MAX_STRIP)?;
    let new_count = count_new_spins(&strip, &newest);
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
        if has_recent_number(pool, number, ts, 3_000)? {
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

    Ok(StripSyncReport {
        inserted,
        rebuilt: new_count == strip.len(),
    })
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
    fn fita_sem_correlacao_e_regravada_como_baseline() {
        let newest = [1u8, 2, 3, 4, 5, 6];
        let strip = [30u8, 31, 32, 33, 34, 35];
        assert_eq!(count_new_spins(&strip, &newest), strip.len());
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
