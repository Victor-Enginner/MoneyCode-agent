# Varredura do MONSTER — 17/11/2026

Base: `logs/core-live.log` e `logs/capture.log` (13–17/11), código em `main` e a suíte (181 testes).
Cada item traz a evidência. Nada aqui é opinião sem número.

## 1. Números do período

| Evento | Vezes |
|---|---|
| Entradas confirmadas | 538 |
| Entradas desfeitas (`EXECUTOR-ABORT`) | 101 (≈ 19%) |
| Conferência de saldo OK / divergente | 264 / 93 |
| Movimento de saldo sem entrada do bot | 171 |
| Bloqueio por exposição (`Abort(Exposure)`) | 115 |
| Giro perdido pelo observer | 16 |
| Ciclos de gale coluna/dúzia/nº repetido: green / abandonados | 145 / 21 |

## 2. Corrigido nesta varredura

| # | Defeito | Evidência | Correção |
|---|---|---|---|
| C1 | **Gale abandonado antes do green** | 21 de 166 ciclos: em 13, outra estratégia tomou o giro do gale; em 7, o gatilho "sumiu" (0 ou sequência acima do máximo) | Gale obrigatório para **todas** as estratégias: o giro seguinte à perda repete as mesmas coberturas, sem disputa e sem gatilho. Os gates de risco continuam valendo. Teste: `gale_da_coluna_vai_ate_o_green_mesmo_sem_gatilho` |

| C2 | **Escada 1:1 usava a ×3 das colunas** (0,40 → 1,20 → 3,60 → 10,80) e o gate media 2 coberturas | Com R$ 40–100 de banca, o 1:1 parava no 3º/4º degrau | Escada própria que dobra recuperando tudo, com o 0 coberto: 0,40 → 0,90 → 1,90 → 4,00 → 8,30 → 17,10 → 35,20 …; todo green rende +0,30. Até 10 gales (`EVEN_MONEY_MAX_STEPS`), cortados por banca e exposição. Gate próprio de 1 cobertura |
| C3 | **Modo cautela abandonava gale em andamento** | A cautela liga justamente nas sequências longas de 1:1, quando o gale está correndo | Gale em andamento passa pela cautela; ela só segura entradas novas |

## 3. Limites que existem e o operador precisa decidir

| # | Limite | Efeito | Opções |
|---|---|---|---|
| L1 | **Banca × 5º degrau.** Coluna com base 0,20: degraus 0,40 → 1,20 → 3,70 → 11,10 → **33,30**. O ciclo inteiro custa ~R$ 49,70 | Com banca de **R$ 40 o 5º degrau é impossível**: depois de 4 perdas sobram ~R$ 24. A trava de 55% (`max_exposure_pct`) também o bloqueia até a banca passar de R$ 61. É o "para no 3º gale" | (a) banca ≥ R$ 61; (b) base 0,10 (5 degraus custam ~R$ 25); (c) aceitar 3 gales com R$ 40 |
| L2 | `stop_loss_cents = 4000` | Perder R$ 40 na sessão corta as entradas — **inclusive no meio de um gale 1:1** (o 7º degrau acumula R$ 70,10) | Subir junto com a banca, ou deixar a banca zerada como único corte |
| L3 | Scripts param no degrau 3 (`SCRIPT_MAX_STEPS`) | Imposto por mim em 14/11, **não pedido pelo operador** | Manter 3 ou liberar até 5 |

## 4. Defeitos abertos, por prioridade

| # | Prioridade | Defeito | Evidência | Caminho |
|---|---|---|---|---|
| A1 | **Alta** | Clique de pleno que não pega na mesa | 20 de 104 entradas de número com 1–7 fichas faltando | Hoje segue sem desfazer (ADR 0006). Definitivo: o sidecar ler quais plenos têm ficha no pano e reclicar só esses |
| A2 | **Alta** | Detecção de "rodada aberta" por texto da página (`PLACE YOUR BETS`) | 18 cliques esperaram 11–13 s e caíram depois do fechamento | Ler o estado do round no protocolo WS da Playtech (fonte primária do §7), não no texto |
| A3 | Média | Conferência de saldo ruidosa | 93 divergências em 357 conferências. Parte é aposta manual (171 movimentos externos), parte é entrada de número parcial | Separar "divergência com aposta manual na rodada" e mostrar só as divergências limpas |
| A4 | Média | UNDO lento | 20 fichas levam ~30 s e invadem a rodada seguinte | UNDO em lote (o sidecar repete sem reler o slip a cada ficha) |
| A5 | Média | Estratégias que não vieram do operador continuam no código | `fibonacci_lag` está **ligada** e entrou em live; `setor_vies`, `cluster_*`, `invictor_*`, `terminal_quente` também existem | Decidir com o operador: manter desligadas ou remover |
| A6 | Baixa | `main.rs` com ~1.700 linhas | Loop de decisão, telemetria, HTTP e vigias no mesmo arquivo | Extrair o loop em módulos testáveis (sessão, gale, vigia, reconciliação) |
| A7 | Baixa | Configuração espalhada | `risk.toml`, `stake.json`, `tuning.json` e constantes no código (`EVEN_MONEY_BASE_CENTS`, `SCRIPT_MAX_STEPS`, `TABLE_CLEAR_WAIT_MS`) | Um arquivo de configuração do operador, lido no boot e mostrado no painel |

## 5. O que foi verificado e funciona

- Sessão por fita da mesa, troca rápida mantendo a sessão (log `[SESSAO]`).
- Giro perdido completado pela fita antes da análise (`[GIRO-PERDIDO]`, 16 vezes).
- Pausa real (`/stop`, tecla P), banca zerada, modo cautela, trava por script.
- Reclique da cobertura por fora que não pegou; espera da mesa limpar antes do gale.

## 6. Fora do código

- **171 movimentos de saldo sem entrada do bot** no período. Aposta manual com o bot ligado distorce o P&L e a conferência de saldo, e faz o bot abortar entradas (A3).
