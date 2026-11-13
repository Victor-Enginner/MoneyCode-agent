# MONSTER

Motor nativo de captura determinística, gestão de risco e execução auditada para Roleta Brasileira.
Arquitetura híbrida: Core/Risk/Store em Rust (2021), Sidecar CDP/WS em Go (1.22) e SQLite WAL.
Invariante monetária: valores exclusivamente em centavos inteiros (`u64`/`i64`) e escada tripla (`×3`).
Captura primária via Chrome DevTools Protocol (`Target.setAutoAttach` + `Network.webSocketFrameReceived`).
Fallback automático via `MutationObserver` injetado no OOPIF da Playtech com deduplicação canônica.
Persistência transacional de `spins`, `intents` e `fills` com reconciliação de órfãos no boot.
Modo `--dry-run` ativo por padrão; operação `--live` exige calibração e validação prévia de preview.
Consulte `AGENTS.md` para especificações de invariantes e `docs/LINKS.md` para referências técnicas.
Licenciado sob Apache License 2.0 (veja `LICENSE` e `NOTICE`).
Status atual: Fases 0 a 3 implementadas (`WS_PROBE_READY`).

## Como iniciar

Um script (`start-monster.ps1`) com três modos; os dois atalhos são só duplo clique no mesmo script.

| você clica | o que acontece |
|---|---|
| `MONSTER OBSERVAR.lnk` | sobe Chrome + sidecar + core em `--dry-run`: lê a mesa, decide, **não aposta** |
| `MONSTER LIVE.lnk` | igual, em `--live`: o executor **aposta dinheiro real** quando o gate autoriza |
| `start-monster.bat -Probe` | só o sidecar em `--ws-probe` (diagnóstico de WS, sem core) |

Se já houver um `monster-core` vivo o script se recusa a subir outro e imprime o `taskkill` pronto.
Painel: `http://127.0.0.1:8791` · health: `http://127.0.0.1:8790/health`.
