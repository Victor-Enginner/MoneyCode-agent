// SPDX-License-Identifier: Apache-2.0
// Testes unitários do correlacionador de caminhos protobuf (--ws-probe).
// Valida janela de ±1500 ms e impressão dos 10 melhores caminhos a cada 5 spins.

package wsdecap

import (
	"bytes"
	"strings"
	"testing"
)

func TestCorrelatorMatchesNearestFrameWithin1500MsAndPrintsEvery5Spins(t *testing.T) {
	var buf bytes.Buffer
	corr := NewCorrelator(true)
	corr.SetOutput(&buf)

	for i := 1; i <= 5; i++ {
		ts := int64(i * 10_000)
		num := byte(i * 3)
		// field 1 = num, field 2 = 35 (noise)
		frame := []byte{0x08, num, 0x10, 35}
		corr.RecordFrame(ts-200, frame)
		corr.OnConfirmedSpin(uint64(num), ts)
	}

	top := corr.Top(10)
	if len(top) == 0 {
		t.Fatal("expected ranked paths, got none")
	}
	if top[0].Path != "/1" || top[0].Hits != 5 {
		t.Fatalf("expected /1 with 5 hits, got %+v", top[0])
	}
	if !strings.Contains(buf.String(), "WS_PROBE TOP") || !strings.Contains(buf.String(), "path=/1 hits=5/5") {
		t.Fatalf("expected ws-probe output after 5 spins, got %q", buf.String())
	}
}
