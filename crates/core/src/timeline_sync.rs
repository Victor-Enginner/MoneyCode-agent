// SPDX-License-Identifier: Apache-2.0
//! Reconciliação da fita de números da Playtech (`history_line`, 12 mais recentes) com a tabela `spins`.
//! Detecta quantos giros novos entraram no topo da fita e grava apenas os ausentes, sempre com `spin_id` único.
//! É a autoridade de coerência da TIMELINE: se a fita e o banco divergirem, o banco se alinha à mesa.

use monster_store::{
    insert_spin, open_session, session_floor_ms, NewSpin, SpinSource, SqlitePool, StoreError,
};

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

/// Pausa máxima entre o último giro gravado e o boot para a sessão anterior continuar valendo.
pub const SESSION_GAP_MS: i64 = 10 * 60_000;

/// Um giro lido do banco: `(número, ts)`.
type SpinRow = (u8, i64);

/// Giros da sessão atual (`ts >= started_at`), mais recente primeiro.
fn session_rows(pool: &SqlitePool, limit: usize) -> Result<Vec<SpinRow>, StoreError> {
    let floor = session_floor_ms(pool)?;
    let sql = format!(
        "SELECT number, ts FROM spins WHERE ts >= ?1 ORDER BY ts DESC, id DESC LIMIT {}",
        limit.min(10_000)
    );
    pool.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([floor], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::with_capacity(limit);
        for row in rows {
            let (n, ts) = row?;
            out.push((u8::try_from(n.clamp(0, 36)).unwrap_or(0), ts));
        }
        Ok(out)
    })
    .map_err(StoreError::Database)
}

/// Como `read_newest_numbers`, mas só a sessão atual: é o que a TIMELINE e os padrões enxergam.
pub fn read_session_newest_numbers(pool: &SqlitePool, limit: usize) -> Result<Vec<u8>, StoreError> {
    Ok(session_rows(pool, limit)?.into_iter().map(|(n, _)| n).collect())
}

/// `ts` do giro da sessão atual imediatamente anterior a `before_ts` (`None` = nenhum).
pub fn session_prev_spin_ts(pool: &SqlitePool, before_ts: i64) -> Result<Option<i64>, StoreError> {
    let floor = session_floor_ms(pool)?;
    pool.with_conn(|conn| {
        conn.query_row(
            "SELECT MAX(ts) FROM spins WHERE ts >= ?1 AND ts < ?2",
            [floor, before_ts],
            |r| r.get::<_, Option<i64>>(0),
        )
    })
    .map_err(StoreError::Database)
}

/// Giros da sessão atual gravados até `until_ts` (inclusive), mais recente primeiro.
pub fn read_session_newest_until(
    pool: &SqlitePool,
    until_ts: i64,
    limit: usize,
) -> Result<Vec<u8>, StoreError> {
    let floor = session_floor_ms(pool)?;
    let sql = format!(
        "SELECT number FROM spins WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts DESC, id DESC LIMIT {}",
        limit.min(10_000)
    );
    pool.with_conn(|conn| {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([floor, until_ts], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::with_capacity(limit);
        for row in rows {
            out.push(u8::try_from(row?.clamp(0, 36)).unwrap_or(0));
        }
        Ok(out)
    })
    .map_err(StoreError::Database)
}

/// Giros da sessão atual em ordem cronológica (mais antigo primeiro), para a janela de padrões.
pub fn load_session_chronological(pool: &SqlitePool, limit: usize) -> Vec<u8> {
    match read_session_newest_numbers(pool, limit) {
        Ok(mut newest) => {
            newest.reverse();
            newest
        }
        Err(_) => Vec::new(),
    }
}

/// O que o boot decidiu sobre a sessão da TIMELINE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionDecision {
    /// A fita da mesa continua o histórico gravado (troca rápida do core): nada muda.
    Continue,
    /// Sessão nova: histórico anterior sai da janela; a fita da mesa vira a base.
    Open {
        started_at: i64,
        seeded: usize,
        reason: &'static str,
    },
}

/// Decide, na primeira fita lida após o boot, se a sessão anterior continua ou se abre outra.
///
/// Continua só se o último giro anterior ao boot tem menos de `SESSION_GAP_MS` e a fita da
/// mesa encaixa nele. Senão abre sessão e semeia a fita (menos os giros que o observer já
/// gravou desde o boot), para os padrões partirem do que a mesa mostra, nunca de números
/// de outra sessão. `None` = fita curta demais, tentar na próxima leitura.
pub fn ensure_session(
    pool: &SqlitePool,
    strip: &[u8],
    boot_ms: i64,
    now_ms: i64,
) -> Result<Option<SessionDecision>, StoreError> {
    let strip = sanitize_strip(strip);
    if strip.len() < 4 {
        return Ok(None);
    }
    let floor = session_floor_ms(pool)?;
    let rows = session_rows(pool, 64)?;
    let (fresh, older): (Vec<SpinRow>, Vec<SpinRow>) =
        rows.into_iter().partition(|&(_, ts)| ts >= boot_ms);
    let k = fresh.len().min(strip.len());
    let tail = &strip[k..];
    let older_nums: Vec<u8> = older.iter().take(MAX_STRIP).map(|&(n, _)| n).collect();

    let reason = match older.first() {
        _ if floor == 0 => "primeira sessão registrada",
        None => "sessão anterior vazia",
        Some(&(_, last_ts)) if boot_ms - last_ts > SESSION_GAP_MS => "pausa maior que 10 min",
        Some(_) if tail.len() < 4 || overlap_new_spins(tail, &older_nums).is_none() => {
            "fita da mesa não continua o histórico"
        }
        Some(_) => return Ok(Some(SessionDecision::Continue)),
    };

    let mut started_at = fresh.last().map_or(now_ms - 100, |&(_, ts)| ts - 100);
    if let Some(&(_, last_ts)) = older.first() {
        started_at = started_at.max(last_ts + 1);
    }
    open_session(pool, started_at, reason)?;

    let mut seeded = 0usize;
    for i in (k..strip.len()).rev() {
        let number = strip[i];
        let spin = NewSpin {
            spin_id: format!("base-{started_at}-{i}-{number}"),
            number,
            ts: started_at + (strip.len() - i) as i64,
            source: SpinSource::Mo,
            received_at: now_ms,
            latency_ms: Some(0),
        };
        match insert_spin(pool, &spin) {
            Ok(_) => seeded += 1,
            Err(StoreError::ConstraintViolation(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(Some(SessionDecision::Open {
        started_at,
        seeded,
        reason,
    }))
}

/// Intervalo entre o giro da entrada e o giro seguinte gravado acima do qual houve giro
/// perdido (rodada real: 37-45 s; dois giros juntos passam de 74 s).
pub const MISSED_SPIN_GAP_MS: i64 = 60_000;

/// Acha na fita da mesa o que saiu DEPOIS do giro em que a entrada foi feita.
///
/// `before` = giros da sessão até o da entrada (mais recente primeiro). Devolve os giros da
/// fita acima dele (mais recente primeiro); o último da lista é o que resolveu a entrada.
/// `None` quando a fita não mostra nada depois desse giro ou não encaixa com segurança.
pub fn spins_after_trigger(strip: &[u8], before: &[u8]) -> Option<Vec<u8>> {
    let strip = sanitize_strip(strip);
    let matches_at = |p: usize| {
        let k = 4.min(strip.len() - p).min(before.len());
        k >= 3 && strip[p..p + k] == before[..k]
    };
    if strip.len() < 4 || matches_at(0) {
        return None;
    }
    (1..strip.len()).find(|&p| matches_at(p)).map(|p| strip[..p].to_vec())
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
    let newest = read_session_newest_numbers(pool, MAX_STRIP)?;
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

    fn gravar(pool: &SqlitePool, id: &str, number: u8, ts: i64) {
        insert_spin(
            pool,
            &NewSpin {
                spin_id: id.to_string(),
                number,
                ts,
                source: SpinSource::Mo,
                received_at: ts,
                latency_ms: Some(0),
            },
        )
        .expect("insert");
    }

    /// Sessão antiga terminando em ... 23, 16 (16 = mais recente) em `fim_ms`.
    fn sessao_antiga(pool: &SqlitePool, fim_ms: i64) {
        for (i, n) in [5u8, 13, 17, 28, 27, 1, 34, 14, 15, 1, 28, 27, 1, 23, 16].iter().enumerate() {
            gravar(pool, &format!("rid-old-{i}"), *n, fim_ms - (14 - i as i64) * 40_000);
        }
    }

    #[test]
    fn reboot_com_fita_nova_abre_sessao_e_timeline_e_so_a_mesa() {
        let pool = init_pool(":memory:").expect("pool");
        let boot = 10_000_000;
        sessao_antiga(&pool, boot - 8 * 60_000);
        let fita = [16u8, 23, 12, 20, 20, 1, 1, 29, 13, 17, 25, 8];
        let d = ensure_session(&pool, &fita, boot, boot + 500).expect("ok").expect("decidiu");
        assert!(matches!(d, SessionDecision::Open { seeded: 12, .. }), "{d:?}");
        assert_eq!(read_session_newest_numbers(&pool, 50).expect("read"), fita.to_vec());
        // histórico longo intacto para o walk-forward
        assert_eq!(read_newest_numbers(&pool, 100).expect("read").len(), 27);
        // giro novo do observer entra no topo, sem o 16 da sessão anterior embaixo
        gravar(&pool, "rid-new-1", 7, boot + 40_000);
        let s = read_session_newest_numbers(&pool, 4).expect("read");
        assert_eq!(s, vec![7, 16, 23, 12]);
    }

    #[test]
    fn giro_gravado_pelo_observer_antes_da_fita_nao_duplica() {
        let pool = init_pool(":memory:").expect("pool");
        let boot = 10_000_000;
        sessao_antiga(&pool, boot - 60_000);
        gravar(&pool, "rid-new-1", 16, boot + 2_000);
        let fita = [16u8, 23, 12, 20, 20, 1, 1, 29, 13, 17, 25, 8];
        let d = ensure_session(&pool, &fita, boot, boot + 3_000).expect("ok").expect("decidiu");
        assert!(matches!(d, SessionDecision::Open { seeded: 11, .. }), "{d:?}");
        assert_eq!(read_session_newest_numbers(&pool, 50).expect("read"), fita.to_vec());
    }

    #[test]
    fn troca_rapida_do_core_continua_a_sessao() {
        let pool = init_pool(":memory:").expect("pool");
        let boot = 10_000_000;
        let fita0 = [16u8, 23, 1, 27, 28, 1, 15, 14, 34, 1, 27, 28];
        assert!(matches!(
            ensure_session(&pool, &fita0, boot - 50_000, boot - 49_000).expect("ok"),
            Some(SessionDecision::Open { .. })
        ));
        // 2 giros saíram durante a troca do core: a fita ainda encaixa -> continua
        let fita = [9u8, 15, 16, 23, 1, 27, 28, 1, 15, 14, 34, 1];
        assert_eq!(
            ensure_session(&pool, &fita, boot, boot + 500).expect("ok"),
            Some(SessionDecision::Continue)
        );
    }

    #[test]
    fn pausa_longa_abre_sessao_mesmo_com_fita_igual() {
        let pool = init_pool(":memory:").expect("pool");
        let boot = 10_000_000;
        let fita0 = [16u8, 23, 1, 27, 28, 1, 15, 14, 34, 1, 27, 28];
        assert!(ensure_session(&pool, &fita0, boot - 3_600_000, boot - 3_599_000)
            .expect("ok")
            .is_some());
        let d = ensure_session(&pool, &fita0, boot, boot + 500).expect("ok").expect("decidiu");
        assert!(
            matches!(d, SessionDecision::Open { reason: "pausa maior que 10 min", .. }),
            "{d:?}"
        );
    }

    #[test]
    fn giro_perdido_resolve_a_entrada_pelo_numero_da_fita() {
        // Caso real de 13/11 21:18: entrada após o 6, observer perdeu um giro (X=31), gravou 15.
        let before = [6u8, 4, 6, 10, 30, 7];
        let fita = [15u8, 31, 6, 4, 6, 10, 30, 7, 32, 7, 16, 23];
        let after = spins_after_trigger(&fita, &before).expect("encaixa");
        assert_eq!(after, vec![15, 31]);
        assert_eq!(*after.last().expect("resultado"), 31, "a entrada foi resolvida no 31, não no 15");
    }

    #[test]
    fn giro_perdido_antes_da_analise_e_achado_na_fita() {
        // Caso real de 14/11 08:36: o observer gravou 35, 14 e depois 8; o 12 se perdeu.
        // Sem ele o bot leu 35-14-8 como 3x coluna 2 e entrou duas rodadas cedo.
        let pool = init_pool(":memory:").expect("pool");
        for (i, n) in [1u8, 5, 14, 36, 0, 7, 35, 14].iter().enumerate() {
            gravar(&pool, &format!("rid-{i}"), *n, 1_000_000 + i as i64 * 40_000);
        }
        let ts_14 = 1_000_000 + 7 * 40_000;
        let ts_8 = ts_14 + 79_000;
        gravar(&pool, "rid-new-8", 8, ts_8);
        assert_eq!(session_prev_spin_ts(&pool, ts_8).expect("q"), Some(ts_14));
        let before = read_session_newest_until(&pool, ts_14, 12).expect("q");
        let fita = [8u8, 12, 14, 35, 7, 0, 36, 14, 5, 1];
        assert_eq!(spins_after_trigger(&fita, &before), Some(vec![8, 12]));
    }

    #[test]
    fn fita_atrasada_ainda_mostra_o_giro_perdido() {
        // fita lida antes do 15 aparecer: o topo já é o giro perdido
        let before = [6u8, 4, 6, 10, 30, 7];
        let fita = [31u8, 6, 4, 6, 10, 30, 7, 32, 7, 16, 23, 12];
        assert_eq!(spins_after_trigger(&fita, &before), Some(vec![31]));
    }

    #[test]
    fn sem_giro_novo_na_fita_nao_inventa_resultado() {
        let before = [6u8, 4, 6, 10, 30, 7];
        let fita = [6u8, 4, 6, 10, 30, 7, 32, 7, 16, 23, 12, 20];
        assert_eq!(spins_after_trigger(&fita, &before), None);
        // fita sem relação com a sessão também não decide nada
        assert_eq!(spins_after_trigger(&[1, 2, 3, 5, 8, 13, 21], &before), None);
    }

    #[test]
    fn fita_curta_espera_a_proxima_leitura() {
        let pool = init_pool(":memory:").expect("pool");
        assert_eq!(ensure_session(&pool, &[1, 2], 0, 1).expect("ok"), None);
    }

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
