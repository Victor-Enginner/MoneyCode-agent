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

func TestSettlementTextNoFormatoDeGrupo(t *testing.T) {
	var tk tick
	bank := int64(5127)
	pnl := int64(120)
	tk.Session.BankrollCents = &bank
	tk.Session.PnlCents = &pnl
	win := settlement{StrategyID: "quebra_permanencia_duzia", Won: true, DeltaCents: 20, Step: 1, MaxSteps: 3, Number: 15, Ts: 10}
	out := settlementText(tk, win, 3, 1)
	for _, want := range []string{
		"🟢 OPERAÇÃO BEM SUCEDIDA ✅",
		"🎲 Número: 15",
		"degrau 1/3",
		"+R$ 0,20",
		"🟢 3 | 🔴 1",
		"taxa 75.0%",
		"Banca R$ 51,27",
	} {
		if !strings.Contains(out, want) {
			t.Fatalf("mensagem sem %q:\n%s", want, out)
		}
	}

	loss := settlement{StrategyID: "cluster_terminal", Won: false, DeltaCents: -40, Step: 2, MaxSteps: 3, Number: 0, Ts: 11}
	out = settlementText(tk, loss, 0, 1)
	if !strings.Contains(out, "🔴 OPERAÇÃO MAL SUCEDIDA ❌") || !strings.Contains(out, "-R$ 0,40") {
		t.Fatalf("derrota formatada errado:\n%s", out)
	}
}

func TestDivergenceTextMostraEsperadoMesaEDiferenca(t *testing.T) {
	out := divergenceText(divergence{StrategyID: "quebra_permanencia_coluna", Number: 22, ExpectedCents: 4460, ObservedCents: 4440, DiffCents: -20, Ts: 1})
	for _, want := range []string{"EXTRATO DIVERGENTE", "quebra_permanencia_coluna", "saiu 22", "44,60", "44,40", "-"} {
		if !strings.Contains(out, want) {
			t.Fatalf("faltou %q em:\n%s", want, out)
		}
	}
}

func TestCautionTextSoAvisaNaMudanca(t *testing.T) {
	if got := cautionText("", "escada chegou ao degrau 4"); !strings.Contains(got, "LIGADO") || !strings.Contains(got, "degrau 4") {
		t.Fatalf("ligar: %q", got)
	}
	if got := cautionText("x", ""); !strings.Contains(got, "DESLIGADO") {
		t.Fatalf("desligar: %q", got)
	}
	if got := cautionText("a", "b"); got != "" {
		t.Fatalf("troca de motivo nao deve avisar: %q", got)
	}
	var tk tick
	if cautionOf(tk) != "" {
		t.Fatal("nulo deve ser desligado")
	}
}
