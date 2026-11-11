# TODO — Pendências por Fase (§15 do AGENTS.md)

- [ ] **Fase 3 (Gate humano)**: Rodar 20 spins reais no Chrome com `--ws-probe` e congelar o caminho vencedor em `config/schema.toml`.
- [ ] **Fase 4 (DAG + Risco + Monte Carlo)**: Implementar `crates/core/src/dag.rs`, `crates/strategy-native` (5 matchers + classificador de regime) e simulação Monte Carlo de 10.000 sessões (< 5% ruína em 8h).
- [ ] **Fase 5 (Executor `--dry-run`)**: Calibrar coordenadas em `config/calibration.toml`, implementar leitura de preview no bet slip, `confirm()` pós-validação e reconciliação de `fills` em 200 entradas simuladas.
- [ ] **Fase 6 (Operação `--live`)**: Habilitar flag `--live` com base 10 centavos e reconciliação contra extrato real da mesa.
- [ ] **Fase 7 (Plugins Python + Corpus RAG)**: Implementar `crates/strategy-runtime` (timeout 120 ms + `kill -9`), adicionar `crates/rag` e compilar os 8 PDFs em `corpus/cards/*.yaml` validados por backtest.
- [ ] **Fase 8 (FUI Tauri + Telegram + Tray)**: Implementar o dashboard completo em `ui/` e `crates/ui`, fila Telegram em `sidecar/telegram` e watchdog em `crates/telemetry`.
