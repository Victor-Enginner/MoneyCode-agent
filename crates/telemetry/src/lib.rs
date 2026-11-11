// SPDX-License-Identifier: Apache-2.0
//! Subsistema de telemetria, logs estruturados (`spin_id` span), métricas e watchdog.
//! Monitora batimentos de cada fonte de captura (5 s) e gerencia transições para `DEGRADED`.
//! Expõe diagnósticos de latência p50/p95, taxa de órfãos e divergência de extrato.

/// Versão do módulo de telemetria.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
