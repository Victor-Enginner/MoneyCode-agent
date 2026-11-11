// SPDX-License-Identifier: Apache-2.0
// Correlacionador entre frames WebSocket protobuf e emissões confirmadas do MutationObserver.
// Mantém tabela map[path]counter para o frame mais próximo na janela de ±1500 ms.
// No modo --ws-probe, imprime os 10 caminhos mais pontuados a cada 5 spins.

package wsdecap

import (
	"fmt"
	"io"
	"os"
	"sort"
	"sync"
)

// MaxCorrelationWindowMs define a janela máxima (até 4500 ms antes / 1500 ms depois) entre frame WS e evento __emit.
const MaxCorrelationWindowMs int64 = 4500

type recordedFrame struct {
	tsMs  int64
	cands []Cand
}

// RankedPath representa um caminho protobuf e sua contagem acumulada de acertos.
type RankedPath struct {
	Path  string
	Hits  uint64
	Spins uint64
}

// Correlator correlaciona candidatos protobuf com resultados confirmados pela UI.
type Correlator struct {
	mu      sync.Mutex
	counts  map[string]uint64
	frames  []recordedFrame
	spins   uint64
	wsProbe bool
	out     io.Writer
}

// NewCorrelator cria uma nova instância de correlação.
func NewCorrelator(wsProbe bool) *Correlator {
	return &Correlator{
		counts:  make(map[string]uint64),
		wsProbe: wsProbe,
		out:     os.Stdout,
	}
}

// SetOutput permite redirecionar a saída do relatório --ws-probe em testes.
func (c *Correlator) SetOutput(w io.Writer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.out = w
}

// RecordFrame registra os candidatos extraídos de um frame binário recebido em tsMs.
func (c *Correlator) RecordFrame(tsMs int64, payload []byte) {
	cands := Walk(payload)
	if len(cands) == 0 {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.frames = append(c.frames, recordedFrame{
		tsMs:  tsMs,
		cands: cands,
	})
	if len(c.frames) > 2048 {
		c.frames = c.frames[len(c.frames)-1024:]
	}
}

// OnConfirmedSpin correlaciona um número confirmado (0..=36) em emitTsMs com os frames na janela de correlação.
func (c *Correlator) OnConfirmedSpin(number uint64, emitTsMs int64) []RankedPath {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.spins++
	seenInSpin := make(map[string]struct{})

	for _, f := range c.frames {
		diff := emitTsMs - f.tsMs
		if diff < -1500 || diff > MaxCorrelationWindowMs {
			continue
		}
		for _, cand := range f.cands {
			if cand.Val == number {
				if _, dup := seenInSpin[cand.Path]; !dup {
					seenInSpin[cand.Path] = struct{}{}
					c.counts[cand.Path]++
				}
			}
		}
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
		list = append(list, RankedPath{
			Path:  path,
			Hits:  hits,
			Spins: c.spins,
		})
	}
	sort.Slice(list, func(i, j int) bool {
		if list[i].Hits != list[j].Hits {
			return list[i].Hits > list[j].Hits
		}
		return list[i].Path < list[j].Path
	})
	if len(list) > n {
		list = list[:n]
	}
	return list
}

func (c *Correlator) printTopLocked(top []RankedPath) {
	fmt.Fprintf(c.out, "=== WS_PROBE TOP %d (spins=%d) ===\n", len(top), c.spins)
	for i, item := range top {
		fmt.Fprintf(c.out, "#%02d path=%s hits=%d/%d\n", i+1, item.Path, item.Hits, item.Spins)
	}
}
