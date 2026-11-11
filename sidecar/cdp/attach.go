// SPDX-License-Identifier: Apache-2.0
// Gerenciador de auto-attach para iframes cross-origin (OOPIF) da Playtech / ptielive.
// Configura Target.setAutoAttach com flatten:true, habilita Runtime/Network/Page e injeta inject.js.
// Implementa reconexão com backoff exponencial (1s -> 30s, máx 5 tentativas) logando erros.

package cdp

import (
	"context"
	_ "embed"
	"errors"
	"fmt"
	"log"
	"strings"
	"time"
)

//go:embed inject.js
var observerJS string

// ObserverJS retorna o script de MutationObserver embutido para inspeção e testes.
func ObserverJS() string {
	return observerJS
}

// AutoAttach ativa a descoberta de targets e o auto-attach recursivo usando protocolo flatten.
func (c *Client) AutoAttach() error {
	_ = c.Send("", "Target.setDiscoverTargets", map[string]any{
		"discover": true,
	})
	return c.Send("", "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": false,
		"flatten":                true,
	})
}

// HandleAttached processa o evento Target.attachedToTarget propagando auto-attach para páginas e anexando sessões Playtech/ptielive.
func (c *Client) HandleAttached(ev Event) error {
	targetInfo, ok := ev.Params["targetInfo"].(map[string]any)
	if !ok {
		return nil
	}
	targetType, _ := targetInfo["type"].(string)
	rawURL, _ := targetInfo["url"].(string)
	sid, _ := ev.Params["sessionId"].(string)
	if sid == "" {
		sid = ev.SessionID
	}
	if sid == "" {
		return errors.New("missing sessionId on attached target")
	}

	// Propaga AutoAttach em qualquer target capaz de gerar sub-targets ou workers.
	_ = c.Send(sid, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": false,
		"flatten":                true,
	})

	lowerURL := strings.ToLower(rawURL)
	if strings.HasPrefix(lowerURL, "devtools://") || strings.HasPrefix(lowerURL, "chrome-extension://") {
		return nil
	}
	log.Printf("[cdp] anexando sessão %s (%s) url=%s", sid, targetType, rawURL)
	return c.AttachPlaytechSession(sid)
}

// AttachPlaytechSession habilita Runtime, Network, Page, binding __emit e injeta inject.js na sessão.
func (c *Client) AttachPlaytechSession(sid string) error {
	if err := c.Send(sid, "Runtime.enable", nil); err != nil {
		return fmt.Errorf("Runtime.enable: %w", err)
	}
	if err := c.Send(sid, "Network.enable", nil); err != nil {
		return fmt.Errorf("Network.enable: %w", err)
	}
	_ = c.Send(sid, "Page.enable", nil)
	if err := c.Send(sid, "Runtime.addBinding", map[string]any{"name": "__emit"}); err != nil {
		return fmt.Errorf("Runtime.addBinding: %w", err)
	}
	_ = c.Send(sid, "Page.addScriptToEvaluateOnNewDocument", map[string]any{
		"source": observerJS,
	})
	if err := c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   observerJS,
		"awaitPromise": false,
	}); err != nil {
		return fmt.Errorf("Runtime.evaluate inject.js: %w", err)
	}
	return nil
}

// ReEvaluateObserver re-executa o inject.js quando um frame navega para a URL da mesa após o login.
func (c *Client) ReEvaluateObserver(sid string) {
	if sid == "" {
		return
	}
	_ = c.Send(sid, "Runtime.addBinding", map[string]any{"name": "__emit"})
	_ = c.Send(sid, "Runtime.evaluate", map[string]any{
		"expression":   observerJS,
		"awaitPromise": false,
	})
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
				log.Printf("[cdp] conectado ao Chrome DevTools em %s (tentativa %d/%d)", cdpEndpoint, attempt, maxAttempts)
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
