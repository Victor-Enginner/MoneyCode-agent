# ECOSYSTEM_RADAR.md — Mapa Tecnológico Completo (24/7, GPU, Cloud e Análise Sistêmica)

> Mapeamento integral de todos os repositórios estratégicos do ecossistema MONSTER (`MoneyCode-agent`), divididos pelo que **já está absorvido em código hoje (Fases 0–6)** e pelo que entra nas **Fases 7–9 (24/7 Autônomo, GPU/ML Local, Grafo de Conhecimento e Orquestração Multi-Agente)**.

---

## 1. Núcleo e Captura (Fases 0, 2, 3, 5 — Já Integrados)

| Repositório / Fonte | Papel no MONSTER | Status no Código |
| :--- | :--- | :--- |
| **`jogatudo.com` (Playtech Roleta Brasileira)** | Alvo primário (`eurolive-frontend.playtechgaming.com` + `ielive-gateway.ptielive.com`) | ✅ Operacional (`sidecar/cmd/capture/main.go`) |
| **`Victor-Enginner/Roullet-bot-eistein`** | Base de referência (decoder protobuf, observer, engine, SQLite) portada para Rust/Go | ✅ Portado para `sidecar/` + `crates/` |
| **CDP — Chrome DevTools Protocol** | `Target.setAutoAttach`, `Network.webSocketFrameReceived`, `FrameSent`, `Input.dispatchMouseEvent` | ✅ Nativo em `sidecar/cdp/` |
| **`vercel-labs/agent-browser` & `agent-desktop`** | Modelo de sessão/attach resiliente e fallback de clique em nível de SO | ✅ Padrão de reconexão e coordenadas em `sidecar/cdp/click.go` |
| **`daijro/camoufox`** | Plano C anti-detect (Firefox C++ stealth) caso uma casa bloqueie o Chromium CDP | 📌 Catalogado como fallback de captura via `inject.js` |
| **`morluto/rea` & `JoasASantos/REA-Prompts`** | Metodologia de engenharia reversa do comportamento ao binário (`--ws-probe`, `--dump-op`) | ✅ Implementado em `sidecar/wsdecap/` (`/3/103/1/1` congelado) |
| **`gorilla/websocket` & `protobuf.dev`** | Cliente WS do sidecar Go e walker recursivo wire-type (`0, 1, 2, 5`) + inflate zlib | ✅ Implementado em `sidecar/wsdecap/walker.go` |

---

## 2. Análise Sistêmica Complexa, Grafo de Conhecimento e Ciência de Dados (Fases 4 e 7)

Estes repositórios são a chave para evoluir o nosso motor Walk-Forward (`crates/core/src/backtest.rs`) para um **analisador estatístico e estrutural contínuo 24/7**:

| Repositório | O que entrega | Como incrementar no MONSTER |
| :--- | :--- | :--- |
| **`Graphify-Labs/graphify`** *(145k ★)* | Converte código, schemas SQL, configs TOML e os 8 PDFs de estratégia em um **grafo consultável com AST determinístico** | **Fase 7 (`corpus/`)**: indexar todas as tabelas (`spins`, `intents`, `fills`), `crates/` e PDFs para que qualquer agente entenda instantaneamente o impacto de uma nova estratégia antes de codar |
| **`DataLab-Platform/DataLab`** | Processamento científico rigoroso: testes **Chi-quadrado ($\chi^2$)**, **Wald-Wolfowitz Runs Test**, entropia de Shannon e bootstrap | **Fase 4.2 (`crates/core/src/backtest.rs`)**: adicionar detector de desvio $\chi^2$ por setor físico da roda e teste exato de runs sobre os 500 giros do `HistoryBuffer` |
| **`HazyResearch/ncm` (Next Concept Modeling)** | Modelagem do próximo conceito/estado em sequências discretas | **Fase 8**: usar representação de estado de janela (`Regime + Setor + Latência do Croupier`) para prever transições entre `Permanence` e `Momentum` antes do 4º giro |
| **`semantica-agi/semantica` & `Egonex-AI/Understand-Anything`** | Rastreabilidade graph-native de decisões e exploração visual da arquitetura | Auditar cadeias completas `Spin -> Matcher -> Walk-Forward -> Intent -> Fill -> Reconcile` |
| **`langgenius/dify` & `onyx-dot-app/onyx`** | Pipeline RAG self-hosted sobre o corpus de PDFs de roleta e gestão de banca | Base de consulta para criação assistida de novos cartões de estratégia |

---

## 3. Inferência em Milissegundos e Aceleração GPU Local (Fase 8)

Para rodar classificadores de regime neurais em `< 10 ms` sem depender de API externa no caminho crítico:

| Repositório | O que entrega | Como incrementar no MONSTER |
| :--- | :--- | :--- |
| **`featherless-ai/simple-jev`** | Transforma qualquer modelo aberto compacto em um **classificador/endpoint JEV de baixíssima latência (`< 10 ms`)** | **Classificador Neural de Regime (`regime.jev`)**: substituir/combinar a heurística de runs por um classificador local treinado nos milhares de giros do `monster.db` |
| **`Zagitta/gpu-unlock`** *(18.6k ★)* | Desbloqueia até **2× throughput FP16 / BF16 / TF32** em GPUs GeForce séries 20/30/40 | Dobrar a velocidade de treinamento/inferência local na máquina Windows ao rodar simulações massivas ou modelos locais (`Edge0` / `colibri`) |
| **`Edge0-AI/Edge0` & `JustVugg/colibri`** | Inferência MoE 35B em 2,5 GB de RAM e execução hierárquica em CPU/NVMe | Rodar um agente supervisor local 24/7 analisando logs, propondo ajustes de `config/risk.toml` e auditando sessões sem custo de API |
| **`mizorewww/laya-coreml` & `Mesh-LLM/mesh-llm`** | Decisões tipadas em ~5 ms (Mac) ou distribuição de compute entre máquinas na rede | Permitir que o analisador pesado rode em outra máquina/cloud enquanto o executor continua leve na máquina da mesa |

---

## 4. Cérebro de Agente 24/7, Memória Persistente e Orquestração (Fases 1 e 8)

Para atingir a sua meta de **deixar os agentes rodando 24h por dia**, administrando apenas bugs, evolução e novas estratégias:

| Repositório | O que entrega | Como incrementar no MONSTER |
| :--- | :--- | :--- |
| **`garrytan/gbrain`** *(32.6k ★)* | **Garry's Opinionated OpenClaw/Hermes Agent Brain**: memória persistente estruturada e cérebro operacional de agente | **Memória Operacional 24/7 do MONSTER**: guardar histórico de cada sessão, quais estratégias performaram melhor em cada horário/croupier, lições aprendidas de bugs e catálogo vivo de hipóteses testadas |
| **`Ryder-Sun/Meldwork`** | Workspace multi-agente local-first com permissões escopadas, evidência por run e human-in-the-loop | Orquestrar os 4 papéis (`Capturador`, `Estrategista`, `Guardião de Risco`, `Auditor de UI`) com provas de execução (`cargo test` + backtest de 479+ giros) antes de promover qualquer estratégia para `--live` |
| **`google-research/harness2`** *(23.4k ★)* | Harness científico de avaliação e benchmark de agentes | **Gate Automático de Novas Estratégias**: toda nova estratégia criada pelo agente precisa passar pelo `harness` (479+ giros reais + 10.000 sessões Monte Carlo com `P(ruína) < 5%`) antes de entrar no `default_matchers()` |
| **`bastani-inc/atomic` & `kenryu42/cc-safety-net`** | Stages com gates obrigatórios e guarda pré-execução contra comandos destrutivos | Proteger o repositório e o banco `monster.db` durante operação autônoma 24/7 |
| **`routerhubSI/uncensored-ai-api`** | Gateway unificado OpenAI/Anthropic-compatível para múltiplos modelos (Claude, GPT, Gemini, DeepSeek, Qwen) | Redundância de LLM para os agentes de pesquisa e geração de estratégias fora do caminho crítico |

---

## 5. Design da FUI e Observabilidade Contínua (Fases 6 e 8)

| Repositório | O que entrega | Como incrementar no MONSTER |
| :--- | :--- | :--- |
| **`heydamir/unslop`**, **`Nutlope/hallmark`** & **`pbakaus/impeccable`** | Auditoria anti-slop e refinamento cirúrgico de UI operacional de alta densidade | Base do nosso contrato [`docs/FUI.md`](file:///O:/Archives/Eistein/FUI%20AGENT%20ROULLET/docs/FUI.md): fundo `#07080c`, tabular-nums, zero dados inventados (`—`), Kill Switch fixo |
| **`openobserve/openobserve` & `rerun-io/rerun`** | Telemetria e visualização temporal de séries e latências | Monitoramento de longo prazo (semanas/meses de operação 24/7) |

---

## 6. Próximos Passos Práticos de Evolução Contínua

1. **Laboratório Automático de Estratégias (`crates/core/src/backtest.rs` + `DataLab`)**:
   - A cada 100 novos giros gravados no `monster.db`, recalcular automaticamente o ranking Walk-Forward e o teste $\chi^2$ / Runs Test de todas as 9 estratégias.
2. **Memória Persistente de Sessões estilo `gbrain` (`docs/memory/` + SQLite)**:
   - Gravar relatório automático de fim de sessão (melhor padrão, pior padrão, horários de maior permanência vs alternância).
3. **Classificador de Regime Rápido estilo `simple-jev` / `ncm`**:
   - Treinar nos giros reais acumulados no `monster.db` para antecipar mudanças de regime antes de entrar na escada.
