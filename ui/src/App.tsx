// SPDX-License-Identifier: Apache-2.0
// Shell principal FUI (docs/FUI.md §4–§8): 12 colunas, 1440x900, zero scroll, 6 estados.
// Agrupa deltas de bus://tick a 4 Hz (250 ms) sem re-render por evento individual.

import React, { useEffect, useRef, useState } from 'react';
import {
  BusTickDelta,
  DecisionItemDto,
  ErrorItemDto,
  FillItemDto,
  fmtCents,
  fmtVal,
  GlobalState,
  HealthDto,
  LadderStateDto,
  OrphanItemDto,
  SessionDto,
  SpinItemDto,
  stateGlyph,
  TabKey,
} from './types';

// Ordem física dos 37 bolsos da roleta europeia / brasileira single-zero.
const EUROPEAN_WHEEL_ORDER: readonly number[] = [
  0, 32, 15, 19, 4, 21, 2, 25, 17, 34, 6, 27, 13, 36, 11, 30, 8, 23, 10, 5, 24,
  16, 33, 1, 20, 14, 31, 9, 22, 18, 29, 7, 28, 12, 35, 3, 26,
];

function pocketColumn(n: number): number {
  if (n === 0) return 0;
  return Math.floor((n - 1) / 12) + 1;
}

function formatClock(sec: number | null): string {
  if (sec === null || sec === undefined || sec < 0) return '—';
  const h = String(Math.floor(sec / 3600)).padStart(2, '0');
  const m = String(Math.floor((sec % 3600) / 60)).padStart(2, '0');
  const s = String(Math.floor(sec % 60)).padStart(2, '0');
  return `${h}:${m}:${s}`;
}

export const App: React.FC = () => {
  const [session, setSession] = useState<SessionDto>({
    state: 'OBSERVANDO',
    uptime_sec: null,
    bankroll_cents: null,
    pnl_cents: null,
    pnl_history_cents: [],
    active_cover_label: null,
    exposure_cents: null,
    remaining_window_ms: null,
    banner_detail: null,
  });

  const [health, setHealth] = useState<HealthDto>({
    ws: 'off',
    mo: 'off',
    exec: 'off',
    db: 'off',
    dead_source: null,
  });

  const [spins, setSpins] = useState<SpinItemDto[]>([]);
  const [ladder, setLadder] = useState<LadderStateDto>({
    current_step: null,
    max_steps: null,
    blow_cost_cents: null,
    rows: [],
  });
  const [decisions, setDecisions] = useState<DecisionItemDto[]>([]);
  const [fills, setFills] = useState<FillItemDto[]>([]);
  const [orphans, setOrphans] = useState<OrphanItemDto[]>([]);
  const [errors, setErrors] = useState<ErrorItemDto[]>([]);
  const [activeTab, setActiveTab] = useState<TabKey>('DECISOES');

  const pendingBatchRef = useRef<BusTickDelta | null>(null);
  const pnlCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const invokeTauri = async (cmd: string, args?: Record<string, unknown>) => {
    const w = window as unknown as {
      __TAURI__?: { core?: { invoke: (c: string, a?: unknown) => Promise<unknown> } };
    };
    if (w.__TAURI__?.core?.invoke) {
      return w.__TAURI__.core.invoke(cmd, args);
    }
    return null;
  };

  const triggerKill = () => {
    setSession((prev) => ({
      ...prev,
      state: 'KILLED',
      banner_detail: 'KILL SWITCH ACIONADO (Ctrl+Alt+K) — executor bloqueado, captura viva',
    }));
    void invokeTauri('kill');
  };

  // Lote a 4 Hz (250 ms) para aplicar deltas de bus://tick sem re-render por evento individual (§7).
  useEffect(() => {
    const timer = window.setInterval(() => {
      const batch = pendingBatchRef.current;
      if (!batch) return;
      pendingBatchRef.current = null;
      if (batch.session) setSession(batch.session);
      if (batch.health) setHealth(batch.health);
      if (batch.spins) setSpins(batch.spins.slice(0, 20));
      if (batch.ladder) setLadder(batch.ladder);
      if (batch.decisions) setDecisions(batch.decisions);
      if (batch.fills) setFills(batch.fills);
      if (batch.orphans) setOrphans(batch.orphans);
      if (batch.errors) setErrors(batch.errors.slice(0, 50));
    }, 250);
    return () => window.clearInterval(timer);
  }, []);

  // Atalhos globais de teclado (§8): Ctrl+Alt+K kill, P pause, R resume, 1..4 abas, Esc.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.altKey && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        triggerKill();
        return;
      }
      if (e.key === 'p' || e.key === 'P') {
        setSession((prev) => ({ ...prev, state: 'PAUSADO' }));
        void invokeTauri('pause');
      } else if (e.key === 'r' || e.key === 'R') {
        setSession((prev) => ({ ...prev, state: 'OBSERVANDO', banner_detail: null }));
        void invokeTauri('resume');
      } else if (e.key === '1') {
        setActiveTab('DECISOES');
      } else if (e.key === '2') {
        setActiveTab('FILLS');
      } else if (e.key === '3') {
        setActiveTab('ORFAOS');
      } else if (e.key === '4') {
        setActiveTab('ERROS');
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  // Desenho da curva P&L no canvas 152x52 (§5.D): ciano acima de 0, magenta abaixo, base cinza.
  useEffect(() => {
    const canvas = pnlCanvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);

    const midY = Math.floor(h / 2);
    ctx.strokeStyle = '#3a4459';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, midY);
    ctx.lineTo(w, midY);
    ctx.stroke();

    const pts = session.pnl_history_cents;
    if (!pts || pts.length < 2) return;

    const maxAbs = Math.max(100, ...pts.map((v) => Math.abs(v)));
    for (let i = 1; i < pts.length; i++) {
      const x0 = ((i - 1) / (pts.length - 1)) * (w - 4) + 2;
      const x1 = (i / (pts.length - 1)) * (w - 4) + 2;
      const y0 = midY - (pts[i - 1] / maxAbs) * (midY - 4);
      const y1 = midY - (pts[i] / maxAbs) * (midY - 4);
      ctx.strokeStyle = pts[i] >= 0 ? '#22d3ee' : '#f472b6';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
    }
  }, [session.pnl_history_cents]);

  const remMs = session.remaining_window_ms;
  const countdownColor =
    remMs === null
      ? 'var(--dim)'
      : remMs > 8000
      ? 'var(--accent)'
      : remMs >= 2000
      ? 'var(--warn)'
      : 'var(--bad)';
  const countdownPct = remMs === null ? 0 : Math.min(100, Math.max(0, (remMs / 15000) * 100));

  const exposurePctStr =
    session.exposure_cents !== null &&
    session.bankroll_cents !== null &&
    session.bankroll_cents > 0
      ? `${((session.exposure_cents / session.bankroll_cents) * 100).toFixed(1)}%`
      : '—';

  const showBanner =
    session.state === 'DEGRADED' || session.state === 'ERRO' || session.state === 'KILLED';

  return (
    <main className="fui-shell">
      {/* PAINEL A · BARRA DE ESTADO (36px fixo) */}
      <header className="panel-a" role="status" aria-label="Barra de estado da máquina">
        <div className={`state-badge state-${session.state}`}>
          <span aria-hidden="true">{stateGlyph(session.state)}</span>
          <span>{session.state}</span>
        </div>

        <div className="metric-cell">
          <span className="metric-label">SESSÃO</span>
          <span className="metric-val num">{formatClock(session.uptime_sec)}</span>
        </div>

        <div className="metric-cell">
          <span className="metric-label">BANCA</span>
          <span className="metric-val num">{fmtCents(session.bankroll_cents)}</span>
        </div>

        <div className="metric-cell">
          <span className="metric-label">P&amp;L</span>
          <span
            className="metric-val num"
            style={{
              color:
                session.pnl_cents === null
                  ? 'var(--fg)'
                  : session.pnl_cents >= 0
                  ? 'var(--accent)'
                  : 'var(--loss)',
            }}
          >
            {fmtCents(session.pnl_cents, true)}
          </span>
        </div>

        <div className="metric-cell">
          <span className="metric-label">DEGRAU</span>
          <span className="metric-val num">
            {ladder.current_step !== null && ladder.max_steps !== null
              ? `${ladder.current_step}/${ladder.max_steps}`
              : '—'}
          </span>
        </div>

        <div />

        <div className="led-group" aria-label="Indicadores de saúde por fonte">
          {(['ws', 'mo', 'exec', 'db'] as const).map((k) => (
            <span key={k} className="led-item num">
              <span className={`led-dot led-${health[k]}`} />
              {k.toUpperCase()}
            </span>
          ))}
        </div>

        <button
          type="button"
          className="btn-kill"
          aria-label="Kill Switch imediato (Ctrl+Alt+K)"
          onClick={triggerKill}
        >
          ■ KILL
        </button>
      </header>

      {/* Banner de DEGRADED / ERRO / KILLED (28px, nunca modal) */}
      {showBanner ? (
        <div
          className={`status-banner ${
            session.state === 'DEGRADED' ? 'banner-warn' : 'banner-bad'
          }`}
        >
          <span>
            {stateGlyph(session.state)}{' '}
            {fmtVal(
              session.banner_detail ??
                (health.dead_source ? `FONTE MORTA: ${health.dead_source}` : null)
            )}
          </span>
          <span className="num">LOG: logs/monster.log</span>
        </div>
      ) : (
        <div style={{ display: 'none' }} />
      )}

      {/* PAINEL B · TIMELINE (20 últimos números) */}
      <section className="panel-b" aria-label="Timeline dos últimos 20 números">
        <div className="panel-header">
          <span>TIMELINE (20)</span>
          <span className="num">FONTE: SPINS</span>
        </div>
        <div className="timeline-list">
          {Array.from({ length: 20 }).map((_, idx) => {
            const item = spins[idx];
            if (!item) {
              return (
                <div key={`empty-${idx}`} className="spin-pill num">
                  <span>—</span>
                  <span>—</span>
                  <span>—</span>
                </div>
              );
            }
            const classes = [
              'spin-pill',
              'num',
              `col-${item.column}`,
              item.triggered_matcher ? 'triggered' : '',
              idx === 0 ? 'latest' : '',
            ]
              .filter(Boolean)
              .join(' ');
            return (
              <div key={item.spin_id} className={classes}>
                <strong>{item.number}</strong>
                <span>{item.column === 0 ? 'ZERO' : `COL ${item.column}`}</span>
                <span>{item.source.toUpperCase()}</span>
              </div>
            );
          })}
        </div>
      </section>

      {/* PAINEL C · MESA E JANELA */}
      <section className="panel-c" aria-label="Roda de 37 bolsos e janela de apostas">
        <div className="panel-header">
          <span>MESA &amp; COBERTURA</span>
          <span className="num">FONTE: INTENT.ACTIVE</span>
        </div>

        <div className="wheel-stage">
          <svg width="240" height="240" viewBox="-120 -120 240 240" role="img" aria-label="Roda 37 fatias">
            {EUROPEAN_WHEEL_ORDER.map((pocket, idx) => {
              const angleStep = (2 * Math.PI) / 37;
              const a0 = idx * angleStep - Math.PI / 2;
              const a1 = (idx + 1) * angleStep - Math.PI / 2;
              const rOuter = 110;
              const rInner = 62;
              const x0 = Math.cos(a0) * rOuter;
              const y0 = Math.sin(a0) * rOuter;
              const x1 = Math.cos(a1) * rOuter;
              const y1 = Math.sin(a1) * rOuter;
              const xi0 = Math.cos(a1) * rInner;
              const yi0 = Math.sin(a1) * rInner;
              const xi1 = Math.cos(a0) * rInner;
              const yi1 = Math.sin(a0) * rInner;
              const col = pocketColumn(pocket);
              const isCovered =
                session.active_cover_label !== null &&
                col > 0 &&
                session.active_cover_label.includes(String(col));
              const fill =
                pocket === 0 ? '#22c55e' : isCovered ? '#22d3ee' : '#1d2433';
              const opacity = isCovered || pocket === 0 ? 1.0 : 0.55;
              const d = `M ${x0} ${y0} A ${rOuter} ${rOuter} 0 0 1 ${x1} ${y1} L ${xi0} ${yi0} A ${rInner} ${rInner} 0 0 0 ${xi1} ${yi1} Z`;
              return (
                <path
                  key={pocket}
                  d={d}
                  fill={fill}
                  fillOpacity={opacity}
                  stroke="#07080c"
                  strokeWidth="1"
                />
              );
            })}
            <circle cx="0" cy="0" r="56" fill="#07080c" stroke="#1d2433" />
            <text
              x="0"
              y="4"
              textAnchor="middle"
              fill="#cfd6e4"
              fontSize="14"
              fontWeight="600"
              className="num"
            >
              {spins[0] ? String(spins[0].number) : '—'}
            </text>
          </svg>

          <div className="cover-overlay">
            <div>
              <div className="metric-label">COBERTURA ATIVA</div>
              <div className="metric-val num" style={{ color: 'var(--accent)', fontSize: '14px' }}>
                {fmtVal(session.active_cover_label)}
              </div>
            </div>
            <div>
              <div className="metric-label">EXPOSIÇÃO TOTAL</div>
              <div className="metric-val num">{fmtCents(session.exposure_cents)}</div>
            </div>
            <div>
              <div className="metric-label">STATUS DE JANELA</div>
              <div
                className="metric-val num"
                style={{ color: remMs !== null && remMs < 2000 ? 'var(--bad)' : 'var(--fg)' }}
              >
                {remMs === null ? '—' : remMs < 2000 ? '✖ SEM ENTRADA (<2s)' : `${remMs} ms`}
              </div>
            </div>
          </div>
        </div>

        <div className="countdown-wrap">
          <div className="panel-header">
            <span>JANELA DE APOSTA</span>
            <span className="num">{remMs !== null ? `${(remMs / 1000).toFixed(1)}s` : '—'}</span>
          </div>
          <div className="countdown-track">
            <div
              className="countdown-bar"
              style={{ width: `${countdownPct}%`, backgroundColor: countdownColor }}
            />
          </div>
        </div>
      </section>

      {/* PAINEL D · P&L, ESCADA E EXPOSIÇÃO */}
      <section className="panel-d" aria-label="Curva P&L, tabela da escada e exposição">
        <div className="panel-header">
          <span>CURVA P&amp;L (R$)</span>
          <span className="num">152×52 · EIXO 0</span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <canvas
            ref={pnlCanvasRef}
            width={152}
            height={52}
            style={{ border: '1px solid var(--line)', background: 'var(--bg)' }}
          />
          <div style={{ textAlign: 'right' }}>
            <div className="metric-label">P&amp;L ACUMULADO</div>
            <div className="metric-val num" style={{ fontSize: '14px' }}>
              {fmtCents(session.pnl_cents, true)}
            </div>
          </div>
        </div>

        <div className="panel-header">
          <span>ESCADA ×3</span>
          <span className="num">FONTE: LADDER_STATE</span>
        </div>
        <table className="ladder-table num">
          <thead>
            <tr>
              <th>#</th>
              <th>FICHA/COL</th>
              <th>EXPOSIÇÃO</th>
              <th>PERDA ACUM.</th>
            </tr>
          </thead>
          <tbody>
            {ladder.rows.length === 0 ? (
              <tr>
                <td>—</td>
                <td>—</td>
                <td>—</td>
                <td>—</td>
              </tr>
            ) : (
              ladder.rows.map((r) => (
                <tr key={r.step} className={r.active ? 'ladder-row-active' : ''}>
                  <td>{r.active ? `▸ ${r.step}` : r.step}</td>
                  <td>{fmtCents(r.stake_per_cover_cents)}</td>
                  <td>{fmtCents(r.exposure_cents)}</td>
                  <td>{fmtCents(r.cum_loss_cents)}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>

        <div
          style={{
            marginTop: 'auto',
            display: 'flex',
            justifyContent: 'space-between',
            paddingTop: '6px',
            borderTop: '1px solid var(--line)',
          }}
        >
          <span className="num">ESTOURO: {fmtCents(ladder.blow_cost_cents)}</span>
          <span className="num">EXP/BANCA: {exposurePctStr}</span>
        </div>
      </section>

      {/* PAINEL E · ABAS: DECISÕES | FILLS | ÓRFÃOS | ERROS */}
      <section className="panel-e" aria-label="Registros operacionais em abas">
        <div className="tab-bar" role="tablist">
          {(['DECISOES', 'FILLS', 'ORFAOS', 'ERROS'] as const).map((tab, idx) => (
            <button
              key={tab}
              type="button"
              role="tab"
              aria-selected={activeTab === tab}
              className={`tab-btn num ${activeTab === tab ? 'active' : ''}`}
              onClick={() => setActiveTab(tab)}
            >
              [{idx + 1}] {tab}
            </button>
          ))}
        </div>

        <div aria-live="polite" style={{ overflowY: 'auto', flex: 1 }}>
          {activeTab === 'DECISOES' && (
            <table className="data-table num">
              <thead>
                <tr>
                  <th>SPIN_ID</th>
                  <th>CARTÃO</th>
                  <th>CONF</th>
                  <th>REGIME</th>
                  <th>CHI²</th>
                  <th>DECISÃO</th>
                  <th>MOTIVO</th>
                </tr>
              </thead>
              <tbody>
                {decisions.length === 0 ? (
                  <tr>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                  </tr>
                ) : (
                  decisions.map((d) => (
                    <tr key={d.id}>
                      <td>{fmtVal(d.spin_id)}</td>
                      <td>{fmtVal(d.strategy_id)}</td>
                      <td>{d.confidence !== null ? d.confidence.toFixed(2) : '—'}</td>
                      <td>{fmtVal(d.regime)}</td>
                      <td>{d.chi_square !== null ? d.chi_square.toFixed(2) : '—'}</td>
                      <td>{fmtVal(d.decision).toUpperCase()}</td>
                      <td>{fmtVal(d.reason)}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          )}

          {activeTab === 'FILLS' && (
            <table className="data-table num">
              <thead>
                <tr>
                  <th>INTENT_ID</th>
                  <th>COBERTURA</th>
                  <th>VALOR</th>
                  <th>ACKED</th>
                  <th>LATÊNCIA</th>
                </tr>
              </thead>
              <tbody>
                {fills.length === 0 ? (
                  <tr>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                  </tr>
                ) : (
                  fills.map((f) => (
                    <tr key={f.id}>
                      <td>{f.intent_id}</td>
                      <td>{fmtVal(f.cover)}</td>
                      <td>{fmtCents(f.stake_cents)}</td>
                      <td>{f.acked ? '● SIM' : '✖ NÃO'}</td>
                      <td>{f.latency_ms !== null ? `${f.latency_ms} ms` : '—'}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          )}

          {activeTab === 'ORFAOS' && (
            <table className="data-table num">
              <thead>
                <tr>
                  <th>INTENT_ID</th>
                  <th>SPIN_ID</th>
                  <th>ESTRATÉGIA</th>
                  <th>VALOR</th>
                  <th>RECONCILIAÇÃO</th>
                </tr>
              </thead>
              <tbody>
                {orphans.length === 0 ? (
                  <tr>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                  </tr>
                ) : (
                  orphans.map((o) => (
                    <tr key={o.intent_id}>
                      <td>{o.intent_id}</td>
                      <td>{fmtVal(o.spin_id)}</td>
                      <td>{fmtVal(o.strategy_id)}</td>
                      <td>{fmtCents(o.stake_cents)}</td>
                      <td>{fmtVal(o.reconcile_status)}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          )}

          {activeTab === 'ERROS' && (
            <table className="data-table num">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>KIND</th>
                  <th>DETALHE</th>
                  <th>TS</th>
                </tr>
              </thead>
              <tbody>
                {errors.length === 0 ? (
                  <tr>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                    <td>—</td>
                  </tr>
                ) : (
                  errors.map((err) => (
                    <tr key={err.id}>
                      <td>{err.id}</td>
                      <td>{fmtVal(err.kind)}</td>
                      <td>{fmtVal(err.detail)}</td>
                      <td>{fmtVal(err.ts)}</td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          )}
        </div>
      </section>
    </main>
  );
};

export default App;
