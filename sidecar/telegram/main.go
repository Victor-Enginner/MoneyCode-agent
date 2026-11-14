// SPDX-License-Identifier: Apache-2.0
// ! Sidecar de Telegram do MONSTER: long polling na Bot API, comandos do §11 e
// ! alertas apenas para entrada executada, estouro de escada, stop loss, fonte
// ! morta e divergência. Só lê o core por HTTP (`/api/tick`) e nunca toca em CDP.
package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	// apiBase é a raiz da Bot API. Trocar só serve para espelhos de teste.
	apiBase = "https://api.telegram.org"
	// queueSize limita a fila não-bloqueante de envio; acima disso a mensagem é
	// descartada com log, porque alerta atrasado vale menos que captura viva (§7).
	queueSize = 64
	// pollTimeout é o long polling da Bot API em segundos.
	pollTimeout = 25
)

// config é o arquivo local (gitignored) com credenciais do bot.
type config struct {
	Token  string `json:"token"`
	ChatID int64  `json:"chat_id"`
	Core   string `json:"core"`
}

type update struct {
	UpdateID int64 `json:"update_id"`
	Message  *struct {
		MessageID int64 `json:"message_id"`
		Chat      struct {
			ID int64 `json:"id"`
		} `json:"chat"`
		Text string `json:"text"`
	} `json:"message"`
}

type tick struct {
	Seq     uint64 `json:"seq"`
	Session struct {
		State             string      `json:"state"`
		UptimeSec         *uint64     `json:"uptime_sec"`
		BankrollCents     *int64      `json:"bankroll_cents"`
		PnlCents          *int64      `json:"pnl_cents"`
		TableBetCents     *uint64     `json:"table_bet_cents"`
		LastWinCents      *uint64     `json:"last_win_cents"`
		ActiveCoverLabel  *string     `json:"active_cover_label"`
		ExposureCents     *uint64     `json:"exposure_cents"`
		RemainingWindowMs *uint32     `json:"remaining_window_ms"`
		BannerDetail      *string     `json:"banner_detail"`
		Settlement        *settlement `json:"last_settlement"`
		Divergence        *divergence `json:"last_divergence"`
		Caution           *string     `json:"caution"`
	} `json:"session"`
	Health struct {
		Ws         string  `json:"ws"`
		Mo         string  `json:"mo"`
		Exec       string  `json:"exec"`
		Db         string  `json:"db"`
		DeadSource *string `json:"dead_source"`
	} `json:"health"`
	Ladder struct {
		CurrentStep   *uint8  `json:"current_step"`
		MaxSteps      *uint8  `json:"max_steps"`
		BlowCostCents *uint64 `json:"blow_cost_cents"`
		Rows          []struct {
			Step               uint8  `json:"step"`
			StakePerCoverCents uint64 `json:"stake_per_cover_cents"`
			ExposureCents      uint64 `json:"exposure_cents"`
			CumLossCents       uint64 `json:"cum_loss_cents"`
			Active             bool   `json:"active"`
		} `json:"rows"`
	} `json:"ladder"`
	Decisions []struct {
		ID         int64    `json:"id"`
		Ts         int64    `json:"ts"`
		StrategyID string   `json:"strategy_id"`
		Confidence *float64 `json:"confidence"`
		Decision   string   `json:"decision"`
		Reason     *string  `json:"reason"`
	} `json:"decisions"`
}

func main() {
	cfgPath := flag.String("config", "config/telegram.json", "arquivo de credenciais do bot")
	coreFlag := flag.String("core", "http://127.0.0.1:8791", "base HTTP do monster-core")
	alertSec := flag.Int("alert-interval", 5, "intervalo de varredura de alertas em segundos")
	flag.Parse()

	cfg := loadConfig(*cfgPath)
	if env := os.Getenv("MONSTER_TG_TOKEN"); env != "" {
		cfg.Token = env
	}
	if env := os.Getenv("MONSTER_TG_CHAT"); env != "" {
		if id, err := strconv.ParseInt(env, 10, 64); err == nil {
			cfg.ChatID = id
		}
	}
	if cfg.Core == "" {
		cfg.Core = *coreFlag
	}
	if cfg.Token == "" {
		log.Fatalf("[telegram] token ausente: preencha %q (campo token) ou exporte MONSTER_TG_TOKEN", *cfgPath)
	}

	bot, err := botIdentity(cfg.Token)
	if err != nil {
		log.Fatalf("[telegram] token recusado pela Bot API: %v", err)
	}
	log.Printf("[telegram] conectado como @%s (id %d) · core=%s", bot.Username, bot.ID, cfg.Core)
	if cfg.ChatID == 0 {
		log.Printf("[telegram] chat_id não configurado: só /whoami responderá até %q ter o seu id", *cfgPath)
	} else {
		log.Printf("[telegram] chat autorizado: %d (qualquer outro é ignorado)", cfg.ChatID)
	}

	sender := newSender(cfg.Token, queueSize)
	go sender.run()

	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		pollUpdates(cfg, sender)
	}()
	go func() {
		defer wg.Done()
		watchAlerts(cfg, sender, time.Duration(*alertSec)*time.Second)
	}()
	wg.Wait()
}

func loadConfig(path string) config {
	raw, err := os.ReadFile(path)
	if err != nil {
		return config{}
	}
	var cfg config
	if err := json.Unmarshal(raw, &cfg); err != nil {
		log.Printf("[telegram] %s ilegível (%v) — seguindo com env", path, err)
		return config{}
	}
	return cfg
}

// ---- Bot API ----

type botInfo struct {
	ID       int64  `json:"id"`
	Username string `json:"username"`
}

func botIdentity(token string) (botInfo, error) {
	var out struct {
		OK     bool    `json:"ok"`
		Result botInfo `json:"result"`
	}
	if err := callJSON(http.MethodGet, apiURL(token, "getMe", nil), nil, &out, 15*time.Second); err != nil {
		return botInfo{}, err
	}
	if !out.OK {
		return botInfo{}, errors.New("resposta sem ok")
	}
	return out.Result, nil
}

func apiURL(token, method string, q url.Values) string {
	base := fmt.Sprintf("%s/bot%s/%s", apiBase, token, method)
	if len(q) == 0 {
		return base
	}
	return base + "?" + q.Encode()
}

func callJSON(method, rawURL string, body any, out any, timeout time.Duration) error {
	var reader io.Reader
	if body != nil {
		buf, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = bytes.NewReader(buf)
	}
	req, err := http.NewRequest(method, rawURL, reader)
	if err != nil {
		return err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	client := &http.Client{Timeout: timeout}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	payload, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode >= 400 {
		return fmt.Errorf("HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(payload)))
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(payload, out)
}

// sender serializa envio com retry exponencial 1s→30s e nunca bloqueia o poller.
type sender struct {
	token string
	queue chan string
}

func newSender(token string, size int) *sender {
	return &sender{token: token, queue: make(chan string, size)}
}

func (s *sender) send(chatID int64, text string) {
	msg := map[string]any{
		"chat_id":                  chatID,
		"text":                     text,
		"disable_web_page_preview": true,
	}
	raw, err := json.Marshal(msg)
	if err != nil {
		log.Printf("[telegram] mensagem não serializável: %v", err)
		return
	}
	select {
	case s.queue <- string(raw):
	default:
		log.Printf("[telegram] fila cheia — alerta descartado")
	}
}

func (s *sender) run() {
	for body := range s.queue {
		backoff := time.Second
		for attempt := 1; attempt <= 5; attempt++ {
			err := callJSON(http.MethodPost, apiURL(s.token, "sendMessage", nil), json.RawMessage(body), nil, 20*time.Second)
			if err == nil {
				break
			}
			log.Printf("[telegram] envio falhou (tentativa %d/5): %v", attempt, err)
			if attempt == 5 {
				break
			}
			time.Sleep(backoff)
			if backoff < 30*time.Second {
				backoff *= 2
			}
		}
	}
}

func pollUpdates(cfg config, send *sender) {
	offset := int64(0)
	// Descarta o backlog: um comando digitado enquanto o bot estava fora (um /kill
	// de ontem, por exemplo) não pode ser executado agora que ele voltou.
	if last, err := latestUpdateID(cfg.Token); err == nil && last > 0 {
		offset = last + 1
		log.Printf("[telegram] backlog descartado até update %d", last)
	}
	client := &http.Client{Timeout: (pollTimeout + 10) * time.Second}
	for {
		q := url.Values{}
		q.Set("timeout", strconv.Itoa(pollTimeout))
		if offset > 0 {
			q.Set("offset", strconv.FormatInt(offset, 10))
		}
		req, err := http.NewRequest(http.MethodGet, apiURL(cfg.Token, "getUpdates", q), nil)
		if err != nil {
			log.Printf("[telegram] update request inválido: %v", err)
			time.Sleep(5 * time.Second)
			continue
		}
		resp, err := client.Do(req)
		if err != nil {
			log.Printf("[telegram] getUpdates falhou: %v — retry em 5s", err)
			time.Sleep(5 * time.Second)
			continue
		}
		var out struct {
			OK     bool     `json:"ok"`
			Result []update `json:"result"`
		}
		err = json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&out)
		_ = resp.Body.Close()
		if err != nil {
			log.Printf("[telegram] resposta de updates ilegível: %v", err)
			time.Sleep(2 * time.Second)
			continue
		}
		for _, up := range out.Result {
			if up.UpdateID >= offset {
				offset = up.UpdateID + 1
			}
			if up.Message == nil {
				continue
			}
			handleMessage(cfg, send, up.Message.Chat.ID, up.Message.Text)
		}
	}
}

func handleMessage(cfg config, send *sender, chatID int64, text string) {
	fields := strings.Fields(strings.TrimSpace(text))
	if len(fields) == 0 {
		return
	}
	cmd := strings.ToLower(fields[0])
	if i := strings.Index(cmd, "@"); i == 0 {
		return
	} else if i > 0 {
		cmd = cmd[:i]
	}
	args := fields[1:]

	// Sem chat_id configurado o bot não aceita ordens: só mostra o id para o operador colar no arquivo.
	if cfg.ChatID == 0 {
		log.Printf("[telegram] mensagem de chat %d (não autorizado) — configure este id em config/telegram.json", chatID)
		if cmd == "/whoami" {
			send.send(chatID, fmt.Sprintf("Seu chat_id é %d.\nColoque em config/telegram.json: {\"chat_id\": %d}", chatID, chatID))
			return
		}
		send.send(chatID, fmt.Sprintf("Bot sem chat autorizado. Seu id: %d — configure config/telegram.json.", chatID))
		return
	}
	if chatID != cfg.ChatID {
		log.Printf("[telegram] comando ignorado de chat não autorizado %d", chatID)
		return
	}
	log.Printf("[telegram] comando %q do chat %d", cmd, chatID)

	switch cmd {
	case "/start", "/help":
		send.send(chatID, "MONSTER · comandos\n/status · sessão, banca, degrau, janela\n/banca · saldo e aposta na mesa\n/pnl · resultado e último ganho\n/ladder · escada ativa\n/padroes · estado e placar de cada matcher\n/ligar <id> · liga um matcher\n/desligar <id> · tira do jogo\n/faixa <id> <min> <max> · repetições de entrada\n/stop · pausa o executor (captura segue)\n/resume · retoma\n/kill · corta o executor")
	case "/whoami":
		send.send(chatID, fmt.Sprintf("chat_id autorizado: %d", chatID))
	case "/status":
		t, err := fetchTick(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		send.send(chatID, statusText(t))
	case "/banca":
		t, err := fetchTick(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		send.send(chatID, fmt.Sprintf("Banca %s · na mesa %s · exposição %s",
			cents(t.Session.BankrollCents), centsU(t.Session.TableBetCents), centsU(t.Session.ExposureCents)))
	case "/pnl":
		t, err := fetchTick(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		send.send(chatID, fmt.Sprintf("P&L da sessão %s · último ganho %s · estado %s",
			cents(t.Session.PnlCents), centsU(t.Session.LastWinCents), t.Session.State))
	case "/ladder":
		t, err := fetchTick(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		send.send(chatID, ladderText(t))
	case "/padroes":
		doc, err := fetchTuning(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		send.send(chatID, tuningText(doc))
	case "/ligar", "/desligar":
		if len(args) == 0 {
			send.send(chatID, "uso: "+cmd+" <id>\n/padroes lista os ids")
			return
		}
		doc, err := fetchTuning(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		id, ok := resolveMatcherID(args[0], doc.Matchers)
		if !ok {
			send.send(chatID, "matcher desconhecido: "+args[0]+" — /padroes")
			return
		}
		off := cmd == "/desligar"
		if !setDisabled(&doc.Current, id, off) {
			send.send(chatID, fmt.Sprintf("%s já estava %s", id, stateWord(off)))
			return
		}
		if err := applyTuning(cfg.Core, doc.Current); err != nil {
			send.send(chatID, "rejeitado: "+err.Error())
			return
		}
		send.send(chatID, fmt.Sprintf("%s agora %s", id, stateWord(off)))
	case "/faixa":
		if len(args) < 3 {
			send.send(chatID, "uso: /faixa <id> <min> <max>\nex.: /faixa coluna 3 5")
			return
		}
		min, errMin := strconv.Atoi(args[1])
		max, errMax := strconv.Atoi(args[2])
		if errMin != nil || errMax != nil {
			send.send(chatID, "min e max precisam ser números")
			return
		}
		doc, err := fetchTuning(cfg.Core)
		if err != nil {
			send.send(chatID, "core inacessível: "+err.Error())
			return
		}
		id, ok := resolveMatcherID(args[0], doc.Matchers)
		if !ok {
			send.send(chatID, "matcher desconhecido: "+args[0]+" — /padroes")
			return
		}
		if !setRange(&doc.Current, id, min, max) {
			send.send(chatID, id+" não usa faixa de repetição (parâmetro estatístico)")
			return
		}
		if err := applyTuning(cfg.Core, doc.Current); err != nil {
			send.send(chatID, "rejeitado: "+err.Error())
			return
		}
		send.send(chatID, fmt.Sprintf("%s entra em %d e aceita até %d repetições", id, min, max))
	case "/stop":
		if err := postCmd(cfg.Core, "pause"); err != nil {
			send.send(chatID, "falha ao pausar: "+err.Error())
			return
		}
		send.send(chatID, "Executor pausado. Captura continua viva.")
	case "/resume":
		if err := postCmd(cfg.Core, "resume"); err != nil {
			send.send(chatID, "falha ao retomar: "+err.Error())
			return
		}
		send.send(chatID, "Executor retomado.")
	case "/kill":
		if err := postCmd(cfg.Core, "kill"); err != nil {
			send.send(chatID, "falha no kill: "+err.Error())
			return
		}
		send.send(chatID, "KILL acionado. Executor cortado, captura mantida.")
	default:
		send.send(chatID, "comando desconhecido — /help")
	}
}

// ---- leitura do core ----

func fetchTick(core string) (tick, error) {
	var t tick
	err := callJSON(http.MethodGet, strings.TrimRight(core, "/")+"/api/tick", nil, &t, 5*time.Second)
	return t, err
}

func postCmd(core, cmd string) error {
	req, err := http.NewRequest(http.MethodPost, strings.TrimRight(core, "/")+"/api/"+cmd, nil)
	if err != nil {
		return err
	}
	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode >= 400 {
		return fmt.Errorf("HTTP %d", resp.StatusCode)
	}
	return nil
}

// settlementText monta a confirmação no formato de grupo: emoji, número, padrão,
// resultado e o placar acumulado desde que este bot subiu.
func settlementText(t tick, s settlement, wins, losses int) string {
	head := "🔴 OPERAÇÃO MAL SUCEDIDA ❌"
	if s.Won {
		head = "🟢 OPERAÇÃO BEM SUCEDIDA ✅"
	}
	sign := ""
	if s.DeltaCents >= 0 {
		sign = "+"
	}
	total := wins + losses
	rate := 0.0
	if total > 0 {
		rate = float64(wins) / float64(total) * 100
	}
	return fmt.Sprintf(
		"%s\n🎲 Número: %d\n🎯 %s · degrau %d/%d\n💰 Resultado: %s%s\n📊 Partidas: 🟢 %d | 🔴 %d · taxa %.1f%%\n🏦 Banca %s · P&L %s",
		head, s.Number, s.StrategyID, s.Step, s.MaxSteps,
		sign, centsI(s.DeltaCents),
		wins, losses, rate,
		cents(t.Session.BankrollCents), cents(t.Session.PnlCents))
}

func statusText(t tick) string {
	win := "—"
	if t.Session.RemainingWindowMs != nil {
		win = strconv.Itoa(int(*t.Session.RemainingWindowMs)) + "ms"
	}
	step := "—"
	if t.Ladder.CurrentStep != nil && t.Ladder.MaxSteps != nil {
		step = fmt.Sprintf("%d/%d", *t.Ladder.CurrentStep, *t.Ladder.MaxSteps)
	}
	up := "—"
	if t.Session.UptimeSec != nil {
		up = fmt.Sprintf("%02d:%02d", *t.Session.UptimeSec/60, *t.Session.UptimeSec%60)
	}
	return fmt.Sprintf("%s · sessão %s\nBanca %s · P&L %s\nDegrau %s · janela %s\nCobertura %s\nLEDs ws=%s mo=%s exec=%s db=%s",
		t.Session.State, up,
		cents(t.Session.BankrollCents), cents(t.Session.PnlCents),
		step, win,
		derefS(t.Session.ActiveCoverLabel),
		t.Health.Ws, t.Health.Mo, t.Health.Exec, t.Health.Db)
}

func ladderText(t tick) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Escada %s degraus · estouro %s\n", stepsLabel(t), centsU(t.Ladder.BlowCostCents))
	for _, r := range t.Ladder.Rows {
		mark := " "
		if r.Active {
			mark = "▸"
		}
		fmt.Fprintf(&b, "%s %d · ficha %s · exposição %s · perda acum. %s\n",
			mark, r.Step, centsI(int64(r.StakePerCoverCents)), centsI(int64(r.ExposureCents)), centsI(int64(r.CumLossCents)))
	}
	return strings.TrimRight(b.String(), "\n")
}

func stepsLabel(t tick) string {
	if t.Ladder.MaxSteps == nil {
		return "—"
	}
	return strconv.Itoa(int(*t.Ladder.MaxSteps))
}

// ---- alertas (zero ruído: só os cinco eventos do §11) ----

// settlement é o resultado que o core publica depois de reconciliar a entrada.
type settlement struct {
	StrategyID string `json:"strategy_id"`
	Won        bool   `json:"won"`
	DeltaCents int64  `json:"delta_cents"`
	Step       uint8  `json:"step"`
	Number     uint8  `json:"number"`
	MaxSteps   uint8  `json:"max_steps"`
	Ts         int64  `json:"ts"`
}

// divergence é o saldo real que não bateu com o esperado depois de um resultado (§9).
type divergence struct {
	StrategyID    string `json:"strategy_id"`
	Number        uint8  `json:"number"`
	ExpectedCents int64  `json:"expected_cents"`
	ObservedCents int64  `json:"observed_cents"`
	DiffCents     int64  `json:"diff_cents"`
	Ts            int64  `json:"ts"`
}

// divergenceText avisa a divergência de extrato com os três números que o operador confere.
func divergenceText(d divergence) string {
	return fmt.Sprintf(
		"⚠️ EXTRATO DIVERGENTE\n🎯 %s · saiu %d\n🧮 Esperado %s · Mesa %s\n➖ Diferença %s\nConfira o histórico da mesa (ficha manual, saque ou erro de clique).",
		d.StrategyID, d.Number, cents(&d.ExpectedCents), cents(&d.ObservedCents), centsI(d.DiffCents))
}

// cautionOf lê o motivo do modo cautela ("" = desligado).
func cautionOf(t tick) string {
	if t.Session.Caution == nil {
		return ""
	}
	return *t.Session.Caution
}

// cautionText avisa quando o modo cautela liga ou desliga ("" = nada a dizer).
func cautionText(prev, cur string) string {
	switch {
	case cur != "" && prev == "":
		return "⚠️ MODO CAUTELA LIGADO\n" + cur + "\nSó coluna e dúzia até 2 acertos de primeira ou 20 giros calmos."
	case cur == "" && prev != "":
		return "✅ MODO CAUTELA DESLIGADO\nTodas as estratégias liberadas."
	}
	return ""
}

type watcher struct {
	seen           map[int64]struct{}
	seeded         bool
	lastState      string
	lastDead       string
	lastSettlement int64
	lastDivergence int64
	lastCaution    string
	wins           int
	losses         int
}

func watchAlerts(cfg config, send *sender, every time.Duration) {
	w := &watcher{}
	for {
		t, err := fetchTick(cfg.Core)
		if err != nil {
			time.Sleep(every)
			continue
		}
		w.scan(cfg, send, t)
		time.Sleep(every)
	}
}

func (w *watcher) scan(cfg config, send *sender, t tick) {
	if !w.seeded {
		w.seen = make(map[int64]struct{}, len(t.Decisions))
		for _, d := range t.Decisions {
			w.seen[d.ID] = struct{}{}
		}
		w.seeded = true
		w.lastState = t.Session.State
		w.lastDead = derefS(t.Health.DeadSource)
		if t.Session.Settlement != nil {
			w.lastSettlement = t.Session.Settlement.Ts
		}
		if t.Session.Divergence != nil {
			w.lastDivergence = t.Session.Divergence.Ts
		}
		w.lastCaution = cautionOf(t)
		return
	}

	if cfg.ChatID != 0 {
		for _, d := range t.Decisions {
			if _, ok := w.seen[d.ID]; ok {
				continue
			}
			w.seen[d.ID] = struct{}{}
			switch strings.ToLower(d.Decision) {
			case "placed":
				send.send(cfg.ChatID, fmt.Sprintf(
					"🍀 ENTRADA CONFIRMADA 🍀\n🎯 %s · confiança %s\n🧾 %s\n🏦 Banca %s · P&L %s",
					d.StrategyID, conf(d.Confidence), derefS(d.Reason),
					cents(t.Session.BankrollCents), cents(t.Session.PnlCents)))
			case "aborted", "orphan":
				reason := strings.ToLower(derefS(d.Reason))
				if strings.Contains(reason, "ladderblown") || strings.Contains(reason, "estouro") ||
					strings.Contains(reason, "stoploss") || strings.Contains(reason, "stop_loss") {
					send.send(cfg.ChatID, fmt.Sprintf("ALERTA · %s interrompida\n%s", d.StrategyID, derefS(d.Reason)))
				}
			}
		}

		if t.Session.State != w.lastState {
			if t.Session.State == "KILLED" || t.Session.State == "DEGRADED" || t.Session.State == "ERRO" ||
				(t.Session.State == "PAUSADO" && derefS(t.Session.BannerDetail) != "—") {
				send.send(cfg.ChatID, fmt.Sprintf("ALERTA · estado %s\n%s", t.Session.State, derefS(t.Session.BannerDetail)))
			}
			w.lastState = t.Session.State
		}

		// Resultado confirmado: mesma linguagem dos grupos de operação, mas com o
		// delta do reconciliador — nada de deduzir vitória pelo saldo.
		if s := t.Session.Settlement; s != nil && s.Ts != w.lastSettlement {
			w.lastSettlement = s.Ts
			if s.Won {
				w.wins++
			} else {
				w.losses++
			}
			send.send(cfg.ChatID, settlementText(t, *s, w.wins, w.losses))
		}

		if d := t.Session.Divergence; d != nil && d.Ts != w.lastDivergence {
			w.lastDivergence = d.Ts
			send.send(cfg.ChatID, divergenceText(*d))
		}

		if c := cautionOf(t); c != w.lastCaution {
			if msg := cautionText(w.lastCaution, c); msg != "" {
				send.send(cfg.ChatID, msg)
			}
			w.lastCaution = c
		}

		dead := derefS(t.Health.DeadSource)
		if dead != w.lastDead && dead != "" && dead != "—" {
			send.send(cfg.ChatID, "ALERTA · fonte morta: "+dead)
			w.lastDead = dead
		}
	}
}

// ---- ajuste de padrões (espelha o POST /api/tuning da dashboard) ----

type tuningRange struct {
	Min int `json:"min"`
	Max int `json:"max"`
}

// tuningDoc é o mesmo contrato do config/tuning.json: faixas achatadas + ids desligados.
type tuningDoc struct {
	Coluna    tuningRange `json:"coluna"`
	Duzia     tuningRange `json:"duzia"`
	Cor       tuningRange `json:"cor"`
	AltoBaixo tuningRange `json:"alto_baixo"`
	ParImpar  tuningRange `json:"par_impar"`
	Disabled  []string    `json:"disabled"`
}

type tuningEnvelope struct {
	Current  tuningDoc `json:"current"`
	Defaults tuningDoc `json:"defaults"`
	Matchers []string  `json:"matchers"`
}

// rangeOfID mapeia os cinco matchers que têm faixa de repetição.
var rangeOfID = map[string]string{
	"quebra_permanencia_coluna": "coluna",
	"quebra_permanencia_duzia":  "duzia",
	"quebra_cor_6x":             "cor",
	"quebra_alto_baixo_6x":      "alto_baixo",
	"quebra_par_impar_6x":       "par_impar",
}

// aliasID aceita nome curto no celular: /desligar cluster, /faixa col 3 5.
var aliasID = map[string]string{
	"col":     "quebra_permanencia_coluna",
	"coluna":  "quebra_permanencia_coluna",
	"duzia":   "quebra_permanencia_duzia",
	"dúzia":   "quebra_permanencia_duzia",
	"cor":     "quebra_cor_6x",
	"alto":    "quebra_alto_baixo_6x",
	"baixo":   "quebra_alto_baixo_6x",
	"par":     "quebra_par_impar_6x",
	"impar":   "quebra_par_impar_6x",
	"setor":   "setor_vies",
	"fib":     "fibonacci_lag",
	"cluster": "cluster_terminal",
	"familia": "cluster_final",
}

func fetchTuning(core string) (tuningEnvelope, error) {
	var env tuningEnvelope
	err := callJSON(http.MethodGet, strings.TrimRight(core, "/")+"/api/tuning", nil, &env, 5*time.Second)
	return env, err
}

// applyTuning manda o documento inteiro (faixas + desligados) e devolve o erro do core.
func applyTuning(core string, doc tuningDoc) error {
	var out struct {
		OK    bool   `json:"ok"`
		Error string `json:"error"`
	}
	if err := callJSON(http.MethodPost, strings.TrimRight(core, "/")+"/api/tuning", doc, &out, 5*time.Second); err != nil {
		return err
	}
	if !out.OK && out.Error != "" {
		return errors.New(out.Error)
	}
	return nil
}

func resolveMatcherID(arg string, ids []string) (string, bool) {
	want := strings.ToLower(arg)
	for _, id := range ids {
		if strings.EqualFold(id, want) {
			return id, true
		}
	}
	if id, ok := aliasID[want]; ok {
		return id, true
	}
	// Prefixo único: /desligar cluster_ resolve cluster_terminal.
	var match string
	for _, id := range ids {
		if strings.HasPrefix(id, want) {
			if match != "" {
				return "", false
			}
			match = id
		}
	}
	return match, match != ""
}

func setDisabled(doc *tuningDoc, id string, off bool) bool {
	for i, d := range doc.Disabled {
		if d == id {
			if !off {
				doc.Disabled = append(doc.Disabled[:i], doc.Disabled[i+1:]...)
				return true
			}
			return false
		}
	}
	if !off {
		return false
	}
	doc.Disabled = append(doc.Disabled, id)
	return true
}

func setRange(doc *tuningDoc, id string, min, max int) bool {
	field, ok := rangeOfID[id]
	if !ok {
		return false
	}
	switch field {
	case "coluna":
		doc.Coluna = tuningRange{Min: min, Max: max}
	case "duzia":
		doc.Duzia = tuningRange{Min: min, Max: max}
	case "cor":
		doc.Cor = tuningRange{Min: min, Max: max}
	case "alto_baixo":
		doc.AltoBaixo = tuningRange{Min: min, Max: max}
	case "par_impar":
		doc.ParImpar = tuningRange{Min: min, Max: max}
	}
	return true
}

func stateWord(off bool) string {
	if off {
		return "DESLIGADO"
	}
	return "LIGADO"
}

// tuningText lista os matchers com estado e faixa, para decidir do celular.
func tuningText(env tuningEnvelope) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Leitura de padrões · %d/%d ligados\n", len(env.Matchers)-len(env.Current.Disabled), len(env.Matchers))
	for _, id := range env.Matchers {
		off := false
		for _, d := range env.Current.Disabled {
			if d == id {
				off = true
			}
		}
		state := "ON "
		if off {
			state = "OFF"
		}
		faixa := "sem faixa"
		if field, ok := rangeOfID[id]; ok {
			r := docRange(env.Current, field)
			faixa = fmt.Sprintf("%d..=%d", r.Min, r.Max)
		}
		fmt.Fprintf(&b, "%s %-30s %s\n", state, id, faixa)
	}
	b.WriteString("\n/ligar <id> · /desligar <id> · /faixa <id> <min> <max>")
	return b.String()
}

func docRange(doc tuningDoc, field string) tuningRange {
	switch field {
	case "coluna":
		return doc.Coluna
	case "duzia":
		return doc.Duzia
	case "cor":
		return doc.Cor
	case "alto_baixo":
		return doc.AltoBaixo
	case "par_impar":
		return doc.ParImpar
	}
	return tuningRange{}
}

// latestUpdateID lê o update mais recente sem consumi-lo em lote, para pular
// comandos antigos quando o bot volta.
func latestUpdateID(token string) (int64, error) {
	q := url.Values{}
	q.Set("offset", "-1")
	q.Set("timeout", "0")
	var out struct {
		OK     bool     `json:"ok"`
		Result []update `json:"result"`
	}
	if err := callJSON(http.MethodGet, apiURL(token, "getUpdates", q), nil, &out, 15*time.Second); err != nil {
		return 0, err
	}
	if len(out.Result) == 0 {
		return 0, nil
	}
	return out.Result[len(out.Result)-1].UpdateID, nil
}

// ---- formatação ----

func cents(c *int64) string {
	if c == nil {
		return "—"
	}
	return centsI(*c)
}

func centsU(c *uint64) string {
	if c == nil {
		return "—"
	}
	return centsI(int64(*c))
}

func centsI(v int64) string {
	sign := ""
	if v < 0 {
		sign = "-"
		v = -v
	}
	return fmt.Sprintf("%sR$ %d,%02d", sign, v/100, v%100)
}

func derefS(s *string) string {
	if s == nil || *s == "" {
		return "—"
	}
	return *s
}

func conf(c *float64) string {
	if c == nil {
		return "—"
	}
	return strconv.FormatFloat(*c, 'f', 2, 64)
}
