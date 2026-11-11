// SPDX-License-Identifier: Apache-2.0
//! Matchers nativos de estratégia em Rust para avaliação determinística em sub-milissegundos.
//! Inclui quebra de permanência em coluna/dúzia, cluster terminal, lag de Fibonacci e viés de setor.
//! Atua como fonte primária e fallback imediato quando subprocessos Python excedem 120 ms.

/// Versão do módulo de estratégias nativas.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
