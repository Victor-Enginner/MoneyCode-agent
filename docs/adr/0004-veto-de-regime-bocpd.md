# ADR 0004 — Veto de regime por BOCPD e Monte Carlo da escada de números

- **Status:** aceito (2026-11-12)
- **Contexto:** §8 (regime.classifier), §13 (Monte Carlo obrigatório), §15.4.

## Problema

1. A escada de números (`NumberLadder`, ADR 0003) nunca passou pelo gate de ruína do §13: o
   `mc.rs` existente simula duas colunas (exposição `2x`, 13 pockets perdedores). Números têm
   `k·x` de exposição e 22 pockets perdedores — a cauda é outra ordem de grandeza.
2. O `regime.classifier` do §8 mede razão de runs de coluna. Ele diz se a mesa está repetitiva,
   mas não diz **há quanto tempo** o bloco de finais começou nem se ele acabou de trocar. O
   matcher de cluster compensa com `streak ≥ 4` + dominância ≥ 55%, que só olham o fim da fila.

## Decisão

- **Novo crate-path:** `crates/risk/src/mc_numbers.rs` — `run_monte_carlo_numbers`, com a escada
  e o `authorize_numbers` reais, 37 pockets equiprováveis (`u % 37 < k`, exato, sem float no
  dinheiro). Relatório: `p_blow_per_cycle`, `p_ruin`, `drawdown_p95_cents`,
  `max_exposure_cents`, `ev_per_hour_cents`, `gate_passes()` com `P_RUIN_GATE = 5%`.
- **Novo crate-path:** `crates/core/src/bocpd.rs` — BOCPD (`arXiv:0710.3742`) em replay puro da
  história: posterior sobre o comprimento do segmento, Dirichlet-multinomial por hipótese,
  hazard `1/18`, truncamento em 120. Publica `p_change`, `map_run_length`, `family_share`,
  `map_family`. Sem estado mutável: reinício não desalinha.
- **Integração:** `evaluate_dag*` calcula a posterior e aplica `apply_regime_veto` aos sinais de
  cluster com cobertura de terminal. Veto = `confidence = 0.0` + `regime_veto = true`; o sinal
  permanece no stream de decisões com motivo (`JustChanged` | `FamilyFaded`).
- **Escopo do veto:** `JustChanged` quando `p_change ≥ 0,20` (medido: mesa estável ≈ 0,02,
  troca real ≈ 0,37); `FamilyFaded` quando a cobertura é de família diferente da que o regime
  aponta, ou quando a preditiva do segmento está abaixo de 0,40. Coleta de coluna/dúzia/cor
  nunca é vetada (não é cobertura de família).
- **Nada muda em `Cfg` nem no schema do §5.** `BocpdCfg::default()` é o único ponto de ajuste;
  `BocpdReport`/`regime_veto` são campos aditivos com `#[serde(default)]`.

## Consequências

- O boot imprime o MC de números e o veredito do gate; com banca de R$ 50,88 e `max_steps=4` o
  gate **reprova** (P(ruína) ≈ 19%) — decisão de banca/stop é do operador, não do código.
- O veto não substitui o teste de permutabilidade (e-value) do estudo: BOCPD detecta mudança de
  distribuição, não prova que exista estrutura. Um streak de 4 *é* evidência de segmento novo —
  por isso o veto não tenta barrar todo cluster, apenas o que está fora de fase com o regime.
