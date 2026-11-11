// SPDX-License-Identifier: Apache-2.0
//! Núcleo de orquestração do projeto MONSTER: event bus, DAG, classificador de regime e executor.
//! Opera em modo `--dry-run` por padrão e coordena reconciliação pós-spin e kill switch.
//! Orçamento estrito de latência (<250 ms no DAG) sem chamadas de LLM no caminho crítico.

/// Versão do módulo core.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
