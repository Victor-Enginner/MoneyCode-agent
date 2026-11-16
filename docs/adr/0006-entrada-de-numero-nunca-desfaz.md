# ADR 0006: Entrada de número nunca é desfeita

- **Status:** aceito (2026-11-15), decisão do operador.
- **Contexto:** §1.4 (conferir o bet slip antes de confirmar; divergência > 1 centavo → `clear_bets()` e abortar).

## Problema

Entradas de número têm 19 a 22 plenos clicados em sequência. Em 14–15/11, 20 de 104 entradas de número
foram desfeitas porque de 1 a 7 fichas não pegaram na mesa. O UNDO de 20 fichas leva ~30 s, invade a
rodada seguinte, e o operador perdia a entrada (e o gale) inteira.

## Decisão

- **Acima de `PER_COVER_VERIFY_MAX` coberturas (entradas de número)**, o slip abaixo ou acima do esperado
  **não** dispara `clear_bets()`. A aposta segue como ficou na mesa, com `warn!` e uma linha
  `number_partial` em `errors` (intent, lido e esperado).
- **Entradas por fora** (até 4 coberturas) mantêm o §1.4. Antes de abortar, a cobertura que não pegou é
  clicada de novo, uma única vez.
- A mesa suja antes da entrada (fichas residuais) continua sendo limpa: isso não é desfazer a entrada.

## Consequências

- O `fills` registra o plano inteiro como aceito. Se o número sorteado for justamente um dos que ficaram
  sem ficha, o reconciliador conta vitória onde houve derrota. A conferência de extrato (`[EXTRATO-DIVERGENTE]`)
  acusa a diferença real no mesmo giro.
- **Correção definitiva pendente:** o sidecar ler quais plenos têm ficha no pano e reclicar só os que faltam.
