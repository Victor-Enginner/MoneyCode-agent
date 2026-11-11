// SPDX-License-Identifier: Apache-2.0
// Testes unitários do walker protobuf genérico.
// Verifica extração do candidato 17 a partir dos bytes fixos [0x08, 0x11] e mensagens aninhadas.
// Garante respeito ao limite de profundidade 8 e filtro 0..=36.

package wsdecap

import "testing"

func TestWalkFixedBytes17(t *testing.T) {
	payload := []byte{0x08, 0x11}
	cands := Walk(payload)
	if len(cands) != 1 {
		t.Fatalf("expected 1 candidate, got %d: %+v", len(cands), cands)
	}
	if cands[0].Val != 17 {
		t.Fatalf("expected candidate value 17, got %d", cands[0].Val)
	}
	if cands[0].Path != "/1" {
		t.Fatalf("expected candidate path /1, got %q", cands[0].Path)
	}
}

func TestWalkNestedAndRangeFilter(t *testing.T) {
	// field 4 (wire 2, len 4) -> field 1 (wire 2, len 2) -> field 2 (wire 0, val 36)
	// plus field 5 (wire 0, val 99 -> out of range 0..=36)
	payload := []byte{0x22, 0x04, 0x0a, 0x02, 0x10, 0x24, 0x28, 0x63}
	cands := Walk(payload)
	if len(cands) != 1 {
		t.Fatalf("expected 1 valid candidate, got %d: %+v", len(cands), cands)
	}
	if cands[0].Path != "/4/1/2" || cands[0].Val != 36 {
		t.Fatalf("unexpected candidate: %+v", cands[0])
	}
}
