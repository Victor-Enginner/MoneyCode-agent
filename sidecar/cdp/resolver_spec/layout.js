// Verificação do resolvedor de alvos contra um layout sintético do pano (sem mesa ao vivo).
const assert = require('assert');

function buildLayout() {
  const items = [];
  const push = (t, x, y, w = 18, h = 18) => items.push({ t, x, y, w, h });
  const rowGap = 38;
  const cellW = 19.5;
  const left = 237;

  // Colunas reais: fileira col1 (base), col2 (meio), col3 (topo) — 12 casas cada.
  for (let i = 0; i < 12; i++) {
    push(String(1 + i * 3), left + i * cellW, 186); // col 1
    push(String(2 + i * 3), left + i * cellW, 148); // col 2
    push(String(3 + i * 3), left + i * cellW, 110); // col 3
  }
  // Âncoras 2to1: x=490, alinhadas às três fileiras.
  push('2to1', 490, 186, 24, 16);
  push('2to1', 490, 148, 24, 16);
  push('2to1', 490, 110, 24, 16);

  // Dúzias e linha de 18/par/ímpar.
  push('1st 12', 300, 186 + rowGap, 60, 20);
  push('2nd 12', 390, 186 + rowGap, 60, 20);
  push('3rd 12', 480, 186 + rowGap, 60, 20);
  push('1-18', 250, 186 + rowGap * 2, 50, 20);
  push('EVEN', 320, 186 + rowGap * 2, 50, 20);
  push('ODD', 460, 186 + rowGap * 2, 50, 20);
  push('19-36', 530, 186 + rowGap * 2, 50, 20);

  // Fita de histórico (direita, mesma faixa de Y das dúzias) — não pode confundir o resolvedor.
  const strip = [20, 11, 20, 5, 13, 9, 11, 6, 23, 4, 24, 4];
  strip.forEach((n, i) => push(String(n), 560 + i * 28, 218));

  // Bandeja de fichas (centavos com decimal).
  [['0.1', 355], ['0.2', 395], ['0.5', 435], ['1', 475], ['5', 515], ['10', 555], ['20', 595], ['50', 635]]
    .forEach(([t, x]) => push(t, x, 300, 22, 22));

  return items;
}

function makeEnv(items) {
  const els = items.map((it) => ({
    textContent: it.t,
    children: [],
    getBoundingClientRect: () => ({
      left: it.x - it.w / 2,
      top: it.y - it.h / 2,
      width: it.w,
      height: it.h,
    }),
  }));
  const emitted = [];
  global.window = {
    innerWidth: 1016,
    innerHeight: 326,
    __emit: (json) => emitted.push(JSON.parse(json)),
  };
  global.document = {
    body: { innerText: 'PLACE YOUR BETS  Balance R$ 50,00' },
    querySelectorAll: () => els,
    querySelector: () => null,
  };
  return emitted;
}

module.exports = { buildLayout, makeEnv, assert };
