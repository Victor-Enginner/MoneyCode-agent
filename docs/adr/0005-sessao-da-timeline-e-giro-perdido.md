# ADR 0005: Sessão da TIMELINE e liquidação após giro perdido

- **Status:** aceito (2026-11-13)
- **Contexto:** §5.3 (DDL), §7 (captura), §9 (reconciliação).

## Problema

1. No boot, a janela de padrões e a TIMELINE eram montadas com os últimos giros do banco, de
   qualquer sessão. A fita da mesa só acrescentava o giro do topo. Em 13/11 às 21:13, o bot leu
   "7, 16, 16" (o segundo 16 era de 21:06, da sessão anterior) como 3× coluna 1 e entrou sem
   padrão: na mesa a sequência era 7, 16, 23.
2. Quando o observer reconecta, ele perde um giro. A entrada aberta era liquidada no giro
   seguinte gravado, e não no que a resolveu de fato. Em 13/11 às 21:18 uma entrada em dúzias
   virou "vitória" no 15, mas a mesa resolveu no giro perdido. A conferência de extrato acusou
   -20¢.

## Decisão

- **DDL:** a tabela nova `sessions(id, started_at, reason)` (`migrations/0002_sessions.sql`) é
  idempotente (`IF NOT EXISTS`) e é aplicada em todo `migrate()`. O início da sessão atual é
  `MAX(started_at)`. Nenhuma tabela existente muda.
- **Boot (`ensure_session`):** na primeira fita lida, a sessão continua se o último giro tem
  menos de 10 min e a fita encaixa no histórico. Senão abre uma sessão nova e semeia a fita da
  mesa como base. Até essa decisão, o executor não entra.
- **O que usa só a sessão:** a janela de padrões (`Window30`), a TIMELINE (`get_spins`) e os
  radares. O walk-forward, o regime (BOCPD), o laboratório e o mapa de calor continuam com o
  histórico inteiro, porque a estatística da mesa vale entre sessões e cortar desligaria o veto
  das estratégias perdedoras.
- **Giro perdido:** um intervalo maior que 60 s entre o giro da entrada e o próximo giro gravado
  aciona a busca na fita pelo número que saiu logo depois do giro da entrada. Esse número
  liquida a entrada, os giros perdidos são gravados (`gap-…`) e o gale é interrompido. Se a fita
  não permitir saber o resultado, a entrada não é liquidada por número (registrado como
  `missed_spin_unknown`) e o P&L segue pelo saldo real.

## Consequências

- Os giros nunca são apagados: sessões anteriores ficam no banco.
- O contrato `spins` não muda. Os ids novos `base-…` e `gap-…` usam `source = 'mo'`.
