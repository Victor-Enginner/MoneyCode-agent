# Auditoria de cliques e janela de apostas — 18/11/2026

Fontes (só leitura): `logs/capture.log` (9.312 linhas `[click-cdp]`), `logs/core-live.log`
(239 `[EXEC-DIAG]`), `monster.db` aberto com `mode=ro`. Bot em `--live`, nada foi parado.
Script de medição: rajada = linhas `[click-cdp]` entre duas linhas `[spin #]` do sidecar
(resolução de 1 s do log).

## 1. Números

### Tempo da fita até os cliques (s, p50 / p95 / máx)

| dia | entradas com clique | 1º clique | último clique | rajadas com buraco ≥ 3 s |
|---|---|---|---|---|
| 15/11 | 303 | 0 / 4 / 5 | 6 / 35 / 41 | 30 (10%) |
| 17/11 | 83 | 0 / 4 / 11 | 5 / 30 / 40 | 26 (31%) |
| 18/11 | 5 | 0 / 1 / 1 | 3 / 9 / 9 | 2 |

- Sem conferência no meio, o clique é rápido: 22:02:16–18 de 17/11 pôs 4 pares
  ficha+coluna em ~2 s (≈ 300 ms por clique, já com os 220 ms de pausa).
- O que demora é a **conferência entre coberturas**: no 17/11 20:27–20:36 os cliques saem
  de 5 em 5 s (`chip` 4 s → `col` 9 s → `chip` 14 s → `col` 22 s → `col` 30 s).
- Somando os ~3,9 s que a fita leva para aparecer depois do resultado, um clique 18 s depois
  da fita já cai **depois do fechamento da mesa (~21,1 s)**. No 17/11 houve rajadas com
  cliques a 24, 30, 35 e 40 s da fita: essas fichas entram na **rodada seguinte**
  (o `isRoundOpenNow` segura o clique até "FAÇA SUAS APOSTAS" voltar e então clica).

### Desfecho das entradas

| métrica | desde 17/11 | histórico |
|---|---|---|
| intents placed / skipped / aborted / orphan | 80 / 79 / 7 / 0 | 682 / 916 / 136 / 2 |
| `cover_mismatch` | 14 | 81 |
| `partial_entry` + `number_partial` | 16 | 23 |
| `click_unresolved` | 2 | 21 |
| `extrato_divergence` | 51 | 144 |
| "não pegou na mesa — clicando de novo" | — | 4 (core-live) |
| cliques em (0,0) com `dom_found=false` | 1 (17/11) | 10 |
| "alvo não resolvido — clique abortado" | 2 | 10 |

Os 5 `cover_mismatch` mais recentes têm todos `lido` muito acima do `esperado`
(`col:1 lido=660 esperado=180 ms=8745`, `col:3 lido=700 esperado=120 ms=10038`):
é ficha do operador, não ficha engolida. E o `ms` mostra o custo: 6 a 10 s por entrada.
As `extrato_divergence` vêm do mesmo motivo (o saldo também mexe com a aposta do operador).

## 2. Calibração

`config/calibration.toml` está **desatualizado e não é usado na prática**: o sidecar resolve
o alvo no DOM (`click_resolved`) e só clica se achar; sem achar, aborta (409).

| alvo | toml | resolvido no pano (moda) |
|---|---|---|
| col:1 / 2 / 3 | 875,760 / 875,710 / 875,660 | 909,593 / 909,558 / 909,522 |
| chip 10 / 20 / 50 / 100 | 955,945 / — / 1045,945 / 1090,945 | 947,683 / 980,683 / 1013,683 / 1046,683 |
| clear | 835,940 | 599,684 |

A resolução é estável: >95% dos cliques de cada alvo caem no mesmo pixel; as variantes são
de layouts diferentes (outra resolução de janela). Recomendação: atualizar o toml para os
valores resolvidos só como documentação, sem efeito no comportamento — não é prioridade.

Os 10 cliques em `(0,0) dom_found=false` são o log do fallback antes do aborto (o sidecar
não clica sem `resolved`); nenhum chegou ao pano. Os plenos que falham são sempre
`straight:*` — a varredura de texto do pano às vezes não encontra o número.

## 3. Pausas e esperas

| parâmetro | onde | valor | veredito |
|---|---|---|---|
| pausa entre cliques | `sidecar/cmd/capture/main.go:340` | 220 ms | **manter**. Ficha engolida só reapareceu 4 vezes no log todo. |
| jitter do press/release | `sidecar/cdp/click.go:55` | 14–28 ms | ok |
| espera de "FAÇA SUAS APOSTAS" | `click.go` `isRoundOpenNow` | até 75×150 ms = 11,25 s | **problema**: não sabe em que rodada está; clique atrasado vai para a rodada seguinte |
| timeout `click_resolved` | `main.go:306` | 12 s | grande demais para uma janela de 21 s |
| `TABLE_CLEAR_WAIT_MS` | `executor.rs:718` | 8 s | ver P3 |
| `SETTLE_DEADLINE_MS` / `COVER_RECHECK_MS` / `BALANCE_CONFIRM_MS` | `executor.rs:711-716` | 1,2 / 2 / 1,5 s | **é aqui que os 5 s por cobertura somem** (P1) |

## 4. Propostas (nenhuma aplicada — dependem do OK do operador e de `--dry-run`)

**P1 — não esperar quando o bilhete já passou do esperado (maior ganho).**
O bot nunca tira ficha (ADR 0007), então se o bilhete leu acima do acumulado esperado ele
nunca vai "descer" até o esperado. Hoje `read_settled_for` espera o prazo inteiro (1,2 s),
depois `balance_confirms` espera mais 1,5 s, que também falha porque o saldo mexe com a
aposta do operador. Proposta: em `read_settled_for`, sair na hora quando `seen > expected`;
e pular `balance_confirms` nesse caso. Ganho estimado: ~2,7 s por cobertura quando o operador
aposta junto (30% das rajadas do 17/11). Risco baixo: não muda o que é clicado, só quando.

**P2 — prazo de rodada no executor (segurança, não só agilidade).**
Passar ao `place()` o instante do resultado (`ts` do spin) e parar de clicar quando faltar
menos de 2 s para o fechamento (~21,1 s depois do resultado, ou seja ~19 s): o que entrou
fica, registra `window_closed` e a conferência de extrato faz o resto. É o §1.12 aplicado
dentro da entrada, e não só no gate. Hoje há evidência de fichas da entrada caindo na rodada
seguinte. No sidecar, baixar o timeout de `click_resolved` de 12 s para ~3 s pelo mesmo motivo.

**P3 — `TABLE_CLEAR_WAIT_MS` 8 s → 2 s.**
A espera serve para a animação de fim de rodada sumir. Como a fita chega ~3,9 s depois do
resultado e a mesa abre ~0,28 s depois dele, quando o bot age as fichas da rodada anterior
já sumiram: o que sobra no pano é ficha nova do operador, e esperar 8 s por ela é perda pura.
O log de hoje não tem nenhum "já no pano (fichas do operador)" desde o ADR 0007, então o
efeito hoje é pequeno; vira importante quando a fonte `ws` for ligada (bot ~3,9 s mais cedo,
pegando a animação). Validar em `--dry-run` com o `ws` ligado antes de mudar.

**P4 — plenos não resolvidos.** 10 abortos de `straight:*` sem coordenada. Investigar na
fixture do DOM qual número a varredura de texto não acha (12, 15, 26, 28, 33, 35, 36, 0
aparecem) — propor resolução pela geometria das âncoras `2to1` em vez do texto.

**P5 — calibration.toml** atualizado para os valores resolvidos (só documentação).

**P6 — seleção de ficha agrupada por denominação (pedido do operador em 18/11).**
Hoje a decomposição é feita cobertura a cobertura: 180¢ em col:1 + col:2 vira
`100→col1, 50→col1, 20→col1, 10→col1, 100→col2, 50→col2, 20→col2, 10→col2`
(22:02 de 17/11): 8 trocas de ficha para 8 fichas. Agrupando pela denominação entre as
coberturas (`100→col1,col2 · 50→col1,col2 · 20→… · 10→…`) são 4 trocas, cerca de 1,2 s a
menos por entrada e menos chance de a mesa ficar com a ficha errada selecionada. Dá para ir
além: escolher a decomposição que minimiza `trocas de ficha + cliques` (60¢ = `20×3`, uma
troca, em vez de `50+10`, duas trocas). A conferência por cobertura (§1.4) passa a ser
conferência por denominação, com os prefixos legítimos recalculados. Muda o executor:
só com OK e `--dry-run`.

Ordem sugerida: P1 → P6 → P2 → P3 (este junto com a ativação do `ws`). Nenhuma mexe em
estratégia ou escada.
