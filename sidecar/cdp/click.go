// SPDX-License-Identifier: Apache-2.0
// Despachante de cliques reais em coordenadas do canvas/SVG via Input.dispatchMouseEvent + resolução dinâmica DOM.
// A geometria do pano é derivada das âncoras do próprio jogo (rótulos "2to1" e bandeja de fichas), nunca de frações fixas de viewport.
// Separa rigorosamente Colunas (2to1) de Dúzias (1st/2nd/3rd 12); alvo não resolvido sai com found:false e o sidecar não clica.
// Envia estritamente UM evento físico CDP por chamada, após aguardar a abertura da janela "PLACE YOUR BETS".
// Regressão: `node sidecar/cdp/resolver_spec/run.js` valida colunas, dúzias, fichas e o caso não resolvido.

package cdp

import (
	"encoding/json"
	"errors"
	"fmt"
	"math/rand"
	"time"
)

// DispatchClick envia o par mousePressed + mouseReleased via CDP na sessão do iframe Playtech
// com micro-jitter humano (12–28 ms entre press e release, ±1 px) conforme M6.
func (c *Client) DispatchClick(sid string, x, y int) error {
	if sid == "" {
		return errors.New("empty sessionId for DispatchClick")
	}
	jx := x + (rand.Intn(3) - 1)
	jy := y + (rand.Intn(3) - 1)
	if jx < 1 {
		jx = x
	}
	if jy < 1 {
		jy = y
	}

	if err := c.Send(sid, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mouseMoved",
		"x":          jx,
		"y":          jy,
		"button":     "none",
		"clickCount": 0,
		"buttons":    0,
	}); err != nil {
		return fmt.Errorf("mouseMoved (%d,%d): %w", jx, jy, err)
	}

	if err := c.Send(sid, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mousePressed",
		"x":          jx,
		"y":          jy,
		"button":     "left",
		"clickCount": 1,
		"buttons":    1,
	}); err != nil {
		return fmt.Errorf("mousePressed (%d,%d): %w", jx, jy, err)
	}

	time.Sleep(time.Duration(14+rand.Intn(14)) * time.Millisecond)

	if err := c.Send(sid, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mouseReleased",
		"x":          jx,
		"y":          jy,
		"button":     "left",
		"clickCount": 1,
		"buttons":    0,
	}); err != nil {
		return fmt.Errorf("mouseReleased (%d,%d): %w", jx, jy, err)
	}
	return nil
}

// RequestTargetCoords aguarda a abertura da janela de apostas (caso ainda esteja em WAIT FOR THE NEXT ROUND)
// e resolve a coordenada exata (x, y) do elemento via getBoundingClientRect(), emitindo `click_resolved`
// sem disparar eventos sintéticos duplicados em JS.
func (c *Client) RequestTargetCoords(sid string, target string, fallbackX, fallbackY int) error {
	if sid == "" {
		return errors.New("empty sessionId for RequestTargetCoords")
	}

	targetJSON, _ := json.Marshal(target)
	// Resolve a coordenada real do alvo DENTRO do pano, derivando a geometria do próprio jogo
	// (âncoras `2to1` e bandeja de fichas) em vez de frações fixas de viewport.
	// Qualquer alvo não resolvido sai com `found:false` para o sidecar abortar em vez de clicar às cegas.
	js := fmt.Sprintf(`(async () => {
  const target = %s;
  const fbX = %d, fbY = %d;
  const sleep = (ms) => new Promise(r => setTimeout(r, ms));

  const isRoundOpenNow = () => {
    const bodyTxt = (document.body && document.body.innerText) || '';
    if (/PLACE YOUR BETS|FA[CÇ]A SUAS APOSTAS|FA[CÇ]AM SUAS APOSTAS/i.test(bodyTxt)) {
      return true;
    }
    if (/WAIT FOR THE NEXT ROUND|NO MORE BETS|AGUARDE A PR[OÓ]XIMA|N[AÃ]O H[AÁ] MAIS APOSTAS|SEM MAIS APOSTAS/i.test(bodyTxt)) {
      return false;
    }
    return true;
  };

  if (target !== 'clear') {
    let waited = false;
    for (let wait = 0; wait < 75; wait++) {
      if (isRoundOpenNow()) break;
      waited = true;
      await sleep(150);
    }
    if (waited) {
      // Aguarda a transição de câmera da Playtech estabilizar o layout do pano antes de ler getBoundingClientRect()
      await sleep(280);
    }
  }

  const near = (a, b) => Math.abs(a - b);

  const boxOf = (el) => {
    if (!el || !el.getBoundingClientRect) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return null;
    return {
      x: Math.round(r.left + r.width / 2),
      y: Math.round(r.top + r.height / 2),
      w: r.width,
      h: r.height,
      t: (el.textContent || '').trim(),
      el: el
    };
  };

  // Varredura única DEPOIS de a janela abrir, para não usar geometria de layout antigo.
  const scans = () => {
    const out = [];
    const all = document.querySelectorAll('text, tspan, span, div, button, g, [data-automation-locator]');
    for (let i = 0; i < all.length; i++) {
      const el = all[i];
      if (el.children && el.children.length > 2) continue;
      const b = boxOf(el);
      if (b) out.push(b);
    }
    return out;
  };
  const nodes = scans();

  // ---- Âncora das colunas reais: os três rótulos "2to1" do pano dizem onde a coluna está.
  // Sem filtro de viewport: "2to1" só existe nessa coluna, então a âncora é o próprio jogo.
  const colAnchor = (() => {
    const labels = nodes.filter(n => /^(2to1|2 to 1|2:1)$/i.test(n.t));
    if (labels.length < 3) return null;

    const groups = [];
    for (const p of labels) {
      let g = groups.find(gr => near(gr.x, p.x) <= 45);
      if (!g) {
        g = { x: p.x, items: [] };
        groups.push(g);
      } else {
        g.x = (g.x + p.x) / 2;
      }
      g.items.push(p);
    }

    let best = null;
    for (const g of groups) {
      const ys = [];
      for (const it of g.items) {
        if (!ys.some(y => near(y, it.y) <= 12)) ys.push(it.y);
      }
      if (ys.length < 3) continue;
      ys.sort((a, b) => b - a); // Y maior = coluna 1 (base); Y menor = coluna 3 (topo)
      const rows = [];
      for (const y of ys.slice(0, 3)) {
        const it = g.items.find(i => near(i.y, y) <= 12);
        if (!it) { rows.length = 0; break; }
        rows.push({ x: it.x, y: it.y });
      }
      if (rows.length !== 3) continue;
      const gap = rows[0].y - rows[2].y;
      if (gap <= 60) continue; // três linhas amontoadas não são o pano
      if (!best || gap > best.gap) best = { x: g.x, rows: rows, gap: gap };
    }
    return best;
  })();

  // ---- Retângulo lógico do pano, derivado das âncoras (nada de 0.65/0.86 fixos).
  const cloth = (() => {
    if (!colAnchor) return null;
    const rowGap = colAnchor.gap / 2;
    if (!(rowGap > 10)) return null;
    const right = colAnchor.x - rowGap * 0.35;
    const top = colAnchor.rows[2].y - rowGap * 0.55;
    const lastRowY = colAnchor.rows[0].y;
    const bottom = lastRowY + rowGap * 3.2; // cobre a linha das dúzias e a de 18/par/vermelho
    const nums = nodes.filter(n =>
      /^\d{1,2}$/.test(n.t) &&
      n.x < right &&
      n.y > top &&
      n.y <= lastRowY + rowGap * 0.55
    );
    if (nums.length < 12) return null;
    let left = nums[0].x;
    for (const n of nums) left = Math.min(left, n.x);
    return { rowGap: rowGap, right: right, left: left, top: top, bottom: bottom, nums: nums };
  })();

  // Procura um texto do pano (dúzias, 18, par, ímpar) só dentro do retângulo derivado.
  const findInCloth = (exactList) => {
    const hits = nodes.filter(n => exactList.includes(n.t));
    if (hits.length === 0) return null;
    if (!cloth) return hits[0];
    const inside = hits.filter(p =>
      p.y > cloth.top &&
      p.y < cloth.bottom &&
      p.x > cloth.left - cloth.rowGap * 4 &&
      p.x < cloth.right + cloth.rowGap * 2
    );
    return inside.length > 0 ? inside[0] : hits[0];
  };

  // ---- Bandeja de fichas: a linha horizontal com mais denominações distintas.
  // Comparar denominações (e não posição na tela) evita clicar numa ficha já posta no pano.
  const CHIP_VALUES = [10, 20, 50, 100, 500, 1000, 2000, 5000];
  const chipValue = (txt) => {
    const cleaned = String(txt).replace(/\s/g, '').replace(/R\$/i, '').replace(',', '.');
    if (!/^\d+(\.\d+)?$/.test(cleaned)) return null;
    const v = parseFloat(cleaned);
    if (!isFinite(v) || v <= 0) return null;
    return Math.round(v * 100);
  };

  const rack = (() => {
    const cands = [];
    for (const n of nodes) {
      const v = chipValue(n.t);
      if (v === null || CHIP_VALUES.indexOf(v) === -1) continue;
      // "0.1"/"0.2" só existem como ficha; nenhuma casa do pano nem número da fita tem decimal.
      const decimal = n.t.indexOf('.') !== -1 || n.t.indexOf(',') !== -1;
      cands.push({ x: n.x, y: n.y, v: v, decimal: decimal });
    }
    const anchors = cands.filter(c => c.decimal && (c.v === 10 || c.v === 20));
    if (anchors.length < 2) return null;

    // A bandeja é a linha que carrega as fichas de centavos; entre candidatas, a mais completa vence.
    let best = [];
    for (const a of anchors) {
      const row = cands.filter(c => near(c.y, a.y) <= 24);
      const distinct = [];
      for (const c of row) {
        if (!distinct.some(d => d.v === c.v)) distinct.push(c);
      }
      if (distinct.length > best.length) best = distinct;
    }
    return best.length >= 3 ? best : null;
  })();

  let pt = null;
  let how = 'none';

  if (target.startsWith('chip:')) {
    const denom = parseInt(target.split(':')[1], 10);
    if (rack) {
      const hit = rack.find(c => c.v === denom);
      if (hit) {
        pt = { x: hit.x, y: hit.y };
        how = 'rack';
      }
    }
    if (!pt) {
      const label = (denom < 100) ? ('0.' + (denom / 10)) : String(denom / 100);
      const loc = document.querySelector(
        '[data-automation-locator*="chip-' + label + '"], [data-automation-locator*="chip_' + label + '"]'
      );
      const b = boxOf(loc);
      if (b) {
        pt = { x: b.x, y: b.y };
        how = 'chip-locator';
      }
    }
  } else if (target === 'dozen:1' || target === 'dozen:2' || target === 'dozen:3') {
    const idx = parseInt(target.split(':')[1], 10);
    const lists = {
      1: ['1st 12', '1ST 12', '1ª 12', '1-12'],
      2: ['2nd 12', '2ND 12', '2ª 12', '13-24'],
      3: ['3rd 12', '3RD 12', '3ª 12', '25-36']
    };
    const hit = findInCloth(lists[idx] || []);
    if (hit) {
      pt = { x: hit.x, y: hit.y };
      how = 'cloth-text';
    }
  } else if (target.startsWith('col:') || target.startsWith('col2to1:')) {
    // Colunas reais do pano ("2to1"): col:1 = base (1,4,7...34), col:2 = meio, col:3 = topo.
    // A âncora é o próprio rótulo da coluna, então o clique cai exatamente sobre ela.
    const rowIdx = parseInt(target.split(':')[1], 10);
    if (colAnchor && rowIdx >= 1 && rowIdx <= 3) {
      const r = colAnchor.rows[rowIdx - 1];
      pt = { x: r.x, y: r.y };
      how = '2to1-anchor';
    }
  } else if (target === 'low') {
    const hit = findInCloth(['1-18', '1 to 18']);
    if (hit) { pt = { x: hit.x, y: hit.y }; how = 'cloth-text'; }
  } else if (target === 'high') {
    const hit = findInCloth(['19-36', '19 to 36']);
    if (hit) { pt = { x: hit.x, y: hit.y }; how = 'cloth-text'; }
  } else if (target === 'even') {
    const hit = findInCloth(['EVEN', 'PAR', 'Par']);
    if (hit) { pt = { x: hit.x, y: hit.y }; how = 'cloth-text'; }
  } else if (target === 'odd') {
    const hit = findInCloth(['ODD', 'ÍMPAR', 'IMPAR', 'Ímpar']);
    if (hit) { pt = { x: hit.x, y: hit.y }; how = 'cloth-text'; }
  } else if (target === 'red' || target === 'black') {
    const loc = document.querySelector(
      '[data-automation-locator*="' + target + '"], [class*="betSpot-' + target + '"], [class*="' + target + '-diamond"]'
    );
    const b = boxOf(loc);
    if (b) {
      pt = { x: b.x, y: b.y };
      how = 'locator';
    }
    if (!pt) {
      const ev = findInCloth(['EVEN', 'PAR']);
      const od = findInCloth(['ODD', 'ÍMPAR', 'IMPAR']);
      if (ev && od && od.x > ev.x) {
        const step = (od.x - ev.x) / 3;
        pt = { x: Math.round(target === 'red' ? (ev.x + step) : (ev.x + step * 2)), y: ev.y };
        how = 'between-even-odd';
      }
    }
  } else if (target === 'clear') {
    const m = nodes.filter(n => /^(UNDO|DESFAZER|Undo)$/i.test(n.t));
    if (m.length > 0) {
      pt = { x: m[0].x, y: m[0].y };
      how = 'undo';
    }
  }

  if (typeof window.__emit === 'function') {
    window.__emit(JSON.stringify({
      t: 'click_resolved',
      target: target,
      x: pt ? pt.x : fbX,
      y: pt ? pt.y : fbY,
      found: !!pt,
      how: how,
      at: Date.now()
    }));
  }
})()`, string(targetJSON), fallbackX, fallbackY)

	return c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   js,
		"awaitPromise": true,
	})
}

// BetReaderJS injeta no iframe da Playtech um leitor imediato de Bet, Balance, Last Win, Game Code e fita de 12 números.
const BetReaderJS = `(() => {
  try {
    const parseMoneyCents = (raw) => {
      if (!raw) return null;
      const m = String(raw).match(/(\d[\d.,]*)/);
      if (!m) return null;
      let s = m[1];
      const lastDot = s.lastIndexOf('.');
      const lastComma = s.lastIndexOf(',');
      if (lastDot !== -1 && lastComma !== -1) {
        if (lastDot > lastComma) s = s.replace(/,/g, '');
        else s = s.replace(/\./g, '').replace(',', '.');
      } else if (lastComma !== -1) {
        s = s.replace(',', '.');
      } else if (lastDot !== -1) {
        const parts = s.split('.');
        if (parts.length > 2) s = parts.slice(0, -1).join('') + '.' + parts[parts.length - 1];
      }
      const val = parseFloat(s);
      if (Number.isNaN(val)) return null;
      return Math.round(val * 100);
    };

    const bodyTxt = (document.body && document.body.innerText) || '';
    let balCents = null;
    const balMatch = bodyTxt.match(/(?:Balance|Saldo)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (balMatch) balCents = parseMoneyCents(balMatch[1]);

    let betCents = 0;
    const betMatch = bodyTxt.match(/\b(?:Bet|Aposta|Total Bet)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (betMatch) {
      const c = parseMoneyCents(betMatch[1]);
      if (c !== null) betCents = c;
    }

    let lastWinCents = 0;
    const winMatch = bodyTxt.match(/(?:Last Win|[UÚ]ltimo Ganho)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (winMatch) {
      const c = parseMoneyCents(winMatch[1]);
      if (c !== null) lastWinCents = c;
    }

    let gameCode = '';
    const gcMatch = bodyTxt.match(/(?:Game Code|C[oó]digo do Jogo)\s*[:\s]*(\d{5,16})/i);
    if (gcMatch) gameCode = gcMatch[1];

    let roundOpen = false;
    if (/PLACE YOUR BETS|FA[CÇ]A SUAS APOSTAS|FA[CÇ]AM SUAS APOSTAS/i.test(bodyTxt)) {
      roundOpen = true;
    } else if (/WAIT FOR THE NEXT ROUND|NO MORE BETS|AGUARDE A PR[OÓ]XIMA|N[AÃ]O H[AÁ] MAIS APOSTAS|SEM MAIS APOSTAS/i.test(bodyTxt)) {
      roundOpen = false;
    } else if (balCents !== null) {
      roundOpen = true;
    }

    const nums = Array.isArray(window.__monster_last_nums) ? window.__monster_last_nums : [];
    if (typeof window.__emit === 'function') {
      window.__emit(JSON.stringify({
        t: 'slip',
        nums,
        bet_cents: betCents,
        balance_cents: balCents,
        last_win_cents: lastWinCents,
        game_code: gameCode,
        round_open: roundOpen,
        at: Date.now()
      }));
    }
  } catch (_) {}
})();`

// RequestSlipRead solicita ao iframe da Playtech a leitura imediata do Bet Slip, Saldo e fita via __emit.
func (c *Client) RequestSlipRead(sid string) error {
	if sid == "" {
		return errors.New("empty sessionId for RequestSlipRead")
	}
	return c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   BetReaderJS,
		"awaitPromise": false,
	})
}
