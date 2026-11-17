# ADR 0007: O bot nunca retira fichas da mesa

- **Status:** aceito (2026-11-18), decisão do operador. Substitui o §1.4 (`clear_bets()` em divergência) e amplia o ADR 0006.

## Problema

O operador aposta junto com o bot (por exemplo, dobrando a ficha ou cobrindo o 0). O executor tratava
qualquer ficha a mais no bilhete como "aposta errada do bot" e fazia UNDO, **tirando as fichas do operador**.
Além disso, o bilhete da Playtech às vezes lê errado (0 com R$ 11,00 no pano, 120 com 40 colocados), e o UNDO
desfazia entradas corretas.

## Decisão

- **Nenhum caminho do executor chama UNDO.** O `clear_bets()` do executor virou no-op (`OwnShare`).
- O que já está no pano antes da entrada é do operador. O bot confere **só a própria parte**: bilhete menos essa base.
- **Ficha a mais no bilhete** é do operador e fica onde está.
- **Ficha do bot faltando:**
  - por fora, o bot reclica a cobertura que não pegou, uma vez;
  - depois disso, segue com o que entrou e registra `partial_entry` ou `number_partial`.
- **Erro de clique (409/502):** o que já entrou fica, e o erro é registrado como `click_unresolved`.
- **Saldo como segunda testemunha:** se a queda do saldo bate com a aposta, vale o saldo, mesmo com o bilhete divergente.

## Consequências

- **Aposta a mais do próprio bot não é mais desfeita.** O risco foi reduzido antes desta decisão: não há mais "completar na marra" (desde 14/11), e o reclique é único e só por cobertura.
- A conferência de extrato (`[EXTRATO-DIVERGENTE]`) é o registro de qualquer diferença.
- O diagnóstico de cada entrada vai para o log com o prefixo `[EXEC-DIAG]`.
