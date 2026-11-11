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

## 3. O que o código NÃO tem (e por isso hoje não é possível apostar número)

1. **Nenhum matcher emite `Terminal` ou `Straight`.** `grep Cover::Terminal` só encontra
   `backtest.rs` e `reconciler.rs`. O `cluster_terminal` conta final (`n % 10`) mas aposta
   **duas colunas** — nome de número, comportamento de coluna.
2. **Não há alvo de clique por número.** `sidecar/cdp/click.go` resolve `col`, `col2to1`,
   `dozen`, `low/high`, `even/odd`, `red/black`, `clear`, `chip`. Não existe `straight:N`
   nem `terminal:N`; qualquer um cai em `found:false` e o sidecar responde **409**.
3. **Não há coordenada de número na calibração.** `config/calibration.toml` tem só
   `col:*` e `dozen:*`; `Calibration::coords()` devolve `MissingCalibration` para
   `straight:*`, então `place()` aborta antes de clicar.
4. **A escada do §6 não vale para número.** `stake_cents` foi derivada para cobertura 2:1
   (duas colunas: exposição `2x`, ganho `x`, perda `2x`). Número pleno paga 35:1 e terminal
   de k números custa `k` fichas por rodada — a progressão ×3 está errada nesse pagamento.
5. **`payout_multiplier(Terminal) = 8` só fecha por coincidência.** Em `reconciler.rs` o
   multiplicador de `Terminal` é fixo em 8. Isso acerta apenas o terminal de 4 números visto como
   exposição de `4x` (lucro `32x`). Para k=3 o correto é `11x` por unidade exposta (lucro `33x`), e
   se a cobertura for expandida em plenos o multiplicador tem de sair de `36/k`, não de uma tabela
   fixa. Hoje ninguém chega nesse caminho porque nada emite `Terminal`, mas ao ligar o matcher é
   aqui que o P&L passa a mentir.
6. **Não existe cartão de estratégia.** `corpus/cards/` não existe (só `corpus/knowledge_graph.json`).
   Sem cartão não há chi², EV nem walk-forward do sinal de número.

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

## 5. Checklist do que precisa ser construído

- [ ] **Expansão de cobertura** em `slip.compiler`: `Terminal(t)` → `[Straight(n) …]`, cada
      um com a ficha calculada por `x = (D+G)/(36−k)`. O contrato §5.2 já permite; o plano
      continua com `covers: Vec<(Cover, u64)>`.
- [ ] **Resolver `straight:N`** no `click.go` derivando a grade 3×12 do próprio pano a partir
      das âncoras `2to1` que já existem (linha = coluna do número, coluna = faixa da dúzia).
      Nenhuma coordenada fixa nova: a geometria já é derivada hoje.
- [ ] **Fallback de calibração** para `straight:*` (`config/calibration.toml`) quando a mesa
      não expuser as âncoras — e erro duro, nunca clique cego, se nem isso resolver.
- [ ] **Escada por pagamento**: `stake_cents_for_payout(k, D, G)` com testes de propriedade
      (acerto devolve exatamente `G`) e Monte Carlo de 10.000 sessões antes de `--live`
      (gate do §13).
- [ ] **Limite de mesa por número**: validar `x` e `k·x` contra o limite por casa
      (R$ 500–2.000) no boot, como o §2 exige.
- [ ] **Matcher de número**: emitir `cover: [Terminal(t)]` com confiança, sem inventar sinal —
      hoje `cluster_terminal` mede final e aposta coluna; decidir se vira matcher de terminal
      ou se nasce um `matcher.terminal_final` novo.
- [ ] **Cartão em `corpus/cards/`** com backtest de 500 giros: taxa de acerto, χ² do desvio por
      final, EV e banda de confiança (senão é aposta sem evidência).
- [ ] **FUI**: overlay de cobertura para número/terminal (hoje desenha coluna e dúzia) e tabela
      da escada ciente do tipo de cobertura.
- [ ] **Teste de replay** com terminais no pipeline: 500 giros, zero `cover_mismatch`, zero
      órfão — o mesmo gate da Fase 5, agora com `k` cliques por cobertura.

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

1. `straight:N` no resolver + calibração (sem isso nada de número chega na mesa).
2. Escada por pagamento + testes + MC.
3. Expansão `Terminal → Straight` no plano.
4. Matcher + cartão com backtest.
5. FUI e gate de replay.
