// SPDX-License-Identifier: Apache-2.0
// Ponto de entrada do sidecar de captura do projeto MONSTER.
// Implementa whitelist estrita de sockets Playtech (eurolive-frontend primário / ielive-gateway standby),
// filtro de frames de controle (len > 8), --strict, --freeze, --dump-frames e --dump-op (FrameSent).

package main

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"monster/sidecar/cdp"
	"monster/sidecar/wsdecap"
)

// SpinEvent representa o contrato canônico §5.1 do AGENTS.md.
type SpinEvent struct {
	V         int    `json:"v"`
	SpinID    string `json:"spin_id"`
	Number    int    `json:"number"`
	Ts        int64  `json:"ts"`
	Source    string `json:"source"`
	LatencyMs int64  `json:"latency_ms"`
}

// Validate verifica invariantes do evento canônico §5.1.
func (e SpinEvent) Validate() error {
	if e.V != 1 {
		return fmt.Errorf("unsupported version: %d", e.V)
	}
	if e.SpinID == "" {
		return errors.New("empty spin_id")
	}
	if e.Number < 0 || e.Number > 36 {
		return fmt.Errorf("number out of range 0..=36: %d", e.Number)
	}
	switch e.Source {
	case "ws", "mo", "ext", "ocr":
	default:
		return fmt.Errorf("invalid source: %s", e.Source)
	}
	return nil
}

// Hub gerencia deduplicação canônica, estado de atuação CDP e fanout NDJSON para assinantes de GET /stream.
type Hub struct {
	mu             sync.Mutex
	seenSpinID     map[string]struct{}
	seenBucket     map[string]struct{}
	subs           map[chan []byte]struct{}
	totalSpins     atomic.Uint64
	lastEventAt    atomic.Int64
	degraded       atomic.Bool
	cdpClient      *cdp.Client
	playtechSid    string
	lastBetCents   atomic.Int64
	lastBalCents   atomic.Int64
}

// NewHub inicializa o hub de eventos de spin.
func NewHub() *Hub {
	return &Hub{
		seenSpinID: make(map[string]struct{}),
		seenBucket: make(map[string]struct{}),
		subs:       make(map[chan []byte]struct{}),
	}
}

// Publish valida, deduplica por spin_id e por (number, ts/50) e envia para todos os clientes NDJSON.
func (h *Hub) Publish(ev SpinEvent) (bool, error) {
	if err := ev.Validate(); err != nil {
		return false, err
	}
	bucketKey := strconv.Itoa(ev.Number) + ":" + strconv.FormatInt(ev.Ts/50, 10)

	h.mu.Lock()
	if _, exists := h.seenSpinID[ev.SpinID]; exists {
		h.mu.Unlock()
		return false, nil
	}
	if _, exists := h.seenBucket[bucketKey]; exists {
		h.mu.Unlock()
		return false, nil
	}
	h.seenSpinID[ev.SpinID] = struct{}{}
	h.seenBucket[bucketKey] = struct{}{}

	line, err := json.Marshal(ev)
	if err != nil {
		h.mu.Unlock()
		return false, err
	}
	line = append(line, '\n')

	for ch := range h.subs {
		select {
		case ch <- line:
		default:
		}
	}
	h.mu.Unlock()

	h.totalSpins.Add(1)
	h.lastEventAt.Store(time.Now().UnixMilli())
	return true, nil
}

func (h *Hub) handlePostSpin(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	defer r.Body.Close()

	var ev SpinEvent
	if err := json.NewDecoder(r.Body).Decode(&ev); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	published, err := h.Publish(ev)
	if err != nil {
		http.Error(w, err.Error(), http.StatusUnprocessableEntity)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = fmt.Fprintf(w, `{"accepted":true,"published":%t}`+"\n", published)
}

func (h *Hub) handleStream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.WriteHeader(http.StatusOK)
	flusher.Flush()

	ch := make(chan []byte, 64)
	h.mu.Lock()
	h.subs[ch] = struct{}{}
	h.mu.Unlock()

	defer func() {
		h.mu.Lock()
		delete(h.subs, ch)
		h.mu.Unlock()
	}()

	ctx := r.Context()
	keepalive := time.NewTicker(15 * time.Second)
	defer keepalive.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-keepalive.C:
			if _, err := w.Write([]byte("\n")); err != nil {
				return
			}
			flusher.Flush()
		case payload := <-ch:
			if _, err := w.Write(payload); err != nil {
				return
			}
			flusher.Flush()
		}
	}
}

func (h *Hub) handleHealth(w http.ResponseWriter, _ *http.Request) {
	state := "OK"
	if h.degraded.Load() {
		state = "DEGRADED"
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = fmt.Fprintf(
		w,
		`{"status":%q,"total_spins":%d,"last_event_at":%d}`+"\n",
		state,
		h.totalSpins.Load(),
		h.lastEventAt.Load(),
	)
}

type clickRequest struct {
	Target string `json:"target"`
	X      int    `json:"x"`
	Y      int    `json:"y"`
}

func (h *Hub) handleActClick(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	defer r.Body.Close()

	var req clickRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	h.mu.Lock()
	client := h.cdpClient
	sid := h.playtechSid
	h.mu.Unlock()

	if client == nil || sid == "" {
		http.Error(w, "playtech cdp session not ready", http.StatusServiceUnavailable)
		return
	}
	if err := client.DispatchTargetClick(sid, req.Target, req.X, req.Y); err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte(`{"ok":true}` + "\n"))
}

func (h *Hub) handleActPreview(w http.ResponseWriter, _ *http.Request) {
	h.mu.Lock()
	client := h.cdpClient
	sid := h.playtechSid
	h.mu.Unlock()

	if client != nil && sid != "" {
		_ = client.RequestSlipRead(sid)
		time.Sleep(35 * time.Millisecond)
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = fmt.Fprintf(
		w,
		`{"preview_cents":%d,"balance_cents":%d}`+"\n",
		h.lastBetCents.Load(),
		h.lastBalCents.Load(),
	)
}

// classifyPlaytechSocket decodifica a URL (incluindo túnel ?u=wss%3A%2F%2F...) e aplica a whitelist estrita.
// Retorna apenas "eurolive-frontend.playtechgaming.com" (primário) ou "ielive-gateway.ptielive.com" (standby).
func classifyPlaytechSocket(rawURL string) (string, bool) {
	if rawURL == "" {
		return "", false
	}
	decoded, err := url.QueryUnescape(rawURL)
	if err != nil {
		decoded = rawURL
	}
	lower := strings.ToLower(decoded + " " + rawURL)
	if strings.Contains(lower, "eurolive-frontend.playtechgaming.com") {
		return wsdecap.PrimaryPlaytechHost, true
	}
	if strings.Contains(lower, "ielive-gateway.ptielive.com") {
		return wsdecap.StandbyPlaytechHost, true
	}
	return "", false
}

func writeFrozenSchema(schemaPath, fieldPath string) error {
	nowDate := time.Now().Format("2006-01-02")
	content := fmt.Sprintf(`# SPDX-License-Identifier: Apache-2.0
# Caminho protobuf congelado via --freeze em %s.
# Host primário declarado: %s (standby: %s).

[protobuf]
frozen_date = %q
primary_host = %q
standby_host = %q
winning_number_path = %q
round_id_path = "/3/11"
`,
		nowDate,
		wsdecap.PrimaryPlaytechHost,
		wsdecap.StandbyPlaytechHost,
		nowDate,
		wsdecap.PrimaryPlaytechHost,
		wsdecap.StandbyPlaytechHost,
		fieldPath,
	)
	if err := os.MkdirAll(filepath.Dir(schemaPath), 0o755); err != nil {
		return err
	}
	return os.WriteFile(schemaPath, []byte(content), 0o644)
}

func appendOpLog(logsDir, reqID string, tsMs int64, raw []byte) {
	if err := os.MkdirAll(logsDir, 0o755); err != nil {
		return
	}
	f, err := os.OpenFile(filepath.Join(logsDir, "oplog.bin"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()

	reqBytes := []byte(reqID)
	hdr := make([]byte, 2+8+4)
	binary.LittleEndian.PutUint16(hdr[0:2], uint16(len(reqBytes)))
	binary.LittleEndian.PutUint64(hdr[2:10], uint64(tsMs))
	binary.LittleEndian.PutUint32(hdr[10:14], uint32(len(raw)))
	_, _ = f.Write(hdr)
	_, _ = f.Write(reqBytes)
	_, _ = f.Write(raw)
}

func dumpHandshakeMeta(logsDir, reqID, host string, params map[string]any) {
	if err := os.MkdirAll(logsDir, 0o755); err != nil {
		return
	}
	safeReqID := strings.Map(func(r rune) rune {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '-' || r == '_' {
			return r
		}
		return '_'
	}, reqID)
	meta := map[string]any{
		"request_id": reqID,
		"host":       host,
		"ts_ms":      time.Now().UnixMilli(),
		"request":    params["request"],
	}
	b, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(filepath.Join(logsDir, fmt.Sprintf("handshake-%s.json", safeReqID)), b, 0o600)
}

type moBindingPayload struct {
	T            string `json:"t"`
	N            int    `json:"n"`
	Rid          string `json:"rid"`
	Src          string `json:"src"`
	At           int64  `json:"at"`
	B64          string `json:"b64"`
	URL          string `json:"url"`
	BetCents     *int64 `json:"bet_cents"`
	BalanceCents *int64 `json:"balance_cents"`
	Target       string `json:"target"`
	X            int    `json:"x"`
	Y            int    `json:"y"`
	Found        bool   `json:"found"`
}

type captureOptions struct {
	cdpURL     string
	logsDir    string
	dumpOp     bool
	frozenPath string
}

func runCaptureLoop(ctx context.Context, opts captureOptions, corr *wsdecap.Correlator, hub *Hub) {
	var seq atomic.Uint64
	var recvDiagCount atomic.Uint64
	var sentOpCount atomic.Uint64
	var sockMu sync.RWMutex
	sockHostByReq := make(map[string]string)

	errLogger := func(kind, detail string) {
		log.Printf("[errors] kind=%s detail=%s", kind, detail)
	}

	client, err := cdp.ConnectWithBackoff(ctx, opts.cdpURL, errLogger)
	if err != nil {
		hub.degraded.Store(true)
		errLogger("cdp_fatal", err.Error())
		return
	}
	defer client.Close()

	hub.mu.Lock()
	hub.cdpClient = client
	hub.mu.Unlock()

	for {
		select {
		case <-ctx.Done():
			return
		case ev, ok := <-client.Events():
			if !ok {
				hub.degraded.Store(true)
				return
			}
			switch ev.Method {
			case "Target.attachedToTarget":
				if err := client.HandleAttached(ev); err != nil {
					errLogger("attach_target", err.Error())
				}
			case "Page.frameNavigated", "Page.loadEventFired":
				client.ReEvaluateObserver(ev.SessionID)
			case "Network.webSocketCreated":
				reqID, _ := ev.Params["requestId"].(string)
				wsURL, _ := ev.Params["url"].(string)
				if host, allowed := classifyPlaytechSocket(wsURL); allowed && reqID != "" {
					sockMu.Lock()
					sockHostByReq[reqID] = host
					sockMu.Unlock()
					if ev.SessionID != "" {
						hub.mu.Lock()
						hub.playtechSid = ev.SessionID
						hub.mu.Unlock()
					}
					role := "PRIMÁRIO"
					if host == wsdecap.StandbyPlaytechHost {
						role = "STANDBY"
					}
					log.Printf("[ws-whitelist] socket aceito (%s) reqId=%s host=%s", role, reqID, host)
				}
			case "Network.webSocketWillSendHandshakeRequest":
				reqID, _ := ev.Params["requestId"].(string)
				sockMu.RLock()
				host, allowed := sockHostByReq[reqID]
				sockMu.RUnlock()
				if allowed && opts.dumpOp {
					dumpHandshakeMeta(opts.logsDir, reqID, host, ev.Params)
				}
			case "Network.webSocketFrameSent":
				reqID, _ := ev.Params["requestId"].(string)
				sockMu.RLock()
				host, allowed := sockHostByReq[reqID]
				sockMu.RUnlock()
				if !allowed {
					continue
				}
				resp, ok := ev.Params["response"].(map[string]any)
				if !ok {
					continue
				}
				payloadB64, _ := resp["payloadData"].(string)
				if payloadB64 == "" {
					payloadB64, _ = resp["payload"].(string)
				}
				if payloadB64 == "" {
					continue
				}
				raw, err := decodeFramePayload(payloadB64)
				if err != nil || len(raw) <= wsdecap.MinFramePayloadLen {
					continue
				}
				nowMs := time.Now().UnixMilli()
				appendOpLog(opts.logsDir, reqID, nowMs, raw)
				if opts.dumpOp {
					opNum := sentOpCount.Add(1)
					fmt.Printf(
						"\n=== [DUMP-OP #%d] FrameSent reqId=%s host=%s len=%d ===\nHEX64: %s\nTREE:\n%s==========================================================\n",
						opNum,
						reqID,
						host,
						len(raw),
						wsdecap.HexHead(raw, 64),
						wsdecap.FormatTree(raw),
					)
				}
			case "Network.webSocketFrameReceived":
				reqID, _ := ev.Params["requestId"].(string)
				sockMu.RLock()
				host, allowed := sockHostByReq[reqID]
				sockMu.RUnlock()
				if !allowed {
					continue
				}
				resp, ok := ev.Params["response"].(map[string]any)
				if !ok {
					continue
				}
				payloadB64, _ := resp["payloadData"].(string)
				if payloadB64 == "" {
					continue
				}
				raw, err := decodeFramePayload(payloadB64)
				// Ignora frames de controle (ping/close <= 8 bytes) para não sujar a correlação
				if err != nil || len(raw) <= wsdecap.MinFramePayloadLen {
					continue
				}
				diagIdx := recvDiagCount.Add(1)
				if diagIdx <= 10 {
					log.Printf(
						"[ws-diag #%02d] host=%s len=%d head4=[%s]",
						diagIdx,
						host,
						len(raw),
						wsdecap.HexHead(raw, 4),
					)
				}
				corr.RecordFrameWithHost(time.Now().UnixMilli(), host, raw)
			case "Runtime.bindingCalled":
				name, _ := ev.Params["name"].(string)
				if name != "__emit" {
					continue
				}
				payloadStr, _ := ev.Params["payload"].(string)
				var mo moBindingPayload
				if err := json.Unmarshal([]byte(payloadStr), &mo); err != nil {
					continue
				}
				nowMs := time.Now().UnixMilli()
				if mo.T == "ws" && mo.B64 != "" {
					host, allowed := classifyPlaytechSocket(mo.URL)
					if !allowed {
						continue
					}
					raw, err := decodeFramePayload(mo.B64)
					if err == nil && len(raw) > wsdecap.MinFramePayloadLen {
						ts := mo.At
						if ts <= 0 {
							ts = nowMs
						}
						corr.RecordFrameWithHost(ts, host, raw)
					}
					continue
				}
				if mo.T == "click_resolved" {
					log.Printf("[click-cdp] target=%s -> (%d, %d) dom_found=%v", mo.Target, mo.X, mo.Y, mo.Found)
					if ev.SessionID != "" && mo.X > 0 && mo.Y > 0 {
						_ = client.DispatchClick(ev.SessionID, mo.X, mo.Y)
					}
					continue
				}
				if mo.T == "slip" {
					if mo.BetCents != nil {
						hub.lastBetCents.Store(*mo.BetCents)
					}
					if mo.BalanceCents != nil {
						hub.lastBalCents.Store(*mo.BalanceCents)
					}
					continue
				}
				if ev.SessionID != "" {
					hub.mu.Lock()
					hub.playtechSid = ev.SessionID
					hub.mu.Unlock()
				}
				if mo.N < 0 || mo.N > 36 {
					continue
				}
				if mo.At <= 0 {
					mo.At = nowMs
				}
				top := corr.OnConfirmedSpin(uint64(mo.N), mo.At)
				id := seq.Add(1)
				spinID := fmt.Sprintf("mo-%d-%d", mo.At, id)
				if mo.Rid != "" {
					spinID = "rid-" + mo.Rid
				}
				dualFeed, dualHits := corr.DualFeedStats()
				if len(top) > 0 {
					log.Printf(
						"[spin #%d] spin_id=%s numero=%d | lider=%s (%d/%d distinct=%d host=%s dual_feed=%v/%d)",
						id,
						spinID,
						mo.N,
						top[0].Path,
						top[0].Hits,
						top[0].Spins,
						top[0].Distinct,
						top[0].Host,
						dualFeed,
						dualHits,
					)
				} else {
					log.Printf("[spin #%d] spin_id=%s numero=%d ts=%d", id, spinID, mo.N, mo.At)
				}
				spinEv := SpinEvent{
					V:         1,
					SpinID:    spinID,
					Number:    mo.N,
					Ts:        mo.At,
					Source:    "mo",
					LatencyMs: maxInt64(0, nowMs-mo.At),
				}
				_, _ = hub.Publish(spinEv)
			}
		}
	}
}

func decodeFramePayload(payloadB64 string) ([]byte, error) {
	if raw, err := base64.StdEncoding.DecodeString(payloadB64); err == nil {
		return raw, nil
	}
	if raw, err := base64.RawStdEncoding.DecodeString(payloadB64); err == nil {
		return raw, nil
	}
	return nil, errors.New("invalid base64 payload")
}

func maxInt64(a, b int64) int64 {
	if a > b {
		return a
	}
	return b
}

func main() {
	cdpFlag := flag.String("cdp", "http://127.0.0.1:9222", "CDP HTTP/WS endpoint")
	wsProbeFlag := flag.Bool("ws-probe", false, "Enable protobuf path correlation ranking every 5 spins")
	strictFlag := flag.Bool("strict", false, "Require 40/40 hits and >=30 distinct numbers in probe")
	freezeFlag := flag.String("freeze", "", "Freeze winning protobuf path to config/schema.toml (e.g. /3/103/1/1)")
	dumpFramesFlag := flag.Int("dump-frames", 0, "Print full hex + decoded tree for the next N rounds")
	dumpOpFlag := flag.Bool("dump-op", false, "Capture and print client->server FrameSent protobuf trees and logs/oplog.bin")
	portFlag := flag.Int("port", 8787, "HTTP port for /spin and /stream")
	flag.Parse()

	schemaPath := filepath.Clean(filepath.Join("..", "config", "schema.toml"))
	logsDir := filepath.Clean(filepath.Join("..", "logs"))

	if *freezeFlag != "" {
		if err := writeFrozenSchema(schemaPath, *freezeFlag); err != nil {
			log.Fatalf("[freeze] falha ao gravar %s: %v", schemaPath, err)
		}
		log.Printf("[freeze] caminho %s congelado em %s (host primário=%s)", *freezeFlag, schemaPath, wsdecap.PrimaryPlaytechHost)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	hub := NewHub()
	corr := wsdecap.NewCorrelator(*wsProbeFlag || *strictFlag)
	corr.SetStrict(*strictFlag)
	corr.SetDumpFrames(*dumpFramesFlag)

	streamMux := http.NewServeMux()
	streamMux.HandleFunc("/spin", hub.handlePostSpin)
	streamMux.HandleFunc("/stream", hub.handleStream)
	streamMux.HandleFunc("/health", hub.handleHealth)
	streamMux.HandleFunc("/act/click", hub.handleActClick)
	streamMux.HandleFunc("/act/preview", hub.handleActPreview)

	streamServer := &http.Server{
		Addr:              fmt.Sprintf("127.0.0.1:%d", *portFlag),
		Handler:           streamMux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	healthMux := http.NewServeMux()
	healthMux.HandleFunc("/health", hub.handleHealth)
	healthServer := &http.Server{
		Addr:              "127.0.0.1:8790",
		Handler:           healthMux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	log.Printf(
		"[monster-sidecar] iniciando | stream=http://127.0.0.1:%d/stream | cdp=%s | ws-probe=%v | strict=%v | dump-frames=%d | dump-op=%v",
		*portFlag,
		*cdpFlag,
		*wsProbeFlag,
		*strictFlag,
		*dumpFramesFlag,
		*dumpOpFlag,
	)

	go func() {
		if err := healthServer.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Printf("[errors] kind=health_http detail=%v", err)
		}
	}()

	go func() {
		if err := streamServer.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Printf("[errors] kind=stream_http detail=%v", err)
		}
	}()

	go runCaptureLoop(
		ctx,
		captureOptions{
			cdpURL:     *cdpFlag,
			logsDir:    logsDir,
			dumpOp:     *dumpOpFlag,
			frozenPath: *freezeFlag,
		},
		corr,
		hub,
	)

	<-ctx.Done()
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_ = streamServer.Shutdown(shutdownCtx)
	_ = healthServer.Shutdown(shutdownCtx)
}
