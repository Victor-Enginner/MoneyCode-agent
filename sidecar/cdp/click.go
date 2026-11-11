// SPDX-License-Identifier: Apache-2.0
// Despachante de cliques reais em coordenadas do canvas/SVG via Input.dispatchMouseEvent + resolução dinâmica DOM.
// Localiza fichas exclusivamente no chip-rack (inferior direito) e separa rigorosamente Colunas (2to1) de Dúzias (1st/2nd/3rd 12).
// Envia estritamente UM evento físico CDP por chamada, após aguardar a abertura da janela "PLACE YOUR BETS".

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

	  const centerOf = (el) => {
	    if (!el || !el.getBoundingClientRect) return null;
	    const r = el.getBoundingClientRect();
	    if (r.width < 2 || r.height < 2) return null;
	    return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2), w: r.width, h: r.height, el };
	  };

	  const findByText = (exactList) => {
	    const all = document.querySelectorAll('text, tspan, span, div, button, g, [data-automation-locator]');
	    const matches = [];
	    for (let i = 0; i < all.length; i++) {
	      const el = all[i];
	      if (el.children && el.children.length > 2) continue;
	      const t = (el.textContent || '').trim();
	      if (exactList.includes(t)) {
	        const c = centerOf(el);
	        if (c && c.y > 120) matches.push(c);
	      }
	    }
	    return matches;
	  };

	  // Localiza uma casa numérica (1..36) especificamente dentro do pano central de apostas (x entre 30%% e 73%% da tela)
	  const findClothNumber = (numStr) => {
	    const m = findByText([numStr]);
	    const cloth = m.filter(p =>
	      p.x > window.innerWidth * 0.34 &&
	      p.x < window.innerWidth * 0.72 &&
	      p.y > window.innerHeight * 0.65 &&
	      p.y < window.innerHeight * 0.86
	    );
	    return cloth.length > 0 ? cloth[0] : null;
	  };

	  let pt = null;
	  if (target.startsWith('chip:')) {
	    const cents = parseInt(target.split(':')[1], 10);
	    const label = (cents < 100) ? ('0.' + (cents / 10)) : String(cents / 100);
	    // Busca exclusivamente no chip-rack (canto inferior direito: x > 72%% da largura e y > 84%% da altura),
	    // jamais clicando em fichas que já estejam posicionadas sobre o pano de apostas.
	    const m = findByText([label, 'R$' + label, label.replace('.', ',')]);
	    const rack = m.filter(p => p.x > window.innerWidth * 0.72 && p.y > window.innerHeight * 0.84);
	    if (rack.length > 0) {
	      pt = rack[rack.length - 1];
	    } else {
	      const chipLoc = document.querySelector('[data-automation-locator*="chip-' + label + '"], [data-automation-locator*="chip_' + label + '"]');
	      if (chipLoc) pt = centerOf(chipLoc);
	    }
	  } else if (target === 'dozen:1') {
	    const m = findByText(['1st 12', '1ª 12', '1-12']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'dozen:2') {
	    const m = findByText(['2nd 12', '2ª 12', '13-24']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'dozen:3') {
	    const m = findByText(['3rd 12', '3ª 12', '25-36']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target.startsWith('col:') || target.startsWith('col2to1:')) {
	    // Colunas reais (2to1 à direita do pano):
	    // col:1 = fileira inferior (1, 4, 7 ... 31, 34)
	    // col:2 = fileira do meio  (2, 5, 8 ... 32, 35)
	    // col:3 = fileira superior (3, 6, 9 ... 33, 36)
	    const rowIdx = parseInt(target.split(':')[1], 10);
	    const m = findByText(['2to1', '2 to 1', '2:1']).filter(p =>
	      p.x > window.innerWidth * 0.55 &&
	      p.x < window.innerWidth * 0.76 &&
	      p.y > window.innerHeight * 0.65 &&
	      p.y < window.innerHeight * 0.86
	    );
	    if (m.length >= 3) {
	      m.sort((a, b) => b.y - a.y); // ordena de baixo (maior Y = col:1) para cima (menor Y = col:3)
	      pt = m[Math.min(Math.max(rowIdx - 1, 0), 2)];
	    }
	    if (!pt) {
	      // Fallback geométrico exato a partir das duas últimas casas da fileira (31->34, 32->35, 33->36)
	      const pair = rowIdx === 1 ? ['31', '34'] : (rowIdx === 2 ? ['32', '35'] : ['33', '36']);
	      const cPrev = findClothNumber(pair[0]);
	      const cLast = findClothNumber(pair[1]);
	      if (cPrev && cLast && cLast.x > cPrev.x) {
	        const stepX = cLast.x - cPrev.x;
	        pt = { x: Math.round(cLast.x + stepX * 0.85), y: cLast.y };
	      } else if (cLast) {
	        pt = { x: Math.round(cLast.x + cLast.w * 0.95), y: cLast.y };
	      }
	    }
	  } else if (target === 'low') {
	    const m = findByText(['1-18', '1 to 18']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'high') {
	    const m = findByText(['19-36', '19 to 36']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'even') {
	    const m = findByText(['EVEN', 'PAR', 'Par']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'odd') {
	    const m = findByText(['ODD', 'ÍMPAR', 'IMPAR', 'Ímpar']).filter(p => p.x < window.innerWidth * 0.72);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'red' || target === 'black') {
	    const loc = document.querySelector('[data-automation-locator*="' + target + '"], [class*="betSpot-' + target + '"], [class*="' + target + '-diamond"]');
	    if (loc) pt = centerOf(loc);
	    if (!pt) {
	      const ev = findByText(['EVEN', 'PAR']).filter(p => p.x < window.innerWidth * 0.72)[0];
	      const od = findByText(['ODD', 'ÍMPAR', 'IMPAR']).filter(p => p.x < window.innerWidth * 0.72)[0];
	      if (ev && od) {
	        const step = (od.x - ev.x) / 3;
	        const x = Math.round(target === 'red' ? (ev.x + step) : (ev.x + step * 2));
	        pt = { x, y: ev.y };
	      }
	    }
	  } else if (target === 'clear') {
	    const m = findByText(['UNDO', 'DESFAZER', 'Undo']);
	    if (m.length > 0) pt = m[0];
	  }

	  const finalX = pt ? pt.x : fbX;
	  const finalY = pt ? pt.y : fbY;
	  if (typeof window.__emit === 'function') {
	    window.__emit(JSON.stringify({ t: 'click_resolved', target, x: finalX, y: finalY, found: !!pt, at: Date.now() }));
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
