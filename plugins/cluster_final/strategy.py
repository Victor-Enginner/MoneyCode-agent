#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Plugin cluster_final: aposta a família de terminais dona da mesa (1-4-7 / 2-5-8 / 3-6-9) mais o final 0.
Lê NDJSON em stdin e escreve NDJSON em stdout conforme plugins/_proto; nunca clica, nunca abre rede.
Espelha o matcher nativo `ClusterFinal` de crates/strategy-native — se um mudar, o teste de paridade muda junto.
"""

from __future__ import annotations

import json
import sys
from typing import Iterable, Optional

# Partição das famílias: final % 3. O 0 é curinga e não pertence a nenhuma.
FAMILIES: tuple[tuple[int, ...], ...] = ((1, 4, 7), (2, 5, 8), (3, 6, 9))
WILDCARD = 0

DEFAULTS = {
    "window": 20,
    "min_streak": 4,
    "min_share": 0.55,
    "include_zero": True,
}


def family_of(number: int) -> Optional[tuple[int, ...]]:
    """Família do número, ou None para final 0 (curinga) e fora de 0..36."""
    if number < 0 or number > 36 or number % 10 == WILDCARD:
        return None
    final = number % 10
    for family in FAMILIES:
        if final in family:
            return family
    return None  # inalcançável: todo final 1..9 está numa família


def evaluate(spins: list[int], ladder_step: int = 1, **overrides) -> dict:
    """Avalia a janela (mais recente primeiro) e devolve o BetIntent no contrato §5.2."""
    cfg = {**DEFAULTS, **overrides}
    intent = {
        "v": 1,
        "strategy_id": "cluster_final",
        "cover": [],
        "confidence": 0.0,
    }

    if not spins or spins[0] > 36 or len(spins) < cfg["min_streak"]:
        return {**intent, "reason": "window_too_short"}

    family = next((f for f in (family_of(n) for n in spins) if f is not None), None)
    if family is None:
        return {**intent, "reason": "no_family"}

    # O 0 é curinga: continua a sequência da família dona.
    streak = 0
    for n in spins:
        if n > 36:
            break
        if n % 10 == WILDCARD or (n % 10) in family:
            streak += 1
        else:
            break
    if streak < cfg["min_streak"]:
        return {**intent, "reason": "streak_short", "streak": streak}

    window = spins[: cfg["window"]]
    non_zero = [n for n in window if n != 0 and n <= 36]
    if not non_zero:
        return {**intent, "reason": "no_non_zero"}
    share = sum(1 for n in non_zero if (n % 10) in family) / len(non_zero)
    if share < cfg["min_share"]:
        return {**intent, "reason": "family_share_low", "share": round(share, 4)}

    cover = []
    if cfg["include_zero"]:
        cover.append({"terminal": WILDCARD})
    cover.extend({"terminal": final} for final in family)

    confidence = min(0.95, 0.55 + 0.08 * (streak - cfg["min_streak"]))
    return {
        "v": 1,
        "strategy_id": "cluster_final",
        "cover": cover,
        "confidence": round(confidence, 4),
        "ladder_step": ladder_step,
        "family": list(family),
        "streak": streak,
        "share": round(share, 4),
    }


def handle(msg: dict) -> Optional[dict]:
    """Despacha uma mensagem do runtime; None significa 'sem resposta'."""
    kind = msg.get("t")
    if kind == "eval":
        spins = [int(n) for n in msg.get("spins", [])]
        return evaluate(spins, int(msg.get("ladder_step", 1)))
    if kind == "ping":
        return {"v": 1, "t": "pong"}
    return None


def serve(stream: Iterable[str] = sys.stdin) -> int:
    """Loop NDJSON: uma requisição por linha, uma resposta por linha, flush imediato."""
    for raw in stream:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            print(json.dumps({"v": 1, "error": "bad_json"}), flush=True)
            continue
        if msg.get("t") == "exit":
            return 0
        out = handle(msg)
        if out is not None:
            print(json.dumps(out, separators=(",", ":")), flush=True)
    return 0


def selftest() -> int:
    """Fixtures sem mesa: garante que o plugin e o matcher nativo concordam no essencial."""
    failures = 0

    def check(name: str, got, want) -> None:
        nonlocal failures
        if got != want:
            failures += 1
            print(f"FAIL {name}: got={got!r} want={want!r}")
        else:
            print(f"ok   {name}")

    window = [1, 4, 0, 7, 20, 13, 22, 1, 4, 7, 31, 16, 25, 7, 4, 1, 10, 28, 7, 4]
    hit = evaluate(window)
    check("trigger_cover", [c["terminal"] for c in hit["cover"]], [0, 1, 4, 7])
    check("trigger_confidence", hit["confidence"], 0.63)
    check("trigger_streak", hit["streak"], 5)

    check("alternating_family", evaluate([1, 2, 7, 8, 4, 5])["cover"], [])
    check("short_streak", evaluate([1, 4, 7, 32, 22, 13])["reason"], "streak_short")
    check(
        "share_low",
        evaluate([1, 4, 7, 11, 2, 5, 8, 20, 2, 5, 8, 29, 2, 5, 8, 20, 2, 5, 8, 32])["reason"],
        "family_share_low",
    )
    check("empty", evaluate([])["reason"], "window_too_short")
    check("no_zero_cover", [c["terminal"] for c in evaluate(window, include_zero=False)["cover"]], [1, 4, 7])
    check("family_236", evaluate([3, 6, 9, 13, 16])["cover"][0], {"terminal": 0})
    check("handle_ping", handle({"t": "ping"}), {"v": 1, "t": "pong"})
    check("handle_exit", handle({"t": "exit"}), None)

    print("\nPLUGIN_SELFTEST_OK" if failures == 0 else f"\nPLUGIN_SELFTEST_FAILED: {failures}")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        raise SystemExit(selftest())
    raise SystemExit(serve())
