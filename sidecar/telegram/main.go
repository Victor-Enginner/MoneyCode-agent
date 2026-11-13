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
		State             string  `json:"state"`
		UptimeSec         *uint64 `json:"uptime_sec"`
		BankrollCents     *int64  `json:"bankroll_cents"`
		PnlCents          *int64  `json:"pnl_cents"`
		TableBetCents     *uint64 `json:"table_bet_cents"`
		LastWinCents      *uint64 `json:"last_win_cents"`
		ActiveCoverLabel  *string `json:"active_cover_label"`
		ExposureCents     *uint64 `json:"exposure_cents"`
		RemainingWindowMs *uint32 `json:"remaining_window_ms"`
		BannerDetail      *string `json:"banner_detail"`
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
	cmd := strings.ToLower(strings.TrimSpace(text))
	if i := strings.Index(cmd, "@"); i == 0 {
		return
	} else if i > 0 {
		cmd = cmd[:i]
	}

	// Sem chat_id configurado o bot não aceita ordens: só mostra o id para o operador colar no arquivo.
	if cfg.ChatID == 0 {
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

	switch cmd {
	case "/start", "/help":
		send.send(chatID, "MONSTER · comandos\n/status · sessão, banca, degrau, janela\n/banca · saldo e aposta na mesa\n/pnl · resultado e último ganho\n/ladder · escada ativa\n/stop · pausa o executor (captura segue)\n/resume · retoma\n/kill · corta o executor")
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

type watcher struct {
	seen      map[int64]struct{}
	seeded    bool
	lastState string
	lastDead  string
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
				send.send(cfg.ChatID, fmt.Sprintf("ENTRADA · %s · conf %s\n%s",
					d.StrategyID, conf(d.Confidence), derefS(d.Reason)))
			case "aborted", "orphan":
				reason := strings.ToLower(derefS(d.Reason))
				if strings.Contains(reason, "ladderblown") || strings.Contains(reason, "estouro") ||
					strings.Contains(reason, "stoploss") || strings.Contains(reason, "stop_loss") {
					send.send(cfg.ChatID, fmt.Sprintf("ALERTA · %s interrompida\n%s", d.StrategyID, derefS(d.Reason)))
				}
			}
		}

		if t.Session.State != w.lastState {
			if t.Session.State == "KILLED" || t.Session.State == "DEGRADED" || t.Session.State == "ERRO" {
				send.send(cfg.ChatID, fmt.Sprintf("ALERTA · estado %s\n%s", t.Session.State, derefS(t.Session.BannerDetail)))
			}
			w.lastState = t.Session.State
		}

		dead := derefS(t.Health.DeadSource)
		if dead != w.lastDead && dead != "" && dead != "—" {
			send.send(cfg.ChatID, "ALERTA · fonte morta: "+dead)
			w.lastDead = dead
		}
	}
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
