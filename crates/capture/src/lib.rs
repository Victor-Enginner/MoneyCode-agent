// SPDX-License-Identifier: Apache-2.0
//! Ponte de captura e normalização de eventos de spin do sidecar Go.
//! Valida números no intervalo `0..=36`, aplica deduplicação canônica e publica no bus.
//! Desacopla as fontes WS protobuf e MutationObserver do núcleo de decisão.

/// Versão do módulo de captura.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
