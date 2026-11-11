// SPDX-License-Identifier: Apache-2.0
// Gerenciador de auto-attach para iframes cross-origin (OOPIF) da Playtech.
// Configura Target.setAutoAttach com flatten:true, habilita Runtime/Network e injeta inject.js.
// Implementa reconexão com backoff exponencial (1s -> 30s, máx 5 tentativas) logando erros.

package cdp

import (
	"context"
	_ "embed"
	"errors"
	"fmt"
	"strings"
	"time"
)

//go:embed inject.js
var observerJS string

// ObserverJS retorna o script de MutationObserver embutido para inspeção e testes.
func ObserverJS() string {
	return observerJS
}

// AutoAttach ativa o auto-attach recursivo para targets OOPIF usando protocolo flatten.
func (c *Client) AutoAttach() error {
	return c.Send("", "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": false,
		"flatten":                true,
	})
}

// HandleAttached processa o evento Target.attachedToTarget filtrando iframes da Playtech.
func (c *Client) HandleAttached(ev Event) error {
	targetInfo, ok := ev.Params["targetInfo"].(map[string]any)
	if !ok {
		return nil
	}
	rawURL, _ := targetInfo["url"].(string)
	if !strings.Contains(strings.ToLower(rawURL), "playtech") {
		return nil
	}
	sid, _ := ev.Params["sessionId"].(string)
	if sid == "" {
		sid = ev.SessionID
	}
	if sid == "" {
		return errors.New("missing sessionId on attached playtech target")
	}
	return c.AttachPlaytechSession(sid)
}

// AttachPlaytechSession habilita Runtime, Network, binding __emit e avalia inject.js na sessão OOPIF.
func (c *Client) AttachPlaytechSession(sid string) error {
	if err := c.Send(sid, "Runtime.enable", nil); err != nil {
		return fmt.Errorf("Runtime.enable: %w", err)
	}
	if err := c.Send(sid, "Network.enable", nil); err != nil {
		return fmt.Errorf("Network.enable: %w", err)
	}
	if err := c.Send(sid, "Runtime.addBinding", map[string]any{"name": "__emit"}); err != nil {
		return fmt.Errorf("Runtime.addBinding: %w", err)
	}
	if err := c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   observerJS,
		"awaitPromise": false,
	}); err != nil {
		return fmt.Errorf("Runtime.evaluate inject.js: %w", err)
	}
	return nil
}

// ConnectWithBackoff estabelece conexão CDP e executa AutoAttach com backoff 1s->30s (máx 5 tentativas).
func ConnectWithBackoff(
	ctx context.Context,
	cdpEndpoint string,
	errLogger func(kind, detail string),
) (*Client, error) {
	const maxAttempts = 5
	backoff := 1 * time.Second
	const maxBackoff = 30 * time.Second

	var lastErr error
	for attempt := 1; attempt <= maxAttempts; attempt++ {
		client, err := Dial(ctx, cdpEndpoint, errLogger)
		if err == nil {
			if attachErr := client.AutoAttach(); attachErr == nil {
				return client, nil
			} else {
				_ = client.Close()
				err = attachErr
			}
		}
		lastErr = err
		if errLogger != nil {
			errLogger("cdp_reattach", fmt.Sprintf("attempt %d/%d failed: %v", attempt, maxAttempts, err))
		}
		if attempt == maxAttempts {
			break
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(backoff):
		}
		backoff *= 2
		if backoff > maxBackoff {
			backoff = maxBackoff
		}
	}
	return nil, fmt.Errorf("cdp reattach exhausted after %d attempts: %w", maxAttempts, lastErr)
}
