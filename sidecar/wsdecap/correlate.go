// SPDX-License-Identifier: Apache-2.0
// Correlacionador entre frames WebSocket protobuf e emissões confirmadas do MutationObserver.
// Suporta deduplicação de dual-feed (eurolive-frontend primário vs ielive-gateway standby em <3000 ms)
// e modo --strict (exige 40/40 hits e >=30 números distintos para eliminar campos de cor 0/1).

package wsdecap

import (
	"fmt"
	"io"
	"os"
	"sort"
	"strings"
	"sync"
)

// MaxCorrelationWindowMs define a janela máxima (até 4500 ms antes / 1500 ms depois) entre frame WS e __emit.
const MaxCorrelationWindowMs int64 = 4500

// DualFeedWindowMs é a janela (< 3000 ms) para deduplicar a mesma rodada entregue pelos dois hosts Playtech.
const DualFeedWindowMs int64 = 3000

// PrimaryPlaytechHost é o host primário declarado; ielive-gateway atua como standby.
const PrimaryPlaytechHost = "eurolive-frontend.playtechgaming.com"

// StandbyPlaytechHost é o host secundário (failover) da Playtech.
const StandbyPlaytechHost = "ielive-gateway.ptielive.com"

type recordedFrame struct {
	tsMs    int64
	host    string
	raw     []byte
	cands   []Cand
}

// RankedPath representa um caminho protobuf, contagem de acertos, cardinalidade de valores e host vencedor.
type RankedPath struct {
	Path     string
	Hits     uint64
	Spins    uint64
	Distinct int
	Host     string
}

// Correlator correlaciona candidatos protobuf com resultados confirmados pela UI.
type Correlator struct {
	mu           sync.Mutex
	counts       map[string]uint64
	distinct     map[string]map[uint64]struct{}
	pathHost     map[string]string
	frames       []recordedFrame
	spins        uint64
	wsProbe      bool
	strictMode   bool
	dualFeed     bool
	dualFeedHits uint64
	dumpFrames   int
	dumpedCount  int
	out          io.Writer
}

// NewCorrelator cria uma nova instância de correlação.
func NewCorrelator(wsProbe bool) *Correlator {
	return &Correlator{
		counts:   make(map[string]uint64),
		distinct: make(map[string]map[uint64]struct{}),
		pathHost: make(map[string]string),
		wsProbe:  wsProbe,
		out:      os.Stdout,
	}
}

// SetStrict habilita o filtro estrito (40/40 e >= 30 números distintos).
func (c *Correlator) SetStrict(strict bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.strictMode = strict
}

// SetDumpFrames define quantas rodadas terão o frame vencedor impresso em hex + árvore protobuf.
func (c *Correlator) SetDumpFrames(n int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.dumpFrames = n
}

// SetOutput permite redirecionar a saída do relatório --ws-probe em testes.
func (c *Correlator) SetOutput(w io.Writer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.out = w
}

// DualFeedStats retorna se dual_feed foi detectado e quantas colisões entre hosts foram filtradas.
func (c *Correlator) DualFeedStats() (bool, uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.dualFeed, c.dualFeedHits
}

// RecordFrame registra candidatos usando o host primário padrão (compatível com testes unitários).
func (c *Correlator) RecordFrame(tsMs int64, payload []byte) {
	c.RecordFrameWithHost(tsMs, PrimaryPlaytechHost, payload)
}

// RecordFrameWithHost registra um frame binário identificando qual host do socket o entregou.
func (c *Correlator) RecordFrameWithHost(tsMs int64, host string, payload []byte) {
	cands := Walk(payload)
	if len(cands) == 0 {
		return
	}
	if host == "" {
		host = PrimaryPlaytechHost
	}
	rawCopy := make([]byte, len(payload))
	copy(rawCopy, payload)

	c.mu.Lock()
	defer c.mu.Unlock()
	c.frames = append(c.frames, recordedFrame{
		tsMs:  tsMs,
		host:  host,
		raw:   rawCopy,
		cands: cands,
	})
	if len(c.frames) > 2048 {
		c.frames = c.frames[len(c.frames)-1024:]
	}
}

// OnConfirmedSpin correlaciona um número confirmado (0..=36) em emitTsMs com deduplicação por rodada/host.
func (c *Correlator) OnConfirmedSpin(number uint64, emitTsMs int64) []RankedPath {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.spins++
	seenInSpin := make(map[string]string) // path -> host que pontuou nesta rodada
	var winningFrameRaw []byte
	var winningFrameHost string

	// Verifica se tanto o host primário quanto o standby entregaram frames na janela de <3000 ms
	hasPrimaryInWindow := false
	hasStandbyInWindow := false
	for _, f := range c.frames {
		diff := emitTsMs - f.tsMs
		if diff >= -1500 && diff <= DualFeedWindowMs {
			if strings.Contains(f.host, "eurolive-frontend") {
				hasPrimaryInWindow = true
			} else if strings.Contains(f.host, "ielive-gateway") {
				hasStandbyInWindow = true
			}
		}
	}
	if hasPrimaryInWindow && hasStandbyInWindow {
		c.dualFeed = true
	}

	for _, f := range c.frames {
		diff := emitTsMs - f.tsMs
		if diff < -1500 || diff > MaxCorrelationWindowMs {
			continue
		}
		// Se ambos os feeds estão ativos na janela (<3000 ms), usa apenas o primário (eurolive-frontend)
		if hasPrimaryInWindow && hasStandbyInWindow && strings.Contains(f.host, "ielive-gateway") {
			c.dualFeedHits++
			continue
		}
		for _, cand := range f.cands {
			if cand.Val == number {
				if prevHost, dup := seenInSpin[cand.Path]; dup {
					if prevHost != f.host {
						c.dualFeed = true
						c.dualFeedHits++
					}
					continue
				}
				seenInSpin[cand.Path] = f.host
				c.counts[cand.Path]++
				c.pathHost[cand.Path] = f.host
				if c.distinct[cand.Path] == nil {
					c.distinct[cand.Path] = make(map[uint64]struct{})
				}
				c.distinct[cand.Path][number] = struct{}{}
				if winningFrameRaw == nil && (cand.Path == "/3/103/1/1" || strings.HasPrefix(cand.Path, "/3/")) {
					winningFrameRaw = f.raw
					winningFrameHost = f.host
				}
			}
		}
	}

	if c.dumpFrames > 0 && c.dumpedCount < c.dumpFrames && len(winningFrameRaw) > 0 {
		c.dumpedCount++
		fmt.Fprintf(
			c.out,
			"\n=== DUMP FRAME RODADA #%d (num=%d host=%s len=%d) ===\nHEX: %s\nTREE:\n%s====================================================\n",
			c.spins,
			number,
			winningFrameHost,
			len(winningFrameRaw),
			HexHead(winningFrameRaw, len(winningFrameRaw)),
			FormatTree(winningFrameRaw),
		)
	}

	top := c.topLocked(10)
	if c.wsProbe && c.spins%5 == 0 {
		c.printTopLocked(top)
	}
	return top
}

// Top retorna os n caminhos com maior contagem de acertos.
func (c *Correlator) Top(n int) []RankedPath {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.topLocked(n)
}

func (c *Correlator) topLocked(n int) []RankedPath {
	list := make([]RankedPath, 0, len(c.counts))
	for path, hits := range c.counts {
		distinctCount := len(c.distinct[path])
		if c.strictMode && c.spins >= 40 && (hits < c.spins || distinctCount < 30) {
			continue
		}
		list = append(list, RankedPath{
			Path:     path,
			Hits:     hits,
			Spins:    c.spins,
			Distinct: distinctCount,
			Host:     c.pathHost[path],
		})
	}
	sort.Slice(list, func(i, j int) bool {
		if list[i].Hits != list[j].Hits {
			return list[i].Hits > list[j].Hits
		}
		if list[i].Distinct != list[j].Distinct {
			return list[i].Distinct > list[j].Distinct
		}
		return list[i].Path < list[j].Path
	})
	if len(list) > n {
		list = list[:n]
	}
	return list
}

func (c *Correlator) printTopLocked(top []RankedPath) {
	fmt.Fprintf(
		c.out,
		"=== WS_PROBE TOP %d (spins=%d dual_feed=%v filtered_dups=%d) ===\n",
		len(top),
		c.spins,
		c.dualFeed,
		c.dualFeedHits,
	)
	for i, item := range top {
		fmt.Fprintf(
			c.out,
			"#%02d path=%s hits=%d/%d distinct=%d host=%s\n",
			i+1,
			item.Path,
			item.Hits,
			item.Spins,
			item.Distinct,
			item.Host,
		)
	}
}
