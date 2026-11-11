// SPDX-License-Identifier: Apache-2.0
// Cliente WebSocket do Chrome DevTools Protocol (CDP) com multiplexação flatten.
// Descobre webSocketDebuggerUrl via /json/version e despacha comandos/eventos por sessionId.
// Suporta Input.dispatchMouseEvent em coordenadas reais de canvas sem uso de seletores CSS.

package cdp

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

// Event representa uma notificação assíncrona recebida do Chrome via CDP.
type Event struct {
	Method    string         `json:"method"`
	Params    map[string]any `json:"params"`
	SessionID string         `json:"sessionId,omitempty"`
}

type rpcRequest struct {
	ID        int64          `json:"id"`
	Method    string         `json:"method"`
	Params    map[string]any `json:"params,omitempty"`
	SessionID string         `json:"sessionId,omitempty"`
}

type rpcMessage struct {
	ID        int64           `json:"id,omitempty"`
	Method    string          `json:"method,omitempty"`
	Params    map[string]any  `json:"params,omitempty"`
	Result    json.RawMessage `json:"result,omitempty"`
	SessionID string          `json:"sessionId,omitempty"`
	Error     *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (e *rpcError) Error() string {
	return fmt.Sprintf("cdp error %d: %s", e.Code, e.Message)
}

type versionResponse struct {
	WebSocketDebuggerURL string `json:"webSocketDebuggerUrl"`
}

// Client gerencia a conexão WebSocket com o navegador Chrome e multiplexa sessões OOPIF.
type Client struct {
	conn    *websocket.Conn
	writeMu sync.Mutex
	nextID  atomic.Int64
	pending sync.Map
	events  chan Event
	closed  chan struct{}
	closeMu sync.Once
	errLog  func(kind, detail string)
}

// DiscoverDebuggerURL consulta /json/version na porta de debug do Chrome e retorna o webSocketDebuggerUrl.
func DiscoverDebuggerURL(ctx context.Context, baseURL string) (string, error) {
	trimmed := strings.TrimRight(baseURL, "/")
	if strings.HasPrefix(trimmed, "ws://") || strings.HasPrefix(trimmed, "wss://") {
		return trimmed, nil
	}
	endpoint := trimmed + "/json/version"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return "", fmt.Errorf("create request: %w", err)
	}
	httpClient := &http.Client{Timeout: 5 * time.Second}
	resp, err := httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("fetch %s: %w", endpoint, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("unexpected status %d from %s", resp.StatusCode, endpoint)
	}
	var ver versionResponse
	if err := json.NewDecoder(resp.Body).Decode(&ver); err != nil {
		return "", fmt.Errorf("decode version json: %w", err)
	}
	if ver.WebSocketDebuggerURL == "" {
		return "", errors.New("empty webSocketDebuggerUrl (verifique --user-data-dir no Chrome 136+)")
	}
	return ver.WebSocketDebuggerURL, nil
}

// Dial conecta ao endpoint HTTP ou WS do Chrome DevTools Protocol e inicia o loop de leitura.
func Dial(ctx context.Context, cdpEndpoint string, errLogger func(kind, detail string)) (*Client, error) {
	wsURL, err := DiscoverDebuggerURL(ctx, cdpEndpoint)
	if err != nil {
		return nil, err
	}
	conn, _, err := websocket.DefaultDialer.DialContext(ctx, wsURL, nil)
	if err != nil {
		return nil, fmt.Errorf("dial ws %s: %w", wsURL, err)
	}
	c := &Client{
		conn:   conn,
		events: make(chan Event, 256),
		closed: make(chan struct{}),
		errLog: errLogger,
	}
	go c.readLoop()
	return c, nil
}

// Events retorna o canal somente-leitura de eventos CDP multiplexados.
func (c *Client) Events() <-chan Event {
	return c.events
}

// Send envia um comando CDP na sessão informada (ou navegador raiz se sessionID == "") e aguarda resposta.
func (c *Client) Send(sessionID, method string, params map[string]any) error {
	id := c.nextID.Add(1)
	respCh := make(chan error, 1)
	c.pending.Store(id, respCh)
	defer c.pending.Delete(id)

	req := rpcRequest{
		ID:        id,
		Method:    method,
		Params:    params,
		SessionID: sessionID,
	}

	c.writeMu.Lock()
	err := c.conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
	if err == nil {
		err = c.conn.WriteJSON(req)
	}
	c.writeMu.Unlock()
	if err != nil {
		return fmt.Errorf("write cdp %s: %w", method, err)
	}

	select {
	case rpcErr := <-respCh:
		return rpcErr
	case <-c.closed:
		return errors.New("cdp client closed")
	case <-time.After(10 * time.Second):
		return fmt.Errorf("timeout waiting for cdp response to %s", method)
	}
}

// DispatchMouseClick executa um clique físico no canvas via Input.dispatchMouseEvent.
func (c *Client) DispatchMouseClick(sessionID string, x, y float64) error {
	if err := c.Send(sessionID, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mousePressed",
		"x":          x,
		"y":          y,
		"button":     "left",
		"clickCount": 1,
		"buttons":    1,
	}); err != nil {
		return err
	}
	return c.Send(sessionID, "Input.dispatchMouseEvent", map[string]any{
		"type":       "mouseReleased",
		"x":          x,
		"y":          y,
		"button":     "left",
		"clickCount": 1,
		"buttons":    0,
	})
}

// Close encerra a conexão WebSocket com o navegador.
func (c *Client) Close() error {
	var err error
	c.closeMu.Do(func() {
		close(c.closed)
		err = c.conn.Close()
	})
	return err
}

func (c *Client) readLoop() {
	defer func() {
		c.closeMu.Do(func() {
			close(c.closed)
		})
		close(c.events)
	}()

	for {
		var msg rpcMessage
		if err := c.conn.ReadJSON(&msg); err != nil {
			if c.errLog != nil {
				c.errLog("cdp_read", err.Error())
			}
			return
		}
		if msg.ID != 0 {
			if chAny, ok := c.pending.Load(msg.ID); ok {
				ch := chAny.(chan error)
				if msg.Error != nil {
					ch <- msg.Error
				} else {
					ch <- nil
				}
			}
			continue
		}
		if msg.Method != "" {
			ev := Event{
				Method:    msg.Method,
				Params:    msg.Params,
				SessionID: msg.SessionID,
			}
			select {
			case c.events <- ev:
			default:
				if c.errLog != nil {
					c.errLog("cdp_event_drop", msg.Method)
				}
			}
		}
	}
}
