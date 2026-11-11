// SPDX-License-Identifier: Apache-2.0
//! Camada de persistência SQLite WAL do projeto MONSTER.
//! Gerencia migrações de schema, inserção de spins/intents/fills e varredura de órfãos.
//! Garante idempotência transacional por `(strategy_id, spin_id)` e retenção periódica.

/// Versão do módulo de persistência.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
