// SPDX-License-Identifier: Apache-2.0
// ! Testes do sidecar de Telegram: formatação de dinheiro em centavos (nunca
// ! float, §1.1) e leitura da escada vinda do `/api/tick` do core.
package main

import (
	"strings"
	"testing"
)

func TestCentsIFormatting(t *testing.T) {
	cases := map[int64]string{
		0:     "R$ 0,00",
		10:    "R$ 0,10",
		520:   "R$ 5,20",
		7067:  "R$ 70,67",
		-1600: "-R$ 16,00",
	}
	for in, want := range cases {
		if got := centsI(in); got != want {
			t.Fatalf("centsI(%d) = %q, quer %q", in, got, want)
		}
	}
}

func TestCentsHandleMissingValuesAsDash(t *testing.T) {
	if got := cents(nil); got != "—" {
		t.Fatalf("cents(nil) = %q, quer —", got)
	}
	if got := centsU(nil); got != "—" {
		t.Fatalf("centsU(nil) = %q, quer —", got)
	}
}

func TestLadderTextMarksActiveStep(t *testing.T) {
	var tick tick
	maxSteps := uint8(3)
	blow := uint64(520)
	tick.Ladder.MaxSteps = &maxSteps
	tick.Ladder.BlowCostCents = &blow
	tick.Ladder.Rows = append(tick.Ladder.Rows, struct {
		Step               uint8  `json:"step"`
		StakePerCoverCents uint64 `json:"stake_per_cover_cents"`
		ExposureCents      uint64 `json:"exposure_cents"`
		CumLossCents       uint64 `json:"cum_loss_cents"`
		Active             bool   `json:"active"`
	}{Step: 2, StakePerCoverCents: 60, ExposureCents: 120, CumLossCents: 80, Active: true})

	out := ladderText(tick)
	if !strings.Contains(out, "3 degraus") {
		t.Fatalf("escada sem contagem de degraus: %q", out)
	}
	if !strings.HasPrefix(out, "Escada 3 degraus · estouro R$ 5,20") {
		t.Fatalf("cabeçalho inesperado: %q", out)
	}
	if !strings.Contains(out, "▸ 2 · ficha R$ 0,60") {
		t.Fatalf("degrau ativo não marcado: %q", out)
	}
}
