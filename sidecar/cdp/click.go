// SPDX-License-Identifier: Apache-2.0
// Despachante de cliques reais em coordenadas do canvas via Input.dispatchMouseEvent.
// Nunca usa element.click() — envia mousePressed e mouseReleased no target OOPIF da Playtech.
// Também inclui scripts de inspeção de coordenadas (getBoundingClientRect) e leitura do Bet Slip (centavos).

package cdp

import (
	"errors"
	"fmt"
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

	time.Sleep(time.Duration(8+rand.Intn(15)) * time.Millisecond)

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

// InjectBetReaderJS injeta no iframe da Playtech um leitor que publica via __emit o valor atual de Bet e Balance
// e as coordenadas reais (getBoundingClientRect) das casas 2to1 e fichas quando a janela é redimensionada.
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
