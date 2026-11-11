# TODO — Pendências por Fase (§15 do AGENTS.md)

- [x] **Fase 0–3 (Workspace, Store, Risco, Captura)**: Entregue (`crates/risk`, `crates/store`, `crates/capture`, `sidecar/`).
- [x] **Fase 4 (DAG + Risco + Monte Carlo)**: Entregue (`crates/core/src/dag.rs`, `crates/strategy-native/src/lib.rs` com os 5 matchers + classificador de regime e `crates/risk/src/mc.rs` com 10.000 sessões < 5% ruína em 1h e 8h).
- [x] **Fase 5 (Executor `--dry-run`)**: Entregue (`config/calibration.toml`, `sidecar/cdp/click.go`, `crates/core/src/executor.rs`, `crates/core/src/reconciler.rs` e `crates/core/src/main.rs` com validação de preview antes do `confirm()`, idempotência e 200 entradas com zero órfãos).
- [ ] **Fase 6 (Operação `--live`)**: Habilitar atuação CDP live com base 10 centavos e reconciliação contra extrato real da mesa em 3 sessões.
- [ ] **Fase 7 (Plugins Python + Corpus RAG)**: Implementar `crates/strategy-runtime` (timeout 120 ms + `kill -9`), adicionar `crates/rag` e compilar os PDFs em `corpus/cards/*.yaml` validados por backtest.
- [ ] **Fase 8 (FUI Tauri + Telegram + Tray)**: Implementar o dashboard completo em `ui/` e `crates/ui`, fila Telegram em `sidecar/telegram` e watchdog em `crates/telemetry`.
