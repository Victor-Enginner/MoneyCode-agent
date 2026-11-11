// SPDX-License-Identifier: Apache-2.0
// Ponto de entrada do sidecar de captura do projeto MONSTER.
// Expõe POST /spin, GET /stream (NDJSON fanout com deduplicação) e GET /health em 127.0.0.1:8790.
// Coordena o cliente CDP (auto-attach + observer + WS protobuf) e o modo --ws-probe.

package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
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

// Hub gerencia deduplicação canônica e fanout NDJSON para assinantes de GET /stream.
type Hub struct {
	mu          sync.Mutex
	seenSpinID  map[string]struct{}
	seenBucket  map[string]struct{}
	subs        map[chan []byte]struct{}
	totalSpins  atomic.Uint64
	lastEventAt atomic.Int64
	degraded    atomic.Bool
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
	for {
		select {
		case <-ctx.Done():
			return
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

type moBindingPayload struct {
	T   string `json:"t"`
	N   int    `json:"n"`
	Src string `json:"src"`
	At  int64  `json:"at"`
}

func runCaptureLoop(ctx context.Context, cdpURL string, corr *wsdecap.Correlator, hub *Hub) {
	var seq atomic.Uint64
	errLogger := func(kind, detail string) {
		log.Printf("[errors] kind=%s detail=%s", kind, detail)
	}

	client, err := cdp.ConnectWithBackoff(ctx, cdpURL, errLogger)
	if err != nil {
		hub.degraded.Store(true)
		errLogger("cdp_fatal", err.Error())
		return
	}
	defer client.Close()

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
			case "Network.webSocketFrameReceived":
				resp, ok := ev.Params["response"].(map[string]any)
				if !ok {
					continue
				}
				payloadB64, _ := resp["payloadData"].(string)
				if payloadB64 == "" {
					continue
				}
				raw, err := base64.StdEncoding.DecodeString(payloadB64)
				if err != nil {
					continue
				}
				corr.RecordFrame(time.Now().UnixMilli(), raw)
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
				if mo.N < 0 || mo.N > 36 {
					continue
				}
				nowMs := time.Now().UnixMilli()
				if mo.At <= 0 {
					mo.At = nowMs
				}
				corr.OnConfirmedSpin(uint64(mo.N), mo.At)
				id := seq.Add(1)
				spinEv := SpinEvent{
					V:         1,
					SpinID:    fmt.Sprintf("mo-%d-%d", mo.At, id),
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

func maxInt64(a, b int64) int64 {
	if a > b {
		return a
	}
	return b
}

func main() {
	cdpFlag := flag.String("cdp", "http://127.0.0.1:9222", "CDP HTTP/WS endpoint")
	wsProbeFlag := flag.Bool("ws-probe", false, "Enable protobuf path correlation ranking every 5 spins")
	portFlag := flag.Int("port", 8787, "HTTP port for /spin and /stream")
	flag.Parse()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	hub := NewHub()
	corr := wsdecap.NewCorrelator(*wsProbeFlag)

	streamMux := http.NewServeMux()
	streamMux.HandleFunc("/spin", hub.handlePostSpin)
	streamMux.HandleFunc("/stream", hub.handleStream)
	streamMux.HandleFunc("/health", hub.handleHealth)

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

	go runCaptureLoop(ctx, *cdpFlag, corr, hub)

	<-ctx.Done()
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_ = streamServer.Shutdown(shutdownCtx)
	_ = healthServer.Shutdown(shutdownCtx)
}
