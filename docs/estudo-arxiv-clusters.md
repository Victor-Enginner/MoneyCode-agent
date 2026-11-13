# Estudo arXiv — clusters de terminais, troca de regime e quando o padrão é real

Este documento responde a uma pergunta só: **o que existe de literatura séria para transformar
"a mesa fica em 1-4-7 e depois troca para 3-6-9" em código com número, e o que é autoengano.**
Todo paper abaixo foi conferido na página `arxiv.org/abs` (IDs reais, autores reais). Onde a
fonte não é arXiv, está marcado.

Leia junto com `docs/numeros-e-terminais.md` (o que falta para apostar número) e
`corpus/cards/cluster_final.yaml` (a especificação da estratégia).

---

## 1. O modelo do operador, formalizado

O que você chama de **cavalo** é uma família de finais:

| família | finais | números |
|---|---|---|
| A | 1, 4, 7 | 1, 4, 7, 11, 14, 17, 21, 24, 27, 31, 34 |
| B | 2, 5, 8 | 2, 5, 8, 12, 15, 18, 22, 25, 28, 32, 35 |
| C | 3, 6, 9 | 3, 6, 9, 13, 16, 19, 23, 26, 29, 33, 36 |

A partição é `final % 3`: A ≡ 1, B ≡ 2, C ≡ 0 (mod 3). O final **0 é curinga**: não pertence a
família nenhuma, entra em toda cobertura e não quebra a sequência. Com o curinga, a cobertura
padrão de uma família é `Terminal(0) + Terminal(f1) + Terminal(f2) + Terminal(f3)` = **15
números**, que é o que `cluster_final` emite hoje.

> **Ponto em aberto:** na sua mensagem os grupos vieram como `(3,6,9,2)`, `(1,4,7,8)` e
> `(2,5,8,4)` — o quarto final não sai da partição mod 3 e não é o `0` (que você já disse
> entrar em todos). O matcher implementado usa mod 3 + curinga 0. Se esse quarto número for
> "o final que costuma vir junto/na troca", ele precisa de definição explícita antes de entrar
> no código; hoje ele não está lá.

### Quanto de desvio seria necessário para ter lucro

Cobrir 15 números com `x` em cada: custa `15x`, recebe `36x` no acerto.

- P(acerto) uniforme = 15/37 = **0,40541**
- P(acerto) de equilíbrio  = 15/36 = **0,41667**

Ou seja: a cobertura precisa ser sorteada **1,13 ponto percentual acima** da frequência
uniforme (≈2,8% relativo) só para empatar. Isso não é "um pouco acima": é uma borda de 2,7% da
casa dividida pela cobertura.

Quantos giros provam isso? Para detectar `d = 1,13pp` com 80% de poder e α = 5%:

```
n ≈ (z_{α/2} + z_{β})² · p(1−p) / d²  =  (1,96 + 0,84)² · 0,4054·0,5946 / 0,0113²  ≈ 15.000 giros
```

Com 500 giros você só enxerga desvios ≥ 4,4pp (2 erros-padrão). **Uma sessão de 200 giros não
prova nada** — prova ruído. É esse número que o cartão exige antes de `--live`.

---

## 2. Os papers, e onde cada um entra

### 2.1 Detectar a troca de família: changepoint online

**Bayesian Online Changepoint Detection** — Adams & MacKay, 2007. `arXiv:0710.3742`
[abs](https://arxiv.org/abs/0710.3742)

Posterior exato sobre "há quanto tempo não há mudança" (run length), por message passing.
Entrega `P(run_length = r | dados)` a cada giro, com o modelo de emissão plugável.

- **Resolve:** exatamente o seu problema — "a mesa ainda está em 1-4-7 ou já trocou?".
- **Onde entra:** `crates/core` como feature do `regime.classifier`; versão pesada no plugin
  Python (`plugins/cluster_final/`), publicando um multiplicador de confiança.
- **Custo:** HMM de 3 estados categóricos 1-de-10, `O(30 · 3)` por giro. Microssegundos.
- **Por que é melhor que "streak ≥ 4":** o streak é uma regra fixa; a run length dá a
  probabilidade de a família ter mudado, e é isso que evita continuar apostando depois da
  troca (o pior modo de falha da estratégia).

### 2.2 Modelar a duração do bloco: semi-Markov

**Bayesian Nonparametric Hidden Semi-Markov Models** — Johnson & Willsky, 2012. `arXiv:1203.1365`
[abs](https://arxiv.org/abs/1203.1365)

O HDP-HSMM troca o decaimento geométrico do HMM por **distribuições de duração explícitas** por
estado, sem fixar o número de estados. É a formalização de "a mesa fica *muito tempo* numa
família": duração não geométrica, com prior.

- **Resolve:** quantificar "bloco longo" em vez de assumir. A duração do bloco é o que decide
  se vale 15 fichas por rodada ou se o cluster já morreu.
- **Onde entra:** offline/backtest (Python), gerando a duração média por família que vai para
  o cartão; online só o resumo (mediana das últimas N durações).
- **Custo:** Gibbs sampling — **nunca** no caminho crítico. Roda em lote sobre o SQLite.

### 2.3 Testar se o padrão é real, sem se enganar

**Retrain or not retrain: Conformal test martingales for change-point detection** — Vovk et al.,
2021. `arXiv:2102.10439` [abs](https://arxiv.org/abs/2102.10439)
**Conformal testing: binary case with Markov alternatives** — Vovk, Nouretdinov, Gammerman,
2021. `arXiv:2111.01885` [abs](https://arxiv.org/abs/2111.01885)
**Conformal e-testing** — Vovk, 2020/2024. `arXiv:2006.02329` [abs](https://arxiv.org/abs/2006.02329)

Teste **online** da hipótese "a sequência é i.i.d./permutável" via martingales (e-values). Se a
sequência é justa, o capital do martingale não cresce; se cresce de forma consistente, há
evidência de estrutura — com validade sequencial (Ville: `P(sup S ≥ c) ≤ 1/c`).

- **Resolve:** o anti-overfitting. Rodar chi² em 500 giros e achar `p < 0,05` é fácil quando se
  testam 3 famílias × vários limiares; e-value/martingale controla isso por construção.
- **Onde entra:** job de análise (Rust ou Python) que lê `spins` e responde uma única pergunta:
  *"rejeito a hipótese de mesa justa para a família X?"* Se não rejeitar, `cluster_final` fica
  em `dry-run` por definição.
- **Regra prática:** só considerar borda com `e-value ≥ 20` sustentado (equivale a α = 5%
  sequencial) e sem decaimento nos 200 giros seguintes.

### 2.4 Prever o próximo número sem inventar complexidade

**On Prediction Using Variable Order Markov Models** — Begleiter, El-Yaniv, Yona, 2004/2011.
`arXiv:1107.0051` [abs](https://arxiv.org/abs/1107.0051)

Compara CTW, PPM e PSTs em log-loss. Conclusão útil: modelos de ordem variável com mistura
(CTW decomposto / PPM) vencem, e a métrica certa é **log-loss**, não acurácia de acerto.

**Context Tree Switching** — Veness, Ng, Hutter, Bowling, 2011. `arXiv:1111.3182`
[abs](https://arxiv.org/abs/1111.3182)

Extensão do CTW que mistura sobre uma classe maior de modelos sem custo assintótico extra.

- **Resolve:** responder, com número, "existe dependência de ordem > 0 nesta sequência?".
  Se o log-loss do CTW empata com o de uma moeda (log 10 = 1230 millibits por giro), **não há
  padrão** — e o cluster é ilusão de amostra pequena.
- **Onde entra:** backtest/Python. É o juiz que pode vetar o matcher.
- **Por que importa:** se o modelo de ordem variável não bate a uniforme em log-loss, nenhuma
  regra de streak vai bater — ela só está olhando o passado.

### 2.5 Escolher entre estratégias sem overfitar a escolha

**A Regret-Variance Trade-Off in Online Learning** — van der Hoeven, Zhivotovskiy, Cesa-Bianchi,
2022. `arXiv:2206.02656` [abs](https://arxiv.org/abs/2206.02656)

Pesos exponenciais (Hedge/EWA) com garantia de regret `O(log K)` e análise de variância.

- **Resolve:** hoje o `aggregator.score` pega o maior `confidence` — um "winner takes all" que
  troca de estratégia a cada giro. Hedge mantém uma distribuição de pesos e só migra quando a
  evidência acumulada muda, o que é o comportamento certo para "a mesa trocou de família".
- **Onde entra:** `crates/core/src/dag.rs::fan_in` (substituindo o `sort` + `find`), com estado
  de pesos por `strategy_id` no `ladder_state`/memória da sessão.
- **Custo:** `O(K)` por giro, `K ≈ 10`. Desprezível.

### 2.6 A verdade desconfortável

**Predicting the outcome of roulette** — Small & Tse, 2012. `arXiv:1204.6412`
[abs](https://arxiv.org/abs/1204.6412)

Os autores medem a roda e a bola e mostram retorno esperado ≥ 18% — **por medição física**
(posição/velocidade/aceleração), não por histórico. O texto usa explicitamente o `−2,7%` de uma
aposta não informada como base de comparação, e mostra que até uma inclinação mínima da mesa
cria viés explorável.

- **O que isso significa para nós:** a única borda documentada em roleta vem de **defeito
  físico** (roda desbalanceada, dealer com padrão de lançamento, mesa inclinada). Se ela existir
  na Playtech, ela aparece como desvio **estável e grande** por setor da roda — não como
  sequência de finais. O cluster de terminais, no melhor caso, é um proxy grosseiro de viés de
  setor; no pior, é aposta −EV com disciplina.
- **Consequência de projeto:** o `cluster_final` precisa de um teste de viés **por pocket**
  (chi² nos 37) rodando em paralelo. Se o viés estiver em pockets específicos, cobrir 15 números
  dilui a borda; se estiver em setor do aro, a cobertura certa é de vizinhos, não de finais.

---

## 3. Arquitetura recomendada (respeitando §9 e o orçamento de 250 ms)

```
                 ┌─────────────────────────────── Rust, < 250 ms, caminho crítico
spin ──────────► │ feat.window(30) → matchers nativos → regime → gate → plano
                 └───────────────────────────────┘
                                │ publica spin (já é o fluxo atual)
                                ▼
                 ┌─────────────────────────────── Python plugin, 120 ms, FORA do crítico
                 │ BOCPD (run length por família)     → confiança
                 │ HSMM (duração do bloco)            → expectativa de vida do bloco
                 │ CTW/log-loss                       → "há padrão?" sim/não
                 │ conformal martingale (e-value)     → "posso acreditar?"
                 └───────────────────────────────┘
                                │ tabela `regime_posterior` (família, p, e_value, ts)
                                ▼
        matcher nativo lê o prior do banco e multiplica a confiança (nunca decide sozinho)
```

Regras:
1. O plugin **não clica e não decide dinheiro** (§10). Ele publica probabilidade.
2. Se o e-value cair abaixo do limiar, o multiplicador vira `0` — o matcher para de entrar sem
   precisar de intervenção humana.
3. Tudo é reprodutível: `spins` no SQLite + semente fixa = mesmo posterior. Sem LLM.

---

## 4. O que os markdowns do projeto já cobrem (e o buraco)

Revisão do `MARKDOWN MAIS IMPORTANTE DO PROJETO` (= `docs/LINKS.md`), que é o índice de arsenal:

| Repositório do LINKS | Serve para este problema? | Como usar |
|---|---|---|
| `Victor-Enginner/Roullet-bot-eistein` (§1, **CLONE**) | **sim** — é a base com `strategy/engine.py`, onde a regra "3.6.9.2 nos últimos N" apareceu primeiro | portar a regra para `ClusterFinal` e comparar saída com a nossa; é a fonte da nomenclatura |
| `DataLab-Platform/DataLab` (§4) | **sim** — chi², runs, bootstrap com rigor | calcular os gates do cartão (χ² por final e por pocket) sem escrever estatística à mão |
| `Graphify-Labs/graphify` (§4, **CLONE**) | **sim** — PDF → grafo | `corpus/cards/` estava vazio; é o motor que transforma os PDFs em cartões |
| `semantica-agi/semantica` (§4) | parcial — rastreabilidade de decisão | ligar `intents`/`fills` à cadeia de features que gerou a entrada |
| `langgenius/dify` / `onyx` (§4) | não para decisão — só curadoria de corpus | consulta dos PDFs fora do caminho crítico |
| `featherless-ai/simple-jev`, `laya-coreml` (§7) | não — classificador de endpoint; seria um LLM no caminho | proibido pelo §9 |

**Buraco encontrado:** nenhum item do LINKS cobre *detecção de mudança de regime*, *HSMM*,
*compressão universal* ou *e-values*. O projeto tinha "chi² e runs" (fase 4) e nada sobre
"quando o padrão deixou de valer". É exatamente o que a §2 deste documento preenche.

`Markdown1`, `Markdown 2` e `Markdown 3` já citam `matcher.cluster_terminal  # 3.6.9.2 nos
últimos N` e a contradição permanência × cluster com penalidade 0,85 — mas nenhum deles define
como medir a troca de regime, e nenhum aponta para literatura.

---

## 5. Ordem de implementação (com gate em cada passo)

| # | Passo | Gate |
|---|---|---|
| 1 | ~~BOCPD de 3 estados sobre finais em Rust~~ — feito (`crates/core/src/bocpd.rs`, ADR 0004) | p95 do DAG continua < 400 ms (medido ao vivo: 3,8–6,6 ms) |
| 2 | e-value / martingale de permutabilidade sobre os spins gravados | roda em 10 mil giros sem falso positivo em sequência sintética uniforme |
| 3 | CTW/PPM em Python como juiz de log-loss | log-loss do CTW ≤ log-loss uniforme − 1% para autorizar `--live` |
| 4 | HSMM offline para duração de bloco → número no cartão | duração média por família com IC 95% |
| 5 | Hedge no `fan_in` no lugar do winner-takes-all | 200 entradas dry-run com menos trocas de estratégia que hoje, mesmo P&L |
| 6 | chi² por pocket (37) em paralelo ao cluster | detectar viés de setor com 500 giros de fixture |

O passo 3 é o que impede o projeto de virar fé: **se o CTW empatar com a moeda, não há padrão
sequencial**, e a decisão correta é não apostar em final nenhum — mesmo que a timeline "pareça"
estar em 1-4-7. Foi para isso que a Fase 5 passou a testar perda de propósito: escada que só
ganha não prova nada.

---

## 6. Anti-padrões que este estudo fecha

- **Chi² com p < 0,05 como licença para apostar.** Com 3 famílias e vários limiares, um p baixo
  sai por acaso; usar e-value/martingale (sequencial, válido a qualquer momento de parada).
- **Treinar em 500 giros e chamar de borda.** São necessários ~15 mil para a borda mínima.
- **"A mesa está quente".** Sem teste de permutabilidade, é o mesmo que apostar em 1-4-7 porque
  saiu 1-4-7.
- **Confundir final com setor da roda.** Vizinho de aro ≠ vizinho de grade (ver
  `docs/numeros-e-terminais.md` §6).
- **LLM no caminho crítico.** O §9 não é burocracia: 250 ms e reprodutibilidade.
