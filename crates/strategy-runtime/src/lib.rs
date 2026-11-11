// SPDX-License-Identifier: Apache-2.0
//! Runtime de isolamento e supervisão de plugins Python via protocolo NDJSON em stdio.
//! Aplica timeout rígido de 120 ms por avaliação com terminação forçada e fallback nativo.
//! Garante que plugins externos emitam apenas `BetIntent` sem acesso direto a cliques ou rede.

/// Versão do módulo de runtime de plugins.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
