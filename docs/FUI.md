# FUI.md — Frontend de operação (Tauri + React)

> Recuperado do briefing original (mockup LAELETTE, painéis, tray, kill switch)
> e endurecido com contratos de dados. É spec de produto, não sugestão.
> Fase 6 do §15 do AGENTS.md. Dono: Agente C. Nenhum outro agente escreve em
> `ui/`, `crates/ui/` ou `src-tauri/`.

## 1. Propósito

Uma tela só, para uma pessoa só, com um objetivo só: **saber em 2 segundos se o
bot está saudável, o que ele está fazendo e quanto está custando.** Não é
dashboard de vaidade. Não é landing page. Não tem card arredondado com ícone
fofo. Se um painel não muda decisão, ele não existe.

## 2. Princípios

1. **Estado antes de dado.** Cor e posição comunicam saúde; número é detalhe.
2. **Nenhum dado inventado.** Campo sem fonte mostra `—`, nunca `0`.
3. **Densidade com hierarquia.** Muita informação, uma coisa óbvia por vez.
4. **Sem scroll na tela principal.** 1440×900 cabe tudo. O que não couber é
   secundário e vai para aba.
5. **Kill sempre no mesmo pixel.** Canto superior direito, nunca se move.
6. **Mono tabular em tudo que é número.** Coluna que pula é coluna que engana.
7. **Movimento só onde significa.** Nada de fade em tudo. Pulse = perda, pisca
   = fonte morta.

## 3. Grid e tokens

- 12 colunas, gutter 8px, padding de shell 8px. Sem `position: fixed`.
- Tokens em `ui/src/theme.css`, variáveis CSS:
  `--bg:#07080c  --panel:#0d1018  --line:#1d2433  --fg:#cfd6e4
   --muted:#5f6b80  --dim:#3a4459  --ok:#22c55e  --warn:#f59e0b
   --bad:#ef4444  --accent:#22d3ee  --loss:#f472b6`
- Tipografia: `ui-monospace` em números e logs; `Inter` em rótulos. Escala
  9/10/11/12/14/18px. `font-variant-numeric: tabular-nums` global em `.num`.
- Contraste mínimo AA. Vermelho nunca é o único sinal: sempre + glifo ou + texto.
- Escuro fixo (o app é noturno). Sem tema claro na v1.

## 4. Layout (regiões fixas)

```
┌──────────────────────────────────────────────────────────────┐
│ A · BARRA DE ESTADO (36px)                                   │
├────┬──────────────────────────────────────────────┬──────────┤
│ B  │ C · MESA E JANELA                            │ D        │
│ tl │                                              │ pnl      │
│ 20 │                                              │ escada   │
│ nº │                                              │ exp      │
├────┴──────────────────────────────────────────────┴──────────┤
│ E · ABAS: DECISÕES | FILLS | ÓRFÃOS | ERROS                   │
└──────────────────────────────────────────────────────────────┘
```

## 5. Painéis e contrato de dados

Cada painel declara **fonte**. Sem fonte, o painel não entra.

### A · Barra de estado
Estado da máquina (`OBSERVANDO|ENTRADA|PAUSADO|DEGRADED|ERRO|KILLED`), relógio de
sessão, banca, P&L da sessão, degrau `n/max`, 4 LEDs (WS, MO, EXEC, DB),
botão KILL.
Fonte: `session`, `health`, `ladder_state`. LEDs: verde ok, âmbar degradado,
vermelho morto, cinza desligado.

### B · Timeline
20 últimos números em pills verticais, coluna como cor de fundo, `0` verde.
Números que dispararam matcher ganham borda ciano à esquerda. O mais recente
ganha anel ciano. Fonte: `spins` (últimos 20).

### C · Mesa e janela
Roda em SVG 37 fatias, colunas cobertas em opacidade cheia, demais em 0.55.
Sobreposição com a cobertura ativa (ex.: `COL 2 · R$ 0,30`) e exposição total.
Barra de countdown da janela com cor por faixa: >8s ciano, 8–2s âmbar, <2s
vermelho. Se `<2s`, overlay `SEM ENTRADA`. Fonte: `intent.active`,
`window.remaining_ms`.

### D · P&L, escada, exposição
Curva de P&L da sessão em canvas 152×52, ciano acima de zero, magenta abaixo,
linha de base cinza. Tabela da escada com degrau ativo em âmbar e `▸`. Rodapé
com custo do estouro e exposição atual em % da banca. Fonte: `session.pnl`,
`ladder_state`, `risk.cfg`.

### E · Abas
- **DECISÕES:** stream com motivo sempre explícito — cartão, `confidence`,
  `regime`, `chi²`. Uma linha por decisão, cor por tipo (entrada âmbar, ganho
  verde, perda vermelho, skip cinza).
- **FILLS:** `intent_id`, cobertura, valor, `acked`, latência.
- **ÓRFÃOS:** intent sem fill confirmado + estado da reconciliação.
- **ERROS:** últimos 50 de `errors`, com `kind` e hora.

## 6. Estados globais

| Estado | Cor da barra | Tray | Comportamento |
|---|---|---|---|
| OBSERVANDO | verde | verde | normal |
| ENTRADA | âmbar pulsando | âmbar | countdown ativo |
| PAUSADO | cinza | cinza | captura viva, sem aposta |
| DEGRADED | âmbar | âmbar | banner com a fonte morta |
| ERRO | vermelho | vermelho | banner + link para o log |
| KILLED | vermelho fixo | vermelho | só `/resume` ou restart volta |

Banner de DEGRADED/ERRO é uma faixa de 28px abaixo da barra de estado. Nunca
modal — modal bloqueia a leitura do resto.

## 7. Tauri — obrigações

- `tauri.conf.json`: `width 1440, height 900, minWidth 1180, minHeight 760`,
  `decorations: true`, sem `alwaysOnTop` por padrão.
- `close` → esconde a janela, **não** encerra o processo. Sair só pelo menu.
- Tray com 4 ícones de estado + menu: Mostrar, Pausar, Kill, Sair.
- Global shortcut `Ctrl+Alt+K` = kill. Registrado no boot, logado.
- IPC somente leitura na v1: `get_session`, `get_spins`, `get_health`,
  `get_ladder`, `get_decisions`, `get_fills`, `get_orphans`, `get_errors`,
  `get_cfg`. Escrita: `pause`, `resume`, `kill`, `set_base`, `set_steps`.
- Evento `bus://tick` a 4 Hz com delta. Nunca re-render por evento individual:
  o React agrupa e aplica em lote.
- Credenciais e token do Telegram em `keyring`, nunca em JSON da UI.

## 8. Acessibilidade e teclado

`Ctrl+Alt+K` kill · `P` pausa · `R` resume · `1..4` abas · `Esc` fecha painel
lateral. Todo controle alcançável por Tab, `aria-label` em botão de ícone,
`role="status"` na barra de estado, `aria-live="polite"` no stream de decisões.

## 9. Anti-slop (o revisor rejeita)

- Card branco com sombra em fundo escuro
- Cantos de 16px em tudo, ícones coloridos decorativos
- Gráfico sem eixo nem unidade
- Número sem fonte, `0` onde deveria ser `—`
- Gradiente roxo/azul "de IA"
- Texto centralizado em tabela
- Animação em todo elemento
- Mais de 2 pesos de fonte na mesma tela
- Estado só por cor (sem glifo/texto)

## 10. Critérios de aceite

- [x] 1440×900 sem scroll na tela principal
- [x] kill no mesmo pixel em todos os estados
- [x] todo painel ligado a uma fonte real do bus
- [x] `tabular-nums` em todo número; coluna não desloca ao atualizar
- [x] `—` em campo sem dado
- [x] 6 estados da §6 implementados e testados
- [x] contraste AA verificado nos 11 pares de cor
- [x] nenhum render acima de 16 ms com 500 spins no banco
- [x] tray + global shortcut funcionando com janela fechada
- [x] fechar janela não mata processo
- [x] `unslop` e `hallmark` aplicados antes do PR final
- [x] screenshot dos 6 estados anexado ao PR
