# ADR 0004 — Cobertura de número: expansão de `terminal` em plenos e escada por pagamento

- Status: aceito
- Data: 2026-11-11
- Contrato afetado: §5.2 (`cover`), §6 (escada), §7 (clique)

## Contexto

O §5.2 lista `terminal` como tipo de cobertura, mas nada definia como ela chega à mesa.
`Cover::Terminal(t)` existe desde o início e `matches_number` já sabia que `t` vence em
`n % 10 == t`; nenhum matcher emitia, `click.go` não tinha alvo para número e a única
escada implementada (`stake_cents`, ×3) foi derivada para duas colunas (exposição `2x`,
retorno `3x`).

Havia ainda um bug silencioso: `matches_number` exigia `number != 0` para terminal, o que
excluía o próprio `0` do final `0` — justamente o pocket que o operador inclui em todos
os clusters.

## Decisões

1. **`Terminal(t)` é expandido em plenos no `slip.compiler`.** O plano que chega ao
   executor contém apenas `Straight(n)`, um por número coberto. O executor, o preview,
   o `fills` e a UI não sabem (nem precisam saber) que a entrada nasceu de um terminal.
2. **A escada de número é `x = ceil((D + G) / (36 − k))`, arredondada à ficha mínima.**
   Implementada em `monster_risk::NumberLadder` e autorizada por `authorize_numbers`,
   com a mesma precedência do gate de colunas (StopLoss → LadderBlown → TableLimit →
   Exposure → Cooldown → Window). A escada ×3 permanece exclusiva de coluna/dúzia/cor.
3. **Nenhuma coordenada nova de calibração.** `straight:N` é resolvido no pano: o sidecar
   deriva a grade 3×12 do mesmo retângulo que já deriva de `2to1` e clica no rótulo do
   número. Grade incompleta (< 30 números distintos detectados) → `found:false` → 409,
   nunca clique cego.
4. **`Calibration::coords` devolve `(0,0)` para pleno/terminal** e delega a resolução ao
   sidecar; qualquer outro tipo sem coordenada continua sendo `MissingCalibration`.
5. **O `0` é curinga de família**: não pertence a `1-4-7`, `2-5-8` nem `3-6-9`, entra em
   toda cobertura e não interrompe a sequência.

## Consequências

- `payout_multiplier(Terminal) = 8` deixa de ser usado no caminho de `fills`, porque não
  existem mais acks de terminal — só de pleno (35:1).
- Uma entrada de cluster 1-4-7 + 0 são **15 cliques** e 15 leituras de preview. O
  orçamento de 3000 ms de clique precisa cobrir isso; o executor já aceita atraso de slip
  e valida o total final.
- O limite de mesa é checado na ficha por número; a exposição, em `k·x`.
- Toda estratégia nova de número precisa declarar `k` (números distintos) no cartão: sem
  isso não há como calcular exposição nem ficha.
