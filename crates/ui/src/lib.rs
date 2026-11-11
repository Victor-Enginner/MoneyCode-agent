// SPDX-License-Identifier: Apache-2.0
//! Esqueleto de comandos IPC Tauri para a interface FUI do projeto MONSTER.
//! Expõe estado da sessão, escada ativa, timeline de spins e acionamento do kill switch.
//! A implementação visual completa é reservada para a Fase 8 conforme §15 do AGENTS.md.

/// Versão do módulo de interface IPC.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
