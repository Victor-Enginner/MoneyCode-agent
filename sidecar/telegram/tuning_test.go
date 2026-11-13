// SPDX-License-Identifier: Apache-2.0
// ! Testes do ajuste de padrões pelo Telegram: interruptor, faixa e resolução de
// ! id curto — o operador digita no celular, então alias e ambiguidade contam.
package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestSetDisabledLigaDesligaUmaVez(t *testing.T) {
	var doc tuningDoc
	if !setDisabled(&doc, "cluster_terminal", true) {
		t.Fatal("desligar deveria mudar o estado")
	}
	if len(doc.Disabled) != 1 || doc.Disabled[0] != "cluster_terminal" {
		t.Fatalf("lista de desligados inesperada: %v", doc.Disabled)
	}
	if setDisabled(&doc, "cluster_terminal", true) {
		t.Fatal("desligar duas vezes não é mudança")
	}
	if !setDisabled(&doc, "cluster_terminal", false) {
		t.Fatal("ligar deveria mudar o estado")
	}
	if len(doc.Disabled) != 0 {
		t.Fatalf("lista deveria esvaziar: %v", doc.Disabled)
	}
	if setDisabled(&doc, "cluster_terminal", false) {
		t.Fatal("ligar um já ligado não é mudança")
	}
}

func TestSetRangeSohNosMatchersComFaixa(t *testing.T) {
	var doc tuningDoc
	if !setRange(&doc, "quebra_permanencia_coluna", 3, 5) {
		t.Fatal("coluna tem faixa")
	}
	if doc.Coluna.Min != 3 || doc.Coluna.Max != 5 {
		t.Fatalf("faixa não aplicada: %+v", doc.Coluna)
	}
	if setRange(&doc, "cluster_terminal", 3, 5) {
		t.Fatal("cluster terminal não usa faixa de repetição")
	}
}

func TestResolveMatcherIDComAliasEPrefixo(t *testing.T) {
	ids := []string{
		"quebra_permanencia_coluna", "quebra_permanencia_duzia", "quebra_cor_6x",
		"quebra_alto_baixo_6x", "quebra_par_impar_6x", "setor_vies", "setor_vies_duzia",
		"fibonacci_lag", "cluster_terminal", "cluster_final",
	}
	cases := map[string]string{
		"col":                      "quebra_permanencia_coluna",
		"PAR":                      "quebra_par_impar_6x",
		"cluster":                  "cluster_terminal",
		"cluster_final":            "cluster_final",
		"quebra_permanencia_duzia": "quebra_permanencia_duzia",
		"fib":                      "fibonacci_lag",
	}
	for arg, want := range cases {
		got, ok := resolveMatcherID(arg, ids)
		if !ok || got != want {
			t.Fatalf("resolveMatcherID(%q) = %q, %v; quer %q", arg, got, ok, want)
		}
	}
	if _, ok := resolveMatcherID("cluster_", ids); ok {
		t.Fatal("prefixo ambíguo (cluster_terminal/cluster_final) precisa ser recusado")
	}
	if _, ok := resolveMatcherID("inexistente", ids); ok {
		t.Fatal("id desconhecido precisa ser recusado")
	}
}

func TestTuningTextMostraEstadoEContagem(t *testing.T) {
	env := tuningEnvelope{
		Matchers: []string{"quebra_permanencia_coluna", "cluster_terminal"},
		Current: tuningDoc{
			Coluna:   tuningRange{Min: 4, Max: 6},
			Disabled: []string{"cluster_terminal"},
		},
	}
	out := tuningText(env)
	if !strings.Contains(out, "1/2 ligados") {
		t.Fatalf("contagem errada: %q", out)
	}
	if !strings.Contains(out, "ON  quebra_permanencia_coluna      4..=6") {
		t.Fatalf("linha da coluna inesperada: %q", out)
	}
	if !strings.Contains(out, "OFF cluster_terminal") {
		t.Fatalf("cluster deveria aparecer desligado: %q", out)
	}
}

func TestFetchApplyTuningFalamComOCore(t *testing.T) {
	var recebido tuningDoc
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/tuning" {
			t.Errorf("path inesperado: %s", r.URL.Path)
		}
		switch r.Method {
		case http.MethodGet:
			_ = json.NewEncoder(w).Encode(tuningEnvelope{
				Current:  tuningDoc{Coluna: tuningRange{Min: 4, Max: 6}},
				Matchers: []string{"quebra_permanencia_coluna", "cluster_terminal"},
			})
		case http.MethodPost:
			_ = json.NewDecoder(r.Body).Decode(&recebido)
			w.WriteHeader(http.StatusBadRequest)
			_, _ = w.Write([]byte(`{"ok":false,"error":"coluna: máximo (3) é menor que o mínimo (9)"}`))
		}
	}))
	defer srv.Close()

	env, err := fetchTuning(srv.URL)
	if err != nil {
		t.Fatalf("fetchTuning: %v", err)
	}
	if len(env.Matchers) != 2 || env.Current.Coluna.Min != 4 {
		t.Fatalf("envelope inesperado: %+v", env)
	}

	err = applyTuning(srv.URL, env.Current)
	if err == nil || !strings.Contains(err.Error(), "menor que o mínimo") {
		t.Fatalf("rejeição do core precisa virar erro legível: %v", err)
	}
	if recebido.Coluna.Min != 4 || recebido.Coluna.Max != 6 {
		t.Fatalf("documento enviado incompleto: %+v", recebido)
	}
}
