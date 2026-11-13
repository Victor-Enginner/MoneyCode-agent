# MONSTER — bot de roleta (Roleta Brasileira · Playtech)

O MONSTER observa uma mesa de roleta ao vivo, detecta padrões, decide entradas e **clica as apostas
sozinho** no seu navegador, com escada de gale, travas de segurança e um painel (FUI) em
`http://127.0.0.1:8791`. Roda 24h com um watchdog que religa tudo se cair.

> **Leia antes de usar.**
> - **+18. Apostas envolvem risco real de perder todo o dinheiro depositado.**
> - A roleta tem vantagem da casa: em média você perde **2,7% de tudo que aposta**. Nenhum padrão,
>   escada ou "puxada" muda isso no longo prazo. O MONSTER entrega **disciplina e execução**, não lucro garantido.
> - Automatizar apostas pode violar os termos do site do cassino e a lei do seu país. A responsabilidade é sua.
> - Deposite só o que você aceita perder inteiro. A configuração inicial deste README é para **R$ 40**.

---

## Sumário

1. [Como o MONSTER aposta](#1-como-o-monster-aposta)
2. [Requisitos](#2-requisitos)
3. [Instalação passo a passo](#3-instalação-passo-a-passo)
4. [Configuração inicial (banca de R$ 40)](#4-configuração-inicial-banca-de-r-40)
5. [Primeira execução](#5-primeira-execução)
6. [Operação 24h, parar e reiniciar](#6-operação-24h-parar-e-reiniciar)
7. [O painel (FUI)](#7-o-painel-fui)
8. [Telegram (opcional)](#8-telegram-opcional)
9. [Regras de ouro](#9-regras-de-ouro)
10. [Problemas comuns](#10-problemas-comuns)
11. [Para desenvolvedores](#11-para-desenvolvedores)

---

## 1. Como o MONSTER aposta

O fluxo é **progressivo e com prioridade fixa**. A cada giro o motor avalia todos os padrões e escolhe
**uma** entrada, nesta ordem:

### 1º — Quebra de permanência (apostas "por fora") · foco principal

Quando uma categoria repete demais, o MONSTER aposta que ela **vai quebrar**:

| Padrão | Dispara quando… | Aposta em |
|---|---|---|
| **Coluna** | a mesma coluna sai **3 a 6** vezes seguidas | as **outras 2 colunas** |
| **Dúzia** | a mesma dúzia sai **4 a 6** vezes seguidas | as **outras 2 dúzias** |
| **Vermelho / Preto** | a mesma cor sai **6 a 12** vezes | a **cor oposta** |
| **Baixos / Altos** | 1–18 ou 19–36 sai **6 a 12** vezes | a **metade oposta** |
| **Par / Ímpar** | a mesma paridade sai **6 a 12** vezes | a **paridade oposta** |
| **Número repetido** | o mesmo número sai 2× seguidas | as 2 colunas que não o contêm |

O **0** não conta para nenhuma sequência. Um giro antes do gatilho aparece o **pré-alerta**
("OBSERVANDO … aguardando confirmação"), que **não aposta**. As faixas podem ser ajustadas no painel
(botão ⚙ AJUSTES).

**Quando uma quebra está disparando, ela sempre vence.** Nenhuma outra estratégia passa na frente dela.

### 2º — Outras entradas por fora

`cluster_terminal`, `fibonacci_lag` e viés de setor só entram em giros **sem** quebra de permanência.
Os dois de viés de setor nascem **desligados** (em janela curta são ruído).

### 3º — Entradas por número (progressão pela análise da roleta)

Só entram quando **não há quebra rolando** e os **terminais estão concentrados**: no máximo
**5 finais diferentes nos últimos 12 giros** (numa mesa normal a média é 7; isso acontece ~5% do tempo).
Mesa "bagunçada" (muitos finais diferentes) é terreno da quebra de permanência, não de número.

| Estratégia | Ideia |
|---|---|
| `terminal_quente` | finais 2-3-6-9 em série → aposta nos 2 finais mais frequentes |
| `cluster_final` | família de finais (1-4-7 / 2-5-8 / 3-6-9) dominando → cobre a família + o 0 |
| `invictor_conservadora` | final quebrante (1,3,5,7,9) seguido de múltiplo (2,4,6,8) → 0 + quebrante + mais atrasado + múltiplo |
| `invictor_arrojada` | sequência de múltiplos curta (2–3) ou longa (5–8) → 0 + 3 quebrantes mais atrasados |

Os plenos são clicados **na grade** da mesa (nunca na pista oval de vizinhos), no máximo **24 números**.

### A escada (gale)

- **Por fora:** cada degrau aposta **3×** o anterior (cobrindo 2 colunas, dobrar não recupera a perda).
  Ganhar em qualquer degrau devolve a perda acumulada + 1 ficha de lucro.
- **Números:** a ficha por número é calculada para recuperar a perda e lucrar 10¢: `(perda + 10¢) / (36 − casas)`.
- Cada estratégia tem a **sua** escada. O gale de uma só sai quando **o mesmo padrão** dispara de novo.

Escada de coluna com a configuração inicial (ficha de 20¢):

| Degrau | Por coluna | Na mesa | Perda acumulada se perder |
|---|---|---|---|
| 1 | R$ 0,20 | R$ 0,40 | R$ 0,40 |
| 2 | R$ 0,60 | R$ 1,20 | R$ 1,60 |
| 3 | R$ 1,80 | R$ 3,60 | R$ 5,20 |
| 4 | R$ 5,40 | R$ 10,80 | R$ 16,00 |
| 5 | R$ 16,20 | R$ 32,40 | R$ 48,40 |

Com R$ 40 o **5º degrau não cabe**: o limite de exposição (55% da banca) bloqueia, e a escada para no 4º
com perda máxima de **R$ 16**. É o comportamento esperado e seguro.

### Travas de segurança (sempre ativas)

- **Bilhete conferido antes de confirmar:** se o valor lido na mesa diferir do planejado, o MONSTER limpa
  as fichas e **não aposta** ("preview mismatch").
- **Aposta registrada antes do clique:** se algo cair no meio, a aposta vira "órfã" e é reconciliada no próximo início.
- **Uma entrada por estratégia por giro**, mesmo com nova tentativa.
- **Kill switch:** botão ■ KILL no painel, `Ctrl+Alt+K`, Ctrl+C na janela ou `stop-monster.bat`.
  A aposta em andamento termina; nenhuma nova começa.
- **Giro duplicado não entra:** cada rodada é gravada uma vez (observador como fonte única).

---

## 2. Requisitos

- **Windows 10 ou 11** (os scripts são PowerShell).
- **Google Chrome** ou Chromium (usado só pelo MONSTER, com perfil próprio).
- **Rust** via [rustup](https://rustup.rs) — o projeto fixa a versão em `rust-toolchain.toml` (instala sozinha).
  No Windows o rustup pede o **Visual Studio Build Tools** (C++); aceite a instalação sugerida.
- **Go 1.22+** — [go.dev/dl](https://go.dev/dl).
- **Git** — [git-scm.com](https://git-scm.com).
- Conta na plataforma com a mesa **Roleta Brasileira (Playtech)** e saldo de pelo menos **R$ 40**.
- Internet estável e PC que **não suspenda** na tomada (Configurações → Sistema → Energia).

---

## 3. Instalação passo a passo

Abra o **PowerShell** e rode:

```powershell
git clone https://github.com/Victor-Enginner/MoneyCode-agent.git
cd MoneyCode-agent
```

### 3.1 Diga onde está o seu Chrome (só se não for o padrão)

O padrão é `C:\Program Files\Google\Chrome\Application\chrome.exe` com o perfil em `.\chrome-profile`.
Se o seu for diferente:

```powershell
copy config\paths.example.ps1 config\paths.ps1
notepad config\paths.ps1
```

e ajuste `$ChromeExe`, `$ProfileDir` e, se quiser outra mesa, `$TargetUrl`. Esse arquivo não vai para o Git.

### 3.2 Libere scripts locais (uma vez)

```powershell
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
```

### 3.3 (Opcional) Atalho na área de trabalho

Crie um atalho para `start-monster.bat` com o argumento `-Live` e chame de **MONSTER LIVE**.
Exemplo de destino: `C:\...\MoneyCode-agent\start-monster.bat -Live`.

---

## 4. Configuração inicial (banca de R$ 40)

**Não precisa mexer em nada:** os valores abaixo já são o padrão do código.

| Item | Valor | Onde muda |
|---|---|---|
| Ficha por fora (coluna, dúzia, cor…) | **R$ 0,20** por cobertura | painel → ⚙ AJUSTES (ou `config/stake.json`) |
| Lucro alvo por ciclo nos números | **R$ 0,10** | `config/stake.json` → `number_base_cents` |
| Degraus da escada | **5** (×3) | `crates/core/src/main.rs` |
| Limite de exposição | **55%** da banca | idem |
| Máx. de números por entrada | **24** | `crates/risk/src/lib.rs` |
| Gatilhos (coluna 3–6, dúzia 4–6, cor/altos/par 6–12) | | painel → ⚙ AJUSTES (`config/tuning.json`) |

A banca é **lida da mesa** a cada giro; você não digita o saldo. Antes de operar, o MONSTER roda um
**Monte Carlo de 10.000 sessões** e mostra no log a chance de quebrar a banca (`P(ruína)`).
Com R$ 40 e ficha de 20¢ ela fica em ~3% por hora.

Arquivo `config/stake.json` (criado pelo painel; opcional):

```json
{
  "base_cents": 20,
  "number_base_cents": 10
}
```

---

## 5. Primeira execução

1. Dê **duplo clique** no atalho **MONSTER LIVE** (ou rode `.\start-monster.bat -Live`).
   O primeiro início **compila** o core (Rust) e a captura (Go): pode levar alguns minutos.
2. O Chrome do MONSTER abre na mesa. **Na primeira vez, faça login** no site nele. O perfil guarda a sessão.
3. A janela mostra `Tudo no ar. Painel: http://127.0.0.1:8791` e passa a exibir o log ao vivo.
4. Abra o painel `http://127.0.0.1:8791` e confira:
   - **topo:** estado, banca (lida da mesa), P&L, LEDs WS/MO/EXEC/DB verdes;
   - **TIMELINE** com os últimos números iguais aos da mesa;
   - aba **1 · Decisões** com uma linha por giro.

Para **só observar** (sem apostar), rode `.\start-monster.bat` sem `-Live`.

---

## 6. Operação 24h, parar e reiniciar

- **Watchdog:** um supervisor invisível mantém Chrome, captura e core vivos. Se a mesa ficar **5 min sem
  giro** (sessão caída), ele fecha e reabre o Chrome. Em manutenção da mesa, a espera cresce (10, 20, 30 min).
- **Iniciar com o Windows (recomendado para 24h):**

  ```powershell
  powershell -ExecutionPolicy Bypass -File scripts\install-autostart.ps1
  ```

  Cria a tarefa **MONSTER Watchdog**: sobe no logon e religa o watchdog a cada 5 min se ele morrer.
  Ela retoma o **último modo** escolhido (live/observação). Remover:
  `Unregister-ScheduledTask -TaskName 'MONSTER Watchdog' -Confirm:$false`.
- **Parar:** `Ctrl+C` na janela do MONSTER, ou `stop-monster.bat`, ou ■ KILL no painel.
  **Fechar a janela no X não para** o MONSTER (proposital).
- **Reiniciar:** abra o atalho de novo. Ele para tudo com segurança (sem cortar aposta), recompila e sobe.
- **Logs:** `logs\core-live.log` (decisões e apostas), `logs\capture.log` (giros e cliques), `logs\watchdog.log`.

O que **não** se resolve sozinho: o site **deslogar** (faça login de novo no Chrome do MONSTER), o
Windows reiniciar sem você entrar na conta, e queda longa de internet.

---

## 7. O painel (FUI)

`http://127.0.0.1:8791`

| Área | O que mostra |
|---|---|
| Topo | estado, banca, P&L, degrau, LEDs de saúde, ⚙ AJUSTES, ■ KILL |
| Timeline | últimos 20 números (borda amarela = giro que disparou padrão) |
| Mapa de calor | a roda com as casas/vizinhanças mais frequentes dos últimos 200 giros reais e o desvio (σ) por setor |
| Mesa & cobertura | o pano com a cobertura ativa e a tabela da escada |
| Abas | Decisões, Fills (apostas feitas), Órfãos, Erros, Padrões (walk-forward), Brain |
| 🧪 Laboratório | testa cada estratégia contra o acaso: treino 60% / validação 20% / **teste oculto 20%**, com correção para várias hipóteses. Veredito: APROVADO / REPROVADO / DADOS INSUFICIENTES |
| 🔬 Física & Estatística | dinâmica da bola (didática), assinatura do crupiê, qui²/entropia/autocorrelação, Monte Carlo da escada, **força dos terminais e mapa de puxadas** |
| ⚙ AJUSTES | liga/desliga cada estratégia, faixas de gatilho e ficha. Nada muda até **SALVAR AJUSTES** |

O Laboratório precisa de giros: com o MONSTER rodando 24h (~70 giros/h), em 2–3 dias há dados para julgar.

---

## 8. Telegram (opcional)

1. Crie um bot com o **@BotFather** e copie o token.
2. `copy config\telegram.example.json config\telegram.json` e cole o token.
3. Mande qualquer mensagem ao bot: ele responde com o seu `chat_id`. Coloque em `config/telegram.json`.
4. Reinicie o MONSTER.

Comandos: `/status`, `/banca`, `/pnl`, `/ladder`, `/stop`, `/kill`, `/resume`.
Alertas: entrada executada, estouro de escada, stop loss, fonte de dados caída, divergência de extrato.

---

## 9. Regras de ouro

1. **Não aposte à mão na mesma conta** com o MONSTER rodando. O saldo é um só: o gale dele fica sem
   dinheiro, um clique seu no meio da aposta dele faz ela abortar, e o painel mistura os resultados.
   Para jogar à mão, pare o MONSTER antes.
2. **Não clique na mesa** do Chrome do MONSTER enquanto ele opera.
3. **Comece com o mínimo** (R$ 40) e só aumente a ficha depois que o 🧪 Laboratório mostrar algo aprovado.
4. **Um MONSTER por conta e por PC.** O iniciador já impede dois ao mesmo tempo.
5. Acompanhe o **P&L real nas abas Decisões/Fills**, não só a variação do saldo.

---

## 10. Problemas comuns

| Sintoma | Causa provável | O que fazer |
|---|---|---|
| "Chrome não encontrado" | caminho diferente do padrão | ajuste `config\paths.ps1` (passo 3.1) |
| Janela fecha com erro de build | Rust/Go ausentes ou sem Build Tools | instale os requisitos; leia a mensagem na janela (ela fica aberta) |
| "O core não respondeu em 90 s" | Chrome sem login ou mesa não carregou | faça login no Chrome do MONSTER e abra o atalho de novo |
| Muitos "preview mismatch" no log | você clicou na mesa, ou aposta manual em paralelo | não toque na mesa com o MONSTER rodando |
| Banca aparece R$ 40,00 fixa no início | saldo ainda não carregado | normal; atualiza no primeiro giro |
| Timeline diferente da mesa | reconexão da mesa | o MONSTER corrige sozinho; se persistir, reinicie |
| Bot parou à noite | site deslogou / Windows reiniciou | veja `logs\watchdog.log`; faça login e abra o atalho |

---

## 11. Para desenvolvedores

| Parte | Linguagem | Pasta |
|---|---|---|
| Core: DAG de decisão, risco/escada, executor, store, laboratório | Rust | `crates/` |
| Captura CDP, cliques, sidecar de Telegram | Go | `sidecar/` |
| Painel | HTML/JS (servido pelo core) | `ui/live.html` |
| Supervisão e inicialização | PowerShell | `start-monster.ps1`, `scripts/` |

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p monster-core -- --mc-only   # só o Monte Carlo da configuração atual
cargo run -p monster-core -- --lab       # laboratório de hipóteses no banco local
```

Invariantes do projeto (dinheiro em centavos inteiros, escada ×3, intent antes do clique, `--dry-run`
padrão, kill switch…) estão em [`AGENTS.md`](AGENTS.md). Banco local: `monster.db` (SQLite, fora do Git).

Licença: Apache-2.0 (veja [`LICENSE`](LICENSE)).
