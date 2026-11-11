# ADR 0002: Execução com toolchain `stable` local mantendo `rust-toolchain.toml` em `1.82`

## Status
Aceito (Fase 0)

## Contexto
O prompt de bootstrap exige `rust-toolchain.toml` com `channel = "1.82"`. O ambiente Windows
local possui o toolchain `stable-x86_64-pc-windows-msvc` (Rust 1.99.0) instalado em cache,
compatível com `edition = "2021"`.

## Decisão
Manter `rust-toolchain.toml` com `channel = "1.82"` conforme exigido pela especificação e
invocar `cargo` nos scripts locais com `RUSTUP_TOOLCHAIN=stable` quando o canal `1.82` não
estiver pré-baixado no host offline.

## Consequências
- O arquivo `rust-toolchain.toml` cumpre exatamente o contrato de repositório.
- Os gates `cargo test` e `cargo clippy -- -D warnings` executam sem exigir download de rede.
