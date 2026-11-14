# Guia do operador — MONSTER

Uma página só. Quando bater a dúvida, volte aqui antes de mexer em qualquer coisa.

**Regra de ouro:** com a cabeça quente, não se muda regra e não se joga à mão. O bot é a parte fria da operação.

---

## 1. Ligar e desligar

| Quero… | Faço… |
|---|---|
| **Ligar** | Atalho **MONSTER LIVE** (ou `start-monster.bat -Live` na pasta do projeto). Ele recompila, abre o Chrome da mesa e o painel. |
| **Pausar as apostas** (a captura segue) | Telegram `/stop` · ou tecla **P** no painel |
| **Retomar** | Telegram `/resume` · ou tecla **R** no painel |
| **Parar as apostas na hora** (emergência) | Telegram `/kill` · ou `Ctrl+Alt+K` · ou botão KILL |
| **Desligar tudo** | `Ctrl+C` na janela preta do MONSTER, ou `stop-monster.bat` |

Fechar a janela preta no **X** **não** para o bot. O watchdog religa o que cair sozinho.

---

## 2. O que o bot faz no modo padrão

- **Aposta de verdade:** só as quebras que você usa desde o começo:
  - quebra de **coluna** e de **dúzia**;
  - **número repetido** (2× o mesmo número → as outras duas colunas);
  - quebras **1:1** (cor, alto/baixo, par/ímpar), com ficha de **R$ 0,40** e o 0 coberto.
- **Ficha das colunas e dúzias:** R$ 0,20. Escada ×3, até 5 degraus.
- **Sem stop win:** lucro nunca pausa o bot. O saque é com você, quando quiser.
- **Observando, sem apostar:** Espelhos & Gêmeos, Família 3-6-9 e Família 0-5-7, no quadro *OBSERVANDO SEM APOSTAR*.
- **Outras estratégias de número** (invictor, terminal quente, clusters): desligadas. Você liga no painel, na lista de estratégias, **se e quando quiser**. Toda entrada em número cobre o 0.

---

## 3. O painel (127.0.0.1:8791)

| Onde | O que significa |
|---|---|
| **OBSERVANDO / ENTRADA** | Esperando padrão / aposta no pano agora |
| **⚠ CAUTELA** (amarelo) | Muitos gales seguidos ou sequência longa: só coluna e dúzia entram até 2 acertos de primeira |
| **Luzes WS · MO · EXEC · DB** | Verde = ok. Uma apagada = aquela fonte caiu; o watchdog tenta religar |
| **BANCA / P&L** | Saldo real da mesa / resultado do bot (saque e depósito não contam) |
| **TIMELINE · 20** | Os últimos giros da **sessão atual**, iguais à fita da mesa |
| **Estatística da mesa** | Igual à tela *Statistics* da Playtech: 100/200/500/1000 giros, quentes e frios, setores, e qual script está mais quente |
| **ESCADA ×3** | Quanto custa cada degrau e quanto se perde se estourar |
| **Aba Decisões** | Cada entrada e cada "não entrou", com o motivo |
| **Tela vermelha piscando — BANCA ZERADA** | A banca acabou. O executor parou. Ao depositar, o bot rearma sozinho |

---

## 4. Mensagens do Telegram

| Mensagem | O que fazer |
|---|---|
| 🍀 **ENTRADA CONFIRMADA** e o resultado | Nada. É o bot trabalhando |
| ⚠️ **MODO CAUTELA LIGADO / DESLIGADO** | Nada. É o freio automático |
| **BANCA ZERADA** (estado PAUSADO) | Pare e respire. Só deposite de novo com a cabeça fria; o bot rearma sozinho |
| ⚠️ **EXTRATO DIVERGENTE** | O saldo não bateu com o que o bot calculou. Confira o histórico da mesa. Se você não apostou à mão, me chame |
| **ALERTA · fonte morta / estado DEGRADED** | O watchdog vai tentar religar. Se passar de 10 min, reinicie pelo MONSTER LIVE |

**Comandos:** `/status` · `/banca` · `/pnl` · `/ladder` · `/padroes` · `/ligar <id>` · `/desligar <id>` · `/stop` · `/resume` · `/kill`

---

## 5. Quando se preocupar — e quando não

**Não se preocupe com:**
- **Gale.** Perder no degrau 1 ou 2 é normal; a escada recupera.
- **Sequência longa de uma cor.** Acontece ~1 vez a cada 180 giros numa roleta normal.
- **Dias negativos.** A roleta tem borda da casa. O bot entrega disciplina, não garantia.

**Preocupe-se (e me chame) com:**
- `EXTRATO DIVERGENTE` repetido, sem você ter apostado à mão.
- O painel mostrando números diferentes da fita da mesa.
- Entrada saindo sem o padrão ter acontecido.

**Nunca:**
- colocar fichas à mão com o bot ligado (ele aborta as próprias entradas);
- mudar regra no calor do momento — anote e decida no dia seguinte.
