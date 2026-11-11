// SPDX-License-Identifier: Apache-2.0
// Despachante de cliques reais em coordenadas do canvas/SVG via Input.dispatchMouseEvent + resolução dinâmica DOM.
// Localiza fichas (0.1, 0.2, 0.5, 1, 5, 10, 20, 50) e casas do pano (2to1, 1st/2nd/3rd 12, 1-18, EVEN, RED, BLACK, ODD, 19-36)
// diretamente via getBoundingClientRect() no iframe da Playtech e envia eventos físicos CDP.

package cdp

import (
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"math/rand"
	"time"
)

// DispatchClick envia o par mousePressed + mouseReleased via CDP na sessão do iframe Playtech
// com micro-jitter humano (8–22 ms entre press e release, ±2 px) conforme M6.
func (c *Client) DispatchClick(sid string, x, y int) error {
	if sid == "" {
		return errors.New("empty sessionId for DispatchClick")
	}
	jx := x + (rand.Intn(5) - 2)
	jy := y + (rand.Intn(5) - 2)
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

	time.Sleep(time.Duration(12+rand.Intn(18)) * time.Millisecond)

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

// DispatchTargetClick resolve a posição exata do elemento na mesa da Playtech (ficha ou cobertura)
// via getBoundingClientRect() + eventos Pointer/Mouse no elemento alvo + Input.dispatchMouseEvent via CDP.
func (c *Client) DispatchTargetClick(sid string, target string, fallbackX, fallbackY int) error {
	if sid == "" {
		return errors.New("empty sessionId for DispatchTargetClick")
	}

	targetJSON, _ := json.Marshal(target)
	js := fmt.Sprintf(`(async () => {
	  const target = %s;
	  const fbX = %d, fbY = %d;
	  const sleep = (ms) => new Promise(r => setTimeout(r, ms));

	  // Aguarda até 6s pela abertura da janela "PLACE YOUR BETS" / "FAÇA SUAS APOSTAS" após o giro
	  for (let wait = 0; wait < 24; wait++) {
	    const bodyTxt = (document.body && document.body.innerText) || '';
	    if (/PLACE YOUR BETS|FA[CÇ]A SUAS APOSTAS|FA[CÇ]AM SUAS APOSTAS/i.test(bodyTxt)) {
	      break;
	    }
	    await sleep(250);
	  }

	  const centerOf = (el) => {
	    if (!el || !el.getBoundingClientRect) return null;
	    const r = el.getBoundingClientRect();
	    if (r.width < 2 || r.height < 2) return null;
	    return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2), el };
	  };
	  const findByText = (exactList) => {
	    const all = document.querySelectorAll('text, span, div, button, g, [data-automation-locator]');
	    const matches = [];
	    for (let i = 0; i < all.length; i++) {
	      const el = all[i];
	      const t = (el.textContent || '').trim();
	      if (exactList.includes(t)) {
	        const c = centerOf(el);
	        if (c && c.y > 100) matches.push(c);
	      }
	    }
	    return matches;
	  };

	  let pt = null;
	  if (target.startsWith('chip:')) {
	    const cents = parseInt(target.split(':')[1], 10);
	    const label = (cents < 100) ? ('0.' + (cents / 10)) : String(cents / 100);
	    const chipLoc = document.querySelector('[data-automation-locator*="chip-' + label + '"], [data-automation-locator*="chip_' + label + '"], [data-automation-locator*="chip"][data-automation-locator*="' + cents + '"]');
	    if (chipLoc) pt = centerOf(chipLoc);
	    if (!pt) {
	      const m = findByText([label, 'R$' + label, label.replace('.', ',')]);
	      // Fichas ficam na metade inferior direita da tela (y > window.innerHeight * 0.65)
	      const lower = m.filter(p => p.y > window.innerHeight * 0.65 && p.x > window.innerWidth * 0.55);
	      if (lower.length > 0) pt = lower[lower.length - 1];
	      else if (m.length > 0) pt = m[m.length - 1];
	    }
	  } else if (target === 'dozen:1' || target === 'col:1') {
	    const m = findByText(['1st 12', '1ª 12', '1-12']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'dozen:2' || target === 'col:2') {
	    const m = findByText(['2nd 12', '2ª 12', '13-24']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'dozen:3' || target === 'col:3') {
	    const m = findByText(['3rd 12', '3ª 12', '25-36']);
	    if (m.length > 0) pt = m[0];
	  } else if (target.startsWith('col2to1:')) {
	    const rowIdx = parseInt(target.split(':')[1], 10); // 1=bottom(1..34), 2=middle(2..35), 3=top(3..36)
	    const m = findByText(['2to1', '2 to 1', '2:1']);
	    if (m.length >= 3) {
	      m.sort((a, b) => b.y - a.y); // ordena de baixo (maior Y = col 1) para cima (menor Y = col 3)
	      pt = m[Math.min(Math.max(rowIdx - 1, 0), m.length - 1)];
	    }
	  } else if (target === 'low') {
	    const m = findByText(['1-18', '1 to 18']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'high') {
	    const m = findByText(['19-36', '19 to 36']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'even') {
	    const m = findByText(['EVEN', 'PAR', 'Par']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'odd') {
	    const m = findByText(['ODD', 'ÍMPAR', 'IMPAR', 'Ímpar']);
	    if (m.length > 0) pt = m[0];
	  } else if (target === 'red' || target === 'black') {
	    const loc = document.querySelector('[data-automation-locator*="' + target + '"], [class*="betSpot-' + target + '"], [class*="' + target + '-diamond"]');
	    if (loc) pt = centerOf(loc);
	    if (!pt) {
	      const ev = findByText(['EVEN', 'PAR'])[0];
	      const od = findByText(['ODD', 'ÍMPAR', 'IMPAR'])[0];
	      if (ev && od) {
	        const step = (od.x - ev.x) / 3;
	        const x = Math.round(target === 'red' ? (ev.x + step) : (ev.x + step * 2));
	        pt = { x, y: ev.y, el: document.elementFromPoint(x, ev.y) };
	      }
	    }
	  } else if (target === 'clear') {
	    const m = findByText(['UNDO', 'DESFAZER', 'Undo']);
	    if (m.length > 0) pt = m[0];
	  }

	  const finalX = pt ? pt.x : fbX;
	  const finalY = pt ? pt.y : fbY;
	  const hitEl = document.elementFromPoint(finalX, finalY) || (pt && pt.el);
	  if (hitEl) {
	    const opts = { bubbles: true, cancelable: true, view: window, clientX: finalX, clientY: finalY, button: 0, buttons: 1 };
	    try {
	      hitEl.dispatchEvent(new PointerEvent('pointerdown', opts));
	      hitEl.dispatchEvent(new MouseEvent('mousedown', opts));
	      hitEl.dispatchEvent(new PointerEvent('pointerup', opts));
	      hitEl.dispatchEvent(new MouseEvent('mouseup', opts));
	      hitEl.dispatchEvent(new MouseEvent('click', opts));
	    } catch (_) {}
	  }
	  if (typeof window.__emit === 'function') {
	    window.__emit(JSON.stringify({ t: 'click_resolved', target, x: finalX, y: finalY, found: !!pt, at: Date.now() }));
	  }
	})()`, string(targetJSON), fallbackX, fallbackY)

	if target == "" {
		return c.DispatchClick(sid, fallbackX, fallbackY)
	}
	if err := c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   js,
		"awaitPromise": true,
	}); err != nil {
		log.Printf("[click] erro em Runtime.evaluate para target=%s: %v", target, err)
		return c.DispatchClick(sid, fallbackX, fallbackY)
	}
	time.Sleep(90 * time.Millisecond)
	return nil
}

// BetReaderJS injeta no iframe da Playtech um leitor que publica via __emit o valor atual de Bet e Balance.
const BetReaderJS = `(() => {
  try {
    const parseBrlCents = (txt) => {
      if (!txt) return null;
      const cleaned = txt.replace(/[^\d.,]/g, '').trim();
      if (!cleaned) return null;
      const normalized = cleaned.replace(/\./g, '').replace(',', '.');
      const val = parseFloat(normalized);
      if (Number.isNaN(val)) return null;
      return Math.round(val * 100);
    };
    let betCents = 0;
    let balCents = null;
    const all = document.querySelectorAll('[class*="balance"], [class*="bet"], [data-automation-locator*="bet"], [data-automation-locator*="balance"], span, div');
    for (let i = 0; i < all.length; i++) {
      const el = all[i];
      if (el.children.length > 2) continue;
      const t = (el.textContent || '').trim();
      if (/^Bet\s*R\$/i.test(t) || /^Aposta\s*R\$/i.test(t)) {
        const c = parseBrlCents(t);
        if (c !== null) betCents = c;
      } else if (/^Balance\s*R\$/i.test(t) || /^Saldo\s*R\$/i.test(t)) {
        const c = parseBrlCents(t);
        if (c !== null) balCents = c;
      }
    }
    if (typeof window.__emit === 'function') {
      window.__emit(JSON.stringify({ t: 'slip', bet_cents: betCents, balance_cents: balCents, at: Date.now() }));
    }
  } catch (_) {}
})();`

// RequestSlipRead solicita ao iframe da Playtech a leitura imediata do Bet Slip e Saldo via __emit.
func (c *Client) RequestSlipRead(sid string) error {
	if sid == "" {
		return errors.New("empty sessionId for RequestSlipRead")
	}
	return c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   BetReaderJS,
		"awaitPromise": false,
	})
}
