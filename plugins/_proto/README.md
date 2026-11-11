# plugins/_proto — contrato NDJSON dos plugins de estratégia

Um plugin é um processo Python 3.11+ que **só decide**: nunca clica, nunca abre socket,
nunca lê o banco. O Rust (`crates/strategy-runtime`) fala por stdio, uma linha JSON por
mensagem, e mata o processo em 120 ms se ele não responder (§5.2, §10).

## Rust → plugin

```json
{"v":1,"t":"eval","spin_id":"ws-1730000000000-812","spins":[17,4,21,9],"ladder_step":1}
```

- `spins` vem **mais recente primeiro** (é o `feat.window(30)` sem `reverse`).
- `ladder_step` é o degrau que a estratégia jogaria agora (1-based).

## plugin → Rust

```json
{"v":1,"strategy_id":"cluster_final","cover":[{"terminal":0},{"terminal":1},{"terminal":4},{"terminal":7}],"confidence":0.63}
```

Sem sinal:

```json
{"v":1,"strategy_id":"cluster_final","cover":[],"confidence":0.0,"reason":"family_share_low"}
```

`cover` vazio é "não entra"; nunca é erro. Números em centavos não aparecem aqui — quem
calcula ficha é o Rust (`monster_risk::NumberLadder`), o plugin não decide dinheiro.

## Tipos de `cover` aceitos

`{"col":1..3}`, `{"dozen":1..3}`, `{"red"/"black"/"low"/"high"/"even"/"odd":null}` no
formato serde de `Cover` (chave = nome da variante em minúsculas), `{"straight":0..36}`
e `{"terminal":0..9}`. `terminal` é expandido em plenos pelo `slip.compiler` (ADR 0003).
