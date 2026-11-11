// Executa o resolvedor real extraído de sidecar/cdp/click.go contra o layout sintético.
const fs = require('fs');
const path = require('path');
const { buildLayout, makeEnv } = require('./layout');

const goSrc = fs.readFileSync(path.join(__dirname, '..', 'click.go'), 'utf8');
const start = goSrc.indexOf('js := fmt.Sprintf(`') + 'js := fmt.Sprintf(`'.length;
const end = goSrc.indexOf('`, string(targetJSON), fallbackX, fallbackY)');
const template = goSrc.slice(start, end);

const items = buildLayout();

async function resolve(target) {
  const emitted = makeEnv(items);
  const js = template
    .split('%s').join(JSON.stringify(target))
    .split('%d').join('0');
  await new Function('return ' + js)();
  const ev = emitted.filter((e) => e.t === 'click_resolved');
  if (ev.length !== 1) throw new Error('expected exactly one click_resolved, got ' + JSON.stringify(emitted));
  return ev[0];
}

const cases = [
  // Coluna 2 repetindo -> a entrada é nas colunas 1 e 3 (base e topo do pano).
  { target: 'col:1', x: 490, y: 186, how: '2to1-anchor' },
  { target: 'col:3', x: 490, y: 110, how: '2to1-anchor' },
  { target: 'col:2', x: 490, y: 148, how: '2to1-anchor' },
  { target: 'dozen:2', x: 390, y: 224, how: 'cloth-text' },
  { target: 'dozen:3', x: 480, y: 224, how: 'cloth-text' },
  { target: 'chip:10', x: 355, y: 300, how: 'rack' },
  { target: 'chip:5000', x: 635, y: 300, how: 'rack' },
  { target: 'even', x: 320, y: 262, how: 'cloth-text' },
  // Pleno: cai na casa do número na grade derivada das âncoras 2to1.
  { target: 'straight:17', x: 335, y: 148, how: 'cloth-number' },
  { target: 'straight:1', x: 237, y: 186, how: 'cloth-number' },
  { target: 'straight:36', x: 452, y: 110, how: 'cloth-number' },
];

(async () => {
  let failures = 0;
  for (const c of cases) {
    const got = await resolve(c.target);
    const ok = got.found && got.x === c.x && got.y === c.y && got.how === c.how;
    if (!ok) failures++;
    console.log(
      (ok ? 'ok   ' : 'FAIL ') + c.target.padEnd(9) +
      ' found=' + got.found + ' x=' + got.x + ' y=' + got.y + ' how=' + got.how +
      '  (esperado x=' + c.x + ' y=' + c.y + ' how=' + c.how + ')'
    );
  }

  // Sem âncoras do pano no DOM: o alvo sai NÃO resolvido (o sidecar aborta em vez de clicar às cegas).
  const elements = items.filter((it) => it.t !== '2to1');
  const emitted = [];
  global.window = { innerWidth: 1016, innerHeight: 326, __emit: (j) => emitted.push(JSON.parse(j)) };
  global.document = {
    body: { innerText: 'PLACE YOUR BETS' },
    querySelectorAll: () => elements.map((it) => ({
      textContent: it.t,
      children: [],
      getBoundingClientRect: () => ({
        left: it.x - it.w / 2, top: it.y - it.h / 2, width: it.w, height: it.h,
      }),
    })),
    querySelector: () => null,
  };
  const js = template.split('%s').join('"col:1"').split('%d').join('0');
  await new Function('return ' + js)();
  const got = emitted.filter((e) => e.t === 'click_resolved')[0];
  const okUnresolved = !got.found;
  if (!okUnresolved) failures++;
  console.log((okUnresolved ? 'ok   ' : 'FAIL ') + 'sem 2to1 -> found=' + got.found + ' how=' + got.how + ' (esperado found=false)');

  // Pleno inexistente no pano (grade sem o 0 neste layout): não resolve, o sidecar recusa.
  const semZero = await resolve('straight:0');
  const okZero = !semZero.found;
  if (!okZero) failures++;
  console.log((okZero ? 'ok   ' : 'FAIL ') + 'straight:0 -> found=' + semZero.found + ' how=' + semZero.how + ' (esperado found=false)');

  console.log(failures === 0 ? '\nRESOLVER_SPEC_OK' : '\nRESOLVER_SPEC_FAILED: ' + failures);
  process.exit(failures === 0 ? 0 : 1);
})();
