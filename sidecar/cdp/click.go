// SPDX-License-Identifier: Apache-2.0
// Despachante de cliques reais em coordenadas do canvas via Input.dispatchMouseEvent.
// Nunca usa element.click() ou seletores CSS — envia mousePressed e mouseReleased no target OOPIF.
// Respeita o contrato §7 do AGENTS.md com clickCount:1 e máscara de buttons consistente.

package cdp

import (
	"errors"
	"fmt"
)

// DispatchClick envia o par mousePressed + mouseReleased via CDP na sessão do iframe Playtech.
func (c *Client) DispatchClick(sid string, x, y int) error {
	if sid == "" {
		return errors.New("empty sessionId for DispatchClick")
	}
	if err := c.Send(sid, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mousePressed",
		"x":          x,
		"y":          y,
		"button":     "left",
		"clickCount": 1,
		"buttons":    1,
	}); err != nil {
		return fmt.Errorf("mousePressed (%d,%d): %w", x, y, err)
	}
	if err := c.Send(sid, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mouseReleased",
		"x":          x,
		"y":          y,
		"button":     "left",
		"clickCount": 1,
		"buttons":    0,
	}); err != nil {
		return fmt.Errorf("mouseReleased (%d,%d): %w", x, y, err)
	}
	return nil
}
