# Mapeamento do Frame de Aposta e Resultado (`oplog.md`)

## 1. Whitelist de Sockets (Filtro por `requestId`)

Apenas os sockets cujo URL decodificado (`url.QueryUnescape`) contenha um dos dois feeds oficiais da Playtech são processados pelo sidecar:

| Papel | Host | Status |
|---|---|---|
| **Primário** | `eurolive-frontend.playtechgaming.com` | Ativo (autoridade em `dual_feed=true`) |
| **Standby** | `ielive-gateway.ptielive.com` | Failover (descartado quando o primário entrega na janela `< 3000 ms`) |
| Ignorados | `tawk.to`, `api6.abrws.club`, `jogatudo.com/ws` | Descartados no `Network.webSocketCreated` |

---

## 2. Árvore Protobuf de Resultado (`Server → Client`)

Caminho confirmado em `21/21` rodadas com valores distintos: **`/3/103/1/1`**.

```text
1 (/1): str "pt.live.game/..."           # Tipo da mensagem no envelope Playtech
2 (/2): varint ??                        # Correlation / sequence ID do envelope (??)
3 (/3): msg                              # Payload principal da mesa
  11 (/3/11): varint <round_id>          # Candidato a spin_id monotônico da rodada (confirmar em --dump-frames=3)
  103 (/3/103): msg                      # Bloco de resultado da Roleta Brasileira
    1 (/3/103/1): msg
      1 (/3/103/1/1): varint <0..36>     # NÚMERO SORTEADO (confirmado 21/21 exatos sem offset)
      2 (/3/103/1/2): varint ??          # Cor / setor / metadado do pocket (??)
```

---

## 3. Árvore Protobuf de Pedido de Aposta (`Client → Server` via `Network.webSocketFrameSent`)

Capturado em `logs/oplog.bin` e impresso no terminal com `--dump-op`:

```text
1 (/1): str "pt.live.game/PlaceBetsRequest" (?? confirmar string exata nas 5 apostas manuais)
2 (/2): varint ??                           # Sequence / request counter do cliente (??)
3 (/3): msg                                 # Payload do pedido de aposta
  1 (/3/1): varint ??                       # ID monotônico da rodada / table session (??)
  2 (/3/2): msg                             # Lista de coberturas apostadas (BetSlip)
    1 (/3/2/1): varint ??                   # Código da cobertura (Coluna 1/2/3, Dúzia, Straight) (??)
    2 (/3/2/2): varint <stake_cents>        # Valor da aposta em centavos inteiros (ex.: 10 = R$ 0,10) (??)
```

---

## 4. Procedimento de Captura Canônica (5 Apostas Manuais de R$ 0,10)

Com o Chrome aberto na mesa da Roleta Brasileira (`jogatudo.com`), execute o `start-probe.bat` (ou rode manualmente):

```powershell
cd "O:\Archives\Eistein\FUI AGENT ROULLET\sidecar"
go run ./cmd/capture --ws-probe --strict --dump-frames=3 --dump-op
```

Para congelar o caminho `/3/103/1/1` em `config/schema.toml`:

```powershell
go run ./cmd/capture --freeze=/3/103/1/1 --ws-probe --strict --dump-frames=3 --dump-op
```
