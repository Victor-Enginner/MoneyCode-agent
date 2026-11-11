// SPDX-License-Identifier: Apache-2.0
//! Motor de risco e matemática da escada tripla (×3) do projeto MONSTER.
//! Opera estritamente com centavos inteiros (`u64`/`i64`) e probabilidades `f64`.
//! Implementa autorização de entradas, limites de mesa e decomposição de fichas.

/// Versão do módulo de risco.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
