// SPDX-License-Identifier: Apache-2.0
//! Shim local da interface `sqlx` para SQLite WAL (bundled `libsqlite3-sys`).
//! Garante compilação determinística offline mantendo a assinatura do workspace.
//! Documentado em `docs/adr/0001-sqlx-offline-rusqlite-shim.md`.

use std::sync::Arc;
use parking_lot::Mutex;
pub use rusqlite::{params, Connection, Error as RusqliteError, OptionalExtension};
use thiserror::Error;

/// Erro unificado de banco de dados compatível com `sqlx::Error`.
#[derive(Debug, Error)]
pub enum Error {
    /// Erro retornado pelo motor SQLite subjacente.
    #[error("sqlite error: {0}")]
    Database(#[from] rusqlite::Error),
    /// Erro de configuração ou migração.
    #[error("migration error: {0}")]
    Migrate(String),
}

/// Pool de conexões SQLite thread-safe em modo WAL.
#[derive(Clone, Debug)]
pub struct SqlitePool {
    conn: Arc<Mutex<Connection>>,
}

impl SqlitePool {
    /// Abre uma conexão em memória ou arquivo com suporte a chaves estrangeiras.
    pub fn connect_sync(uri: &str) -> Result<Self, Error> {
        let conn = if uri == ":memory:" || uri == "sqlite::memory:" || uri == "sqlite://:memory:" {
            Connection::open_in_memory()?
        } else {
            let path = uri
                .strip_prefix("sqlite://")
                .or_else(|| uri.strip_prefix("sqlite:"))
                .unwrap_or(uri);
            Connection::open(path)?
        };
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Executa uma operação síncrona protegida pelo mutex do pool.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    {
        let guard = self.conn.lock();
        Ok(f(&guard)?)
    }
}
