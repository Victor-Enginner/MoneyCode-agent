# AGENTS.md — Projeto MONSTER (bot de roleta, observador + executor)

## 0. Papel
Você é um time de engenharia sênior construindo um app Windows nativo de
monitoramento e execução automatizada de apostas em roleta live dealer
(Playtech "Roleta Brasileira", embedada em iframe cross-origin).
O app observa a timeline, identifica padrões, decide entrada, executa cliques,
gerencia escada de apostas e expõe tudo em UI. Roda 24h sem supervisão.

Entregue código compilando, testado, com commits pequenos.
NÃO peça confirmação. NÃO resuma. NÃO explique o óbvio. Produza arquivos.

## 1. Invariantes (violar = bug crítico)

1. DINHEIRO É INTEIRO EM CENTAVOS (`u64`/`i64`). Zero `f64` para valor.
   `f64` só para probabilidade, confiança, EV e percentual.
2. Escada de apostas cresce **×3 por degrau**, nunca ×2. Ver §6.
   Cobrir 2 colunas custa 2× por perda; dobrar não recupera.
3. Idempotência obrigatória: `UNIQUE(strategy_id, spin_id)`. Um spin nunca
   gera duas entradas da mesma estratégia, nem com retry.
4. `confirm()` só depois de ler o total no bet slip e comparar com o esperado.
   Divergência > 1 centavo → `clear_bets()` e aborta.
5. `--dry-run` é o default. Dinheiro real só com `--live` explícito.
6. Kill switch sempre alcançável: global shortcut + `/kill` no Telegram +
   botão na UI. Corta o executor, mantém a captura viva.
7. Captura nunca para por erro de execução. Execução nunca roda sem captura viva.
8. Toda aposta gravada em `intents` ANTES do clique. Crash no meio → `orphan`
   reconciliado no boot contra o extrato.
9. Nenhum LLM no caminho crítico de aposta (orçamento total: 250 ms).
10. Plugin Python nunca clica, nunca toca em rede. Só devolve `BetIntent`.
11. Licença Apache-2.0 com header SPDX em todo arquivo-fonte.
12. Janela restante < 2000 ms → `Skip`, não tenta.

## 2. Fatos do domínio (não renegocie)

- Roleta single-zero: 37 pockets, 0–36. `0` perde colunas e dúzias.
- Colunas: 1–12 / 13–24 / 25–36. Dúzias idem. Pagam 2:1.
- Cobrir 2 colunas: P(acerto) = 24/37 = 0,6486. P(perda) = 13/37 = 0,3514.
- Borda da casa: 2/37 = 5,4% por unidade em cobertura dupla (2,7% por unidade
  apostada). Roulette é -EV: o bot entrega disciplina e throughput, não edge.
- Janela de aposta: ~12–15 s após o resultado. Orçamento:
  captura <150 ms · DAG <250 ms · ficha <100 ms · cliques <3000 ms.
- Limite de mesa por cobertura costuma ser R$ 500–2.000. Ler no boot e validar
  a escada ANTES de operar.
- Chips: 0.10 / 0.50 / 1 / 5 / 10 / 25. Valor precisa ser decomponível.

## 3. Stack e limites

- Rust 2021 (core, risco, DAG, executor, store) · Go 1.22 (sidecar de captura,
  Telegram, health) · Python 3.11 (apenas plugins de estratégia, subprocess) ·
  TypeScript + React + Tailwind (UI) · Tauri 2 (shell) · SQLite WAL (sqlx).
- Proibido adicionar dependência sem justificar em `docs/adr/`.
- Proibido Playwright no caminho de captura. Captura é CDP puro sobre o Chrome
  do usuário, já logado, perfil dedicado.
- Proibido `headless`. Proibido digitar senha. Perfil persistente = sessão.

## 4. Estrutura (respeite nomes)

```
Cargo.toml                      # [workspace]
crates/risk/                    # escada, gate, testes de propriedade
crates/store/                   # sqlx, migrations, retenção, órfãos
crates/capture/                 # IPC com sidecar, normalizador, dedupe
crates/strategy-native/         # matchers em Rust (sempre disponíveis)
crates/strategy-runtime/        # spawn/timeout/kill do subprocess Python
crates/core/                    # bus, DAG, regime, executor, kill switch
crates/rag/                     # ingestão de corpus, índice, retrieval
crates/telemetry/               # tracing, métricas, health, watchdog
crates/ui/                      # comandos Tauri (IPC)
sidecar/cmd/capture/            # main do sidecar
sidecar/cdp/                    # cliente CDP, auto-attach, Input.dispatch*
sidecar/cdp/inject.js           # observer injetado NO IFRAME
sidecar/wsdecap/                # walker protobuf genérico, correlação
sidecar/telegram/               # fila não-bloqueante
plugins/_proto/                 # NDJSON, sandbox, harness
plugins/*/                      # um dir por estratégia
corpus/pdf/                     # PDFs de origem
corpus/cards/*.yaml             # cartões compilados
migrations/
ui/                             # React + Tailwind
docs/adr/ · docs/runbooks/
```

## 5. Contratos

### 5.1 Evento canônico de spin (sidecar → Rust, NDJSON em `127.0.0.1:8787`)
```json
{"v":1,"spin_id":"<monotonic>","number":17,"ts":1730000000000,"source":"ws","latency_ms":84}
```
`source ∈ ws|mo|ext|ocr`. `spin_id` do WS é a autoridade. Dedupe por
`spin_id`, depois por `(number, ts/50)`. Três fontes, um evento, uma persistência.

### 5.2 BetIntent (plugin → Rust)
```json
{"v":1,"strategy_id":"quebra_permanencia_coluna","cover":[{"col":2},{"col":3}],"confidence":0.79}
```
`cover` é lista de coberturas: `col|dozen|straight|terminal`. Timeout 120 ms,
`kill -9`, fallback para o matcher nativo.

### 5.3 DDL
`migrations/0001_init.sql` com `PRAGMA journal_mode=WAL`,
`PRAGMA synchronous=NORMAL`, e as tabelas:
`spins(id, spin_id UNIQUE, number CHECK 0..36, ts, source, received_at, latency_ms)`,
`intents(id, spin_id, strategy_id, ladder_step, stake_cents, ev_cents, confidence,
decision CHECK IN placed|skipped|aborted|orphan, reason, created_at,
UNIQUE(strategy_id, spin_id))`,
`fills(id, intent_id FK, cover, stake_cents, acked, confirmed_at)`,
`ladder_state(strategy_id PK, step, loss_cents, reset_at)`,
`errors(id, kind, detail, ts)`.
Índices em `spins(ts DESC)`, `intents(created_at DESC)`, `fills(intent_id)`.
Retenção: spins > 180 dias e errors > 30 dias apagados por job diário.

## 6. Matemática da escada (implemente exatamente)

Cobrir 2 colunas: aposta `x` em cada → exposição `2x`; ganho `+x`; perda `-2x`.
Para recuperar perda acumulada `D` e lucrar `G`: `x = D + G`.

```rust
pub fn stake_cents(step: u8, base_cents: u64) -> u64 {
    let mut d = 0u64;
    for _ in 1..step { let x = d + base_cents; d += 2 * x; }
    d + base_cents
}
```
| degrau | ficha/col | exposição | perda acum. | P(chegar) |
|---|---|---|---|---|
| 1 | 0,10 | 0,20 | 0,20 | 100% |
| 2 | 0,30 | 0,60 | 0,80 | 35,14% |
| 3 | 0,90 | 1,80 | 2,60 | 12,35% |
| 4 | 2,70 | 5,40 | 8,00 | 4,34% |
| 5 | 8,10 | 16,20 | 24,20 | 1,53% |

Testes obrigatórios:
```rust
assert_eq!(stake_cents(1,10), 10); assert_eq!(stake_cents(2,10), 30);
assert_eq!(stake_cents(3,10), 90); assert_eq!(stake_cents(5,10), 810);
assert_eq!(cum_loss_cents(5,10), 2420);
// toda vitória no degrau k devolve exatamente base_cents de lucro líquido
for k in 1..=5 { assert_eq!(stake_cents(k,10) as i64 - cum_loss_cents(k-1,10) as i64, 10); }
```

Gate `authorize()` retorna `Place | Skip(WindowTooShort|Cooldown) |
Abort(LadderBlown|TableLimit|Exposure|StopLoss)`. Ordem de checagem:
StopLoss → LadderBlown → TableLimit → Exposure → Cooldown → Window.
Defaults em `config/risk.toml`: `base_cents=10, max_steps=5,
max_exposure_pct=0.16, stop_loss_cents=7260, min_window_ms=2000,
cooldown_spins=2, max_entries_per_hour=45, stop_win_pct=0.08`.

## 7. Captura (leia com atenção, é a parte que quebra)

- Chrome do usuário com `--remote-debugging-port=9222
  --user-data-dir=<perfil dedicado> --disable-blink-features=AutomationControlled`.
  Desde o Chrome 136 a porta fica MUDA sem `--user-data-dir` não-padrão.
- `Target.setAutoAttach {autoAttach:true, waitForDebuggerOnStart:false,
  flatten:true}`. Iframes cross-origin são OOPIFs: chegam como target próprio.
- Filtrar por URL contendo `playtech`. Por sessão: `Runtime.enable`,
  `Network.enable`, `Runtime.addBinding{name:"__emit"}`,
  `Runtime.evaluate` com `inject.js`.
- Fonte primária: `Network.webSocketFrameReceived` → base64 → walker protobuf
  genérico (varint + length-delimited, profundidade 8, coleta candidatos 0..36).
  Modo `--ws-probe` imprime candidatos com contador de acerto; após 20 spins
  fixar o caminho em `config/schema.toml` e ler só ele.
- Fallback: `inject.js` com MutationObserver no container da timeline, dedupe
  por `(número, data-ts)`, emitindo via `Runtime.bindingCalled`. Re-arma se o
  SPA mover a timeline. Fallback de fallback: extensão com `allFrames:true` +
  `ws://127.0.0.1:8787`. Último: OCR.
- Clique: `Input.dispatchMouseEvent` (mousePressed + mouseReleased, `buttons` e
  `clickCount` corretos) nas coordenadas de `config/calibration.toml`.
  Nunca `element.click()` — a mesa é canvas.
- Decomposição de ficha: greedy decrescente. 8,10 → 5,00+2,00+1,00+0,10.
- Health por fonte (heartbeat 5 s). Fonte morta → estado `DEGRADED` na UI.
  Nunca parar tudo por causa de uma fonte.

## 8. DAG e regime

Fan-out paralelo → fan-in → gates. Nós: `feat.window(30)` (ring buffer O(1)),
`matcher.permanencia_col`, `matcher.permanencia_doz`, `matcher.cluster_terminal`,
`matcher.fibonacci_lag`, `matcher.setor_vies`, `regime.classifier`,
`aggregator.score`, `gate.edge`, `gate.risk`, `slip.compiler`, `executor.place`,
`reconciler.confirm`.

Permanência (contrarian) e cluster (momentum) se contradizem. `regime.classifier`
usa razão de runs sobre janela de 20: `< 0.75` → Permanence, `> 1.25` → Momentum,
senão Neutral. Permanence penaliza matchers de cluster em 0.85 e vice-versa.
Sem isso você tem dois sinais opostos e nenhum desempate — é bug de projeto.

`matcher.permanencia_col`: mapeia `n==0 → 0`, senão `(n-1)/12+1`; conta streak
da coluna mais recente; dispara em `4..=6`; confiança `0.55 + 0.08*(streak-min)`
saturando em 0.95; cobre as outras duas colunas. Dúzia idem com `(n-1)/12`.

## 9. Executor e reconciliação

`place()`: grava intent → clica cobertura → aplica denominações → lê preview →
compara → `confirm()` → coleta acks por cobertura.
Ack parcial = `naked`, registra e NÃO tenta completar após o fechamento.
Pós-spin: casa `fills` com resultado e com extrato. Divergência → alerta +
sessão marcada `suspeita`. Boot: intent sem fill confirmado vira `orphan` e
é reconciliado antes de qualquer nova entrada.
`SingletonLock` órfão do perfil: limpar no boot.

## 10. UI (Tauri, FUI)

Grid escuro, mono tabular, densidade alta. Painéis: topo (sessão, banca, P&L,
degrau, LEDs WS/MO/executor/DB, kill switch), esquerda (timeline 20 números com
marcação dos que dispararam matcher), centro (roda + overlay de cobertura +
barra de countdown da janela), direita (curva P&L, tabela da escada com degrau
ativo, EV/h, drawdown), base (stream de decisões com motivo — cartão, chi² —,
aba de fills, aba de órfãos). Tray com cor por estado. Fechar janela não mata o
processo. Sem `position:fixed`. Números com `font-variant-numeric: tabular-nums`.
Todo texto com contraste AA. Nenhum dado inventado: campo sem fonte mostra `—`.

## 11. Telegram

Comandos: `/status`, `/banca`, `/stop`, `/kill`, `/resume`, `/ladder`, `/pnl`.
Alertas apenas: entrada executada, estouro de escada, stop loss, fonte morta,
divergência de extrato. Fila não-bloqueante com retry exponencial. Zero ruído.

## 12. Observabilidade

`tracing` JSON com `spin_id` como span id. Log rotativo 5 MB × 10.
Métricas: latência p50/p95 por estágio, taxa de abort, taxa de órfão, divergência,
uptime por fonte. Watchdog 5 s por componente, restart com backoff 1s→30s,
máx 5 tentativas → `DEGRADED`. Health check HTTP em `127.0.0.1:8790/health`.

## 13. Testes e validação

- Unitário + propriedade em `crates/risk` (escada, gate, ordem de checagem).
- Fixtures HTML gravadas do DOM real para o observer. Sem mesa ao vivo em CI.
- Replay de histórico SQLite com relógio virtual: DAG inteiro sem clicar.
- Monte Carlo obrigatório antes de mudar `base_cents`: 10.000 sessões de 1 h,
  reportar P(ruína), drawdown p95, EV/h. Gate: P(ruína) < 5%.
- `cargo test` verde e `cargo clippy -- -D warnings` limpo para mergear.

## 14. Divisão de agentes (ownership de arquivo — não invada o do outro)

| Agente | Dono de | Entrega |
|---|---|---|
| A · CAPTURE | `sidecar/**`, `crates/capture/**`, `config/schema.toml`, `config/calibration.toml` | WS probe, observer, dedupe, health, clique |
| B · CORE/RISK | `crates/risk/**`, `crates/core/**`, `crates/store/**`, `migrations/**`, `config/risk.toml` | escada, gate, DAG, regime, executor, reconciliação, órfãos |
| C · UI | `crates/ui/**`, `ui/**`, `src-tauri/**` | dashboard, tray, kill switch, IPC |
| D · INTEL | `crates/rag/**`, `crates/strategy-native/**`, `crates/strategy-runtime/**`, `plugins/**`, `corpus/**` | matchers nativos, runtime Python, compilação dos PDFs em cartões |
| E · OPS/QA | `crates/telemetry/**`, `sidecar/telegram/**`, `docs/**`, `.github/**`, `docker/**` | tracing, watchdog, Telegram, runbooks, CI |

Ordem de merge: B (contratos) → A (captura) → D (intel) → C (UI) → E (ops).
Ninguém muda contrato de §5 sem ADR em `docs/adr/` e aviso no commit.

## 15. Fases com gate de aceite (não avance sem cumprir)

1. Workspace compila, migrations rodam, `/health` responde. Gate: `cargo test` verde.
2. `--ws-probe` identifica o caminho do protobuf em 20 spins. Gate: 20/20 acertos.
3. Observer em fixture + dedupe. Gate: 500 spins sem duplicata nem perda.
4. DAG + risco + MC. Gate: p95 < 400 ms, P(ruína 8 h) < 5%.
5. Executor `--dry-run`, 200 entradas. Gate: zero órfão, zero preview-mismatch.
6. `--live` base 0,10, 1 h/dia. Gate: 3 sessões sem divergência de extrato.
7. Plugins + 5 cartões validados por backtest. Gate: chi² e EV dentro da banda.
8. UI completa + Telegram + tray. Gate: 8 h sem intervenção.

## 16. Definição de pronto (por PR)

Compila · testes verdes · clippy limpo · SPDX Apache-2.0 · sem `unwrap()` em
caminho de produção · sem float para dinheiro · log no erro · doc de 3 linhas
no topo do módulo · commit convencional (`feat(capture): ...`).

## 17. Anti-padrões (o revisor vai rejeitar)

Dobrar a escada ×2 · `unwrap()` no executor · LLM na decisão · `sleep` fixo em
vez de evento · clique por seletor CSS · aposta sem intent gravada · continuar
após preview divergente · float para centavos · parar a captura no erro de
execução · alerta Telegram a cada spin · UI com dado inventado · dep nova sem ADR.

## 18. Primeiro comando

`cargo new --lib` para cada crate do §4, `go mod init monster/sidecar` em
`sidecar/`, migrations do §5.3, `config/risk.toml` com os defaults do §6,
headers SPDX, CI mínimo, e comece pela Fase 1.
