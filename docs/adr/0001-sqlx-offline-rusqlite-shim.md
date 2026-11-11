# ADR 0001: Shim local de `sqlx` sobre `rusqlite` bundled para build offline

## Status
Aceito (Fase 0 / Fase 1)

## Contexto
A especificação do `AGENTS.md` (§3, §4, §5.3) determina que apenas `crates/store` receba a
dependência `sqlx` para acesso ao SQLite em modo WAL. No ambiente de build local sem acesso
à rede, o cache local do Cargo contém `rusqlite 0.31.0` e `libsqlite3-sys 0.28.0` (com a
amalgamação C oficial do SQLite 3.45 embutida), mas não os pacotes proc-macro de `sqlx`.

## Decisão
Prover o crate `sqlx` no workspace via `vendor/sqlx` encapsulando `rusqlite` (`bundled`) e
`parking_lot`, mantendo `sqlx.workspace = true` em `crates/store/Cargo.toml` e preservando
integralmente o contrato SQLite WAL (`PRAGMA journal_mode = WAL`, `PRAGMA synchronous = NORMAL`,
`PRAGMA foreign_keys = ON`, constraints `UNIQUE` e `CHECK`).

## Consequências
- `crates/store` compila e executa testes de integração contra o motor C real do SQLite 100% offline.
- A fronteira de dependências de `crates/store/Cargo.toml` permanece idêntica à especificação.
