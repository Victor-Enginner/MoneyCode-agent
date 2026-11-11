# Números e terminais — o que falta para o agente apostar em número

Levantamento feito no código e nos markdowns do repositório. Serve como especificação
do que existe, do que **não** existe e do que é obrigatório construir antes de o agente
entrar em cobertura de número (pleno, terminal/final, vizinhos).

## 1. O que os markdowns do projeto têm hoje

| Arquivo | Linhas | Assunto | Fala de número/terminal? |
|---|---|---|---|
| `AGENTS.md` | 275 | contrato do sistema (§5.2 já cita `terminal` na lista de coberturas) | só a palavra no contrato |
| `docs/FUI.md` | 150 | painéis da UI | não |
| `docs/LINKS.md` | 161 | links de referência | não |
| `docs/ECOSYSTEM_RADAR.md` | 78 | ideias de backtest (χ² por setor, runs) | não |
| `docs/oplog.md` | 61 | formato do protobuf do WS (`split`/`splitBet`) | não |
| `docs/adr/0001`, `0002` | — | sqlx offline / toolchain | não |
| `README.md`, `TODO.md` | 12 / 8 | banner e lista curta | não |

Resultado do grep por `terminal|final|vizinh|voisin|pleno|straight|cavalo|setor`: nenhuma
linha descreve aposta de número. Onde aparece "terminal" é o console do Windows; onde
aparece "setor" é um matcher de coluna/dúzia com nome de setor.

**Conclusão:** a especificação de aposta em número não existe em markdown nenhum. O
`AGENTS.md` §5.2 lista `terminal` como tipo de cobertura, mas nada define semântica,
stake, resolução de clique nem limites — é exatamente o buraco que este arquivo cobre.

## 2. O que o código já tem

- `Cover::Terminal(u8)` e `Cover::Straight(u8)` existem em `crates/strategy-native`.
- `canonical_key()` gera `terminal:{t}` / `straight:{n}`; `display_label()` já os rotula.
- `Cover::matches_number` acerta a regra da roleta para todos os tipos: `Terminal(t)` vence
  quando `número % 10 == t`, `Straight(n)` quando `número == n` (usado pelo `reconciler`).
- `reconciler.rs` já ordena o pagamento de `Terminal` acima de coluna/dúzia.

## 3. O que faltava (e o que já foi resolvido em 2026-11-11)

1. ~~**Nenhum matcher emite `Terminal` ou `Straight`.**~~ Resolvido: `ClusterFinal`
   (`matcher.cluster_final`) emite `Terminal(0)` + os três finais da família ativa. O
   `cluster_terminal` antigo continua cobrindo duas colunas (nome de número, comportamento de
   coluna) e foi mantido por compatibilidade de histórico.
2. ~~**Não há alvo de clique por número.**~~ Resolvido: `straight:N` em `sidecar/cdp/click.go`,
   resolvido pela grade 3×12 derivada das âncoras `2to1`; grade incompleta → `found:false` →
   409, nunca clique cego. Coberto por `resolver_spec` (`straight:17`, `straight:1`,
   `straight:36` e o caso do número ausente).
3. ~~**Não há coordenada de número na calibração.**~~ Resolvido: `Calibration::coords` devolve
   `(0,0)` para pleno/terminal e delega ao DOM; os outros tipos continuam exigindo calibração.
4. ~~**A escada do §6 não vale para número.**~~ Resolvido: `monster_risk::NumberLadder`
   (`x = ceil((D+G)/(36−k))`, arredondado à ficha) + `authorize_numbers` com a mesma
   precedência do gate de colunas. `stake_cents` (×3) continua exclusiva de coluna/dúzia/cor.
5. ~~**`payout_multiplier(Terminal) = 8` só fecha por coincidência.**~~ Resolvido por
   construção: o `slip.compiler` expande `Terminal(t)` em plenos antes do executor, então
   `fills` só contém `Straight` (35:1). O multiplicador 8 virou caminho morto.
6. ~~**Não existe cartão de estratégia.**~~ Resolvido: `corpus/cards/cluster_final.yaml` com
   hipótese, cobertura, escada, gates de backtest e modos de falha.

### O que continua faltando

- Detecção de **troca de regime** de verdade (o matcher usa streak + dominância, não um
  changepoint bayesiano): ver `docs/estudo-arxiv-clusters.md`.
- Teste de **permutabilidade/e-value** antes de autorizar `--live`.
- Backtest real nos 500 giros (o cartão define as métricas; o número ainda não existe).
- Overlay de cobertura de número na FUI (hoje desenha coluna/dúzia).
- `corpus/pdf/` vazio: os PDFs de origem não estão no repositório.

## 4. Matemática que tem de substituir a escada ×3

Terminal/final `t` = todos os números terminados em `t`:

| final | números | k |
|---|---|---|
| 0 | 0, 10, 20, 30 | 4 |
| 1..6 | t, t+10, t+20, t+30 | 4 |
| 7, 8, 9 | t, t+10, t+20 | 3 |

Uma ficha `x` em cada número do terminal → exposição `k·x`; acerto paga `36x` (35:1 no
número sorteado); P(acerto) `= k/37`.

Para recuperar perda acumulada `D` e lucrar `G`:

```
x = (D + G) / (36 − k)     →  k=4: x = (D+G)/32   ·   k=3: x = (D+G)/33
```

Compare com o §6: cobrir 2 colunas custa `2x` e paga `3x` (recupera `D+G` com `x = D+G`).
Número não recupera com ×3; recupera com divisor `36−k`. **Reaproveitar `stake_cents` para
terminal é bug de dinheiro**, não de estilo.

Stakes têm de continuar inteiros em centavos e decomponíveis nas fichas
(`0,10 / 0,50 / 1 / 5 / 10 / 25`); `x` cai no centavo e a sobra não pode virar aposta
fantasma — arredondar para cima e registrar o total real no plano.

## 5. Checklist

- [x] **Expansão de cobertura** no `slip.compiler`: `Terminal(t)` → `[Straight(n) …]`, ficha de
      cada número por `NumberLadder`. Plano continua `covers: Vec<(Cover, u64)>`.
- [x] **Resolver `straight:N`** no `click.go`, derivado da grade 3×12 das âncoras `2to1`.
- [x] **Fallback de calibração** para `straight:*` com delegação ao DOM e 409 quando não resolve.
- [x] **Escada por pagamento**: `NumberLadder` + `authorize_numbers`, com teste de propriedade
      (`lucro no acerto ≥ base` para `k = 3..=15`, todos os degraus) e teste dedicado
      `number_ladder_does_not_reuse_the_x3_ladder`.
- [x] **Limite de mesa por número**: `steps_within_limit(max_bet_per_cover_cents)` diz até onde
      a escada cabe; o gate aborta com `TableLimit` acima disso.
- [x] **Matcher de número**: `cluster_final` emite `Terminal(0)` + família, com teste de que a
      cobertura é de terminal (nunca coluna).
- [x] **Cartão em `corpus/cards/`**: `cluster_final.yaml`.
- [x] **Teste de integração** do caminho de números: `fase5_cluster_final_aposta_em_numeros_com_escada_propria`
      (15 plenos, preview somando, acerto/erro e degrau subindo 1→2→3).
- [ ] **FUI**: overlay de cobertura para número/terminal e tabela da escada ciente do tipo.
- [ ] **Backtest de 500 giros** com χ² por final e e-value (o cartão define; o número não existe).
- [ ] **Monte Carlo** com a escada de números antes de `--live` (§13).

## 6. Vocabulário (para não confundir na UI e no código)

- **final / terminal** — último dígito do número (grupo de 3 ou 4 números).
- **pleno / straight** — um único número, 35:1.
- **cavalo / split** — 2 números vizinhos na grade, 17:1.
- **rua / street** — 3 números de uma linha, 11:1.
- **quadra / corner** — 4 números, 8:1.
- **vizinhos / voisins** — números adjacentes **no aro físico**; na Playtech a aposta é feita
  sobre a grade, então "vizinho" só existe se a mesa oferecer call bet. Não implementar antes
  de confirmar na mesa — clicar em número adjacente de grade **não** é vizinho de roda.

## 7. Ordem sugerida

1. ~~`straight:N` no resolver + calibração~~ — feito (ADR 0003).
2. ~~Escada por pagamento + testes~~ — feito; falta o MC de 10.000 sessões com `k=15`.
3. ~~Expansão `Terminal → Straight` no plano~~ — feito.
4. ~~Matcher + cartão~~ — feito; falta o backtest de 500 giros.
5. FUI e gate de replay com números reais.
