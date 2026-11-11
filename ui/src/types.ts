// SPDX-License-Identifier: Apache-2.0
// Contratos de dados estritos da FUI (docs/FUI.md §5, §6 e §7).
// Regra de ouro (§2.2): campo sem fonte retorna "—", nunca "0" inventado.

export type GlobalState =
  | 'OBSERVANDO'
  | 'ENTRADA'
  | 'PAUSADO'
  | 'DEGRADED'
  | 'ERRO'
  | 'KILLED';

export type LedStatus = 'ok' | 'warn' | 'bad' | 'off';

export type TabKey = 'DECISOES' | 'FILLS' | 'ORFAOS' | 'ERROS';

export interface SessionDto {
  state: GlobalState;
  uptime_sec: number | null;
  bankroll_cents: number | null;
  pnl_cents: number | null;
  pnl_history_cents: number[];
  active_cover_label: string | null;
  exposure_cents: number | null;
  remaining_window_ms: number | null;
  banner_detail: string | null;
}

export interface HealthDto {
  ws: LedStatus;
  mo: LedStatus;
  exec: LedStatus;
  db: LedStatus;
  dead_source: string | null;
}

export interface SpinItemDto {
  spin_id: string;
  number: number;
  column: number;
  ts: number;
  source: string;
  triggered_matcher: boolean;
}

export interface LadderRowDto {
  step: number;
  stake_per_cover_cents: number;
  exposure_cents: number;
  cum_loss_cents: number;
  active: boolean;
}

export interface LadderStateDto {
  current_step: number | null;
  max_steps: number | null;
  blow_cost_cents: number | null;
  rows: LadderRowDto[];
}

export interface DecisionItemDto {
  id: number;
  spin_id: string;
  ts: number;
  strategy_id: string;
  confidence: number | null;
  regime: string | null;
  chi_square: number | null;
  decision: 'placed' | 'won' | 'lost' | 'skipped' | 'aborted';
  reason: string | null;
}

export interface FillItemDto {
  id: number;
  intent_id: number;
  cover: string;
  stake_cents: number;
  acked: boolean;
  latency_ms: number | null;
}

export interface OrphanItemDto {
  intent_id: number;
  spin_id: string;
  strategy_id: string;
  stake_cents: number;
  reconcile_status: string;
}

export interface ErrorItemDto {
  id: number;
  kind: string;
  detail: string;
  ts: number;
}

export interface RiskCfgDto {
  base_cents: number | null;
  max_steps: number | null;
  max_bet_per_cover_cents: number | null;
  max_exposure_pct: number | null;
  stop_loss_cents: number | null;
}

export interface BusTickDelta {
  seq: number;
  session?: SessionDto;
  health?: HealthDto;
  spins?: SpinItemDto[];
  ladder?: LadderStateDto;
  decisions?: DecisionItemDto[];
  fills?: FillItemDto[];
  orphans?: OrphanItemDto[];
  errors?: ErrorItemDto[];
}

/** Formata centavos inteiros para R$ tabular ou "—" se ausente. */
export function fmtCents(cents: number | null | undefined, signed = false): string {
  if (cents === null || cents === undefined || Number.isNaN(cents)) {
    return '—';
  }
  const sign = cents < 0 ? '-' : signed && cents > 0 ? '+' : '';
  const abs = Math.abs(Math.trunc(cents));
  const reais = Math.floor(abs / 100);
  const centavos = String(abs % 100).padStart(2, '0');
  return `${sign}R$ ${reais},${centavos}`;
}

/** Formata fração percentual ou "—" se ausente. */
export function fmtPct(pct: number | null | undefined): string {
  if (pct === null || pct === undefined || Number.isNaN(pct)) {
    return '—';
  }
  return `${(pct * 100).toFixed(1)}%`;
}

/** Formata texto/inteiro ou "—" quando sem fonte. */
export function fmtVal(val: string | number | null | undefined): string {
  if (val === null || val === undefined || val === '') {
    return '—';
  }
  return String(val);
}

/** Glifo obrigatório por estado global (§3: cor nunca é o único sinal). */
export function stateGlyph(st: GlobalState): string {
  switch (st) {
    case 'OBSERVANDO':
      return '●';
    case 'ENTRADA':
      return '◈';
    case 'PAUSADO':
      return '⏸';
    case 'DEGRADED':
      return '⚠';
    case 'ERRO':
      return '✖';
    case 'KILLED':
      return '■';
  }
}
