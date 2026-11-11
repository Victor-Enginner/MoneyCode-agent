// SPDX-License-Identifier: Apache-2.0
//! Teste de integração do `monster-store` com banco SQLite em memória.
//! Verifica aplicação do schema `0001_init.sql`, inserção de 3 spins e
//! erro de constraint ao tentar duplicar `spin_id`.

use monster_store::{init_pool, insert_spin, NewSpin, SpinSource, StoreError};

#[test]
fn integration_insert_three_spins_and_reject_duplicate_spin_id() {
    let pool = init_pool(":memory:").expect("banco em memória inicializado");

    let spins = [
        NewSpin {
            spin_id: "playtech-spin-0001".to_string(),
            number: 17,
            ts: 1_730_000_000_000,
            source: SpinSource::Ws,
            received_at: 1_730_000_000_084,
            latency_ms: Some(84),
        },
        NewSpin {
            spin_id: "playtech-spin-0002".to_string(),
            number: 0,
            ts: 1_730_000_030_000,
            source: SpinSource::Ws,
            received_at: 1_730_000_030_071,
            latency_ms: Some(71),
        },
        NewSpin {
            spin_id: "playtech-spin-0003".to_string(),
            number: 36,
            ts: 1_730_000_060_000,
            source: SpinSource::Mo,
            received_at: 1_730_000_060_140,
            latency_ms: Some(140),
        },
    ];

    for spin in &spins {
        let row_id = insert_spin(&pool, spin).expect("inserção de spin válido");
        assert!(row_id > 0);
    }

    let duplicate = NewSpin {
        spin_id: "playtech-spin-0002".to_string(),
        number: 23,
        ts: 1_730_000_090_000,
        source: SpinSource::Ws,
        received_at: 1_730_000_090_060,
        latency_ms: Some(60),
    };

    let result = insert_spin(&pool, &duplicate);
    assert!(
        matches!(result, Err(StoreError::ConstraintViolation(_))),
        "esperava erro de constraint UNIQUE em spin_id duplicado, obteve: {result:?}"
    );
}
