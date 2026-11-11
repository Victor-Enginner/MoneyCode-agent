// SPDX-License-Identifier: Apache-2.0
// Observer e telemetria ao vivo injetados no iframe da Playtech (ptielive / playtech) via CDP.
// Extrai a fita ordenada de 12 números (.roulette-history_line), Saldo real (Balance), Aposta na mesa (Bet),
// Último Ganho (Last Win), Game Code real e status da janela (PLACE YOUR BETS vs WAIT FOR THE NEXT ROUND).
(() => {
  if (window.__monster_injected) return;
  window.__monster_injected = true;

  try {
    const OrigWS = window.WebSocket;
    if (OrigWS && !OrigWS.__monster_hooked) {
      const arrayBufToB64 = (buf) => {
        const bytes = new Uint8Array(buf);
        let bin = '';
        for (let i = 0; i < bytes.byteLength; i++) {
          bin += String.fromCharCode(bytes[i]);
        }
        return btoa(bin);
      };
      const PatchedWS = function (url, protocols) {
        const ws = protocols !== undefined ? new OrigWS(url, protocols) : new OrigWS(url);
        ws.addEventListener('message', (ev) => {
          try {
            if (typeof window.__emit !== 'function') return;
            if (ev.data instanceof ArrayBuffer) {
              window.__emit(JSON.stringify({ t: 'ws', b64: arrayBufToB64(ev.data), url: String(url || ''), at: Date.now() }));
            } else if (typeof Blob !== 'undefined' && ev.data instanceof Blob) {
              ev.data.arrayBuffer().then((buf) => {
                window.__emit(JSON.stringify({ t: 'ws', b64: arrayBufToB64(buf), url: String(url || ''), at: Date.now() }));
              }).catch(() => {});
            }
          } catch (_) {}
        });
        return ws;
      };
      PatchedWS.prototype = OrigWS.prototype;
      Object.assign(PatchedWS, OrigWS);
      PatchedWS.__monster_hooked = true;
      window.WebSocket = PatchedWS;
    }
  } catch (_) {}

  const parseMoneyCents = (raw) => {
    if (!raw) return null;
    const m = String(raw).match(/(\d[\d.,]*)/);
    if (!m) return null;
    let s = m[1];
    const lastDot = s.lastIndexOf('.');
    const lastComma = s.lastIndexOf(',');
    if (lastDot !== -1 && lastComma !== -1) {
      if (lastDot > lastComma) {
        s = s.replace(/,/g, '');
      } else {
        s = s.replace(/\./g, '').replace(',', '.');
      }
    } else if (lastComma !== -1) {
      s = s.replace(',', '.');
    } else if (lastDot !== -1) {
      const parts = s.split('.');
      if (parts.length > 2) {
        s = parts.slice(0, -1).join('') + '.' + parts[parts.length - 1];
      }
    }
    const val = parseFloat(s);
    if (Number.isNaN(val)) return null;
    return Math.round(val * 100);
  };

  const readTableTelemetry = () => {
    const bodyTxt = (document.body && document.body.innerText) || '';
    if (!bodyTxt) return null;

    let balanceCents = null;
    const balMatch = bodyTxt.match(/(?:Balance|Saldo)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (balMatch) {
      balanceCents = parseMoneyCents(balMatch[1]);
    }

    let betCents = 0;
    const betMatch = bodyTxt.match(/\b(?:Bet|Aposta|Total Bet)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (betMatch) {
      const parsed = parseMoneyCents(betMatch[1]);
      if (parsed !== null) betCents = parsed;
    }

    let lastWinCents = 0;
    const winMatch = bodyTxt.match(/(?:Last Win|[UÚ]ltimo Ganho)\s*[:\r\n\s]*R?\$\s*([\d.,]+)/i);
    if (winMatch) {
      const parsed = parseMoneyCents(winMatch[1]);
      if (parsed !== null) lastWinCents = parsed;
    }

    let gameCode = '';
    const gcMatch = bodyTxt.match(/(?:Game Code|C[oó]digo do Jogo)\s*[:\s]*(\d{5,16})/i);
    if (gcMatch) {
      gameCode = gcMatch[1];
    }

    let roundOpen = false;
    if (/PLACE YOUR BETS|FA[CÇ]A SUAS APOSTAS|FA[CÇ]AM SUAS APOSTAS/i.test(bodyTxt)) {
      roundOpen = true;
    } else if (/WAIT FOR THE NEXT ROUND|NO MORE BETS|AGUARDE A PR[OÓ]XIMA|N[AÃ]O H[AÁ] MAIS APOSTAS|SEM MAIS APOSTAS/i.test(bodyTxt)) {
      roundOpen = false;
    } else if (balanceCents !== null) {
      roundOpen = true;
    }

    return { balanceCents, betCents, lastWinCents, gameCode, roundOpen };
  };

  const HISTORY_LINE_SEL = [
    '.roulette-history_line',
    '[class*="roulette-history_line"]',
    '[class*="roulette-history-line"]'
  ].join(', ');

  // Extrai os números da barra horizontal de histórico da Playtech ordenados da esquerda (mais recente) para a direita.
  const extractStripNumbers = () => {
    const collectFromContainer = (root, requireBottomRight) => {
      if (!root || !root.querySelectorAll) return [];
      const all = root.querySelectorAll('div, span, li');
      const candidates = [];
      for (let i = 0; i < all.length; i++) {
        const el = all[i];
        if (el.children && el.children.length > 0) continue;
        const txt = (el.textContent || '').trim();
        if (!/^\d{1,2}$/.test(txt)) continue;
        const val = parseInt(txt, 10);
        if (val < 0 || val > 36) continue;
        const r = el.getBoundingClientRect();
        if (r.width < 4 || r.height < 4) continue;
        const cx = r.left + r.width / 2;
        const cy = r.top + r.height / 2;
        if (requireBottomRight) {
          if (cx < window.innerWidth * 0.66 || cy < window.innerHeight * 0.70 || cy > window.innerHeight * 0.90) {
            continue;
          }
        }
        candidates.push({ val, x: cx, y: cy });
      }
      if (candidates.length === 0) return [];

      // Agrupa na linha horizontal dominante (mesmo Y ± 18px)
      let bestRow = [];
      for (let i = 0; i < candidates.length; i++) {
        const refY = candidates[i].y;
        const row = candidates.filter(c => Math.abs(c.y - refY) <= 18);
        if (row.length > bestRow.length) bestRow = row;
      }
      bestRow.sort((a, b) => a.x - b.x);

      const deduped = [];
      for (let i = 0; i < bestRow.length && deduped.length < 12; i++) {
        if (deduped.length === 0 || Math.abs(bestRow[i].x - deduped[deduped.length - 1].x) >= 6) {
          deduped.push(bestRow[i]);
        }
      }
      return deduped.map(d => d.val);
    };

    const lineHost = document.querySelector(HISTORY_LINE_SEL);
    if (lineHost) {
      const nums = collectFromContainer(lineHost, false);
      if (nums.length >= 4) return nums;
    }
    const fallback = collectFromContainer(document.body, true);
    return fallback.length >= 6 ? fallback : [];
  };

  // Calcula quantos giros novos (1..=6) entraram à esquerda de `curr` em relação a `prev`.
  const countPrependedSpins = (prev, curr) => {
    if (!Array.isArray(prev) || !Array.isArray(curr) || prev.length < 4 || curr.length < 4) {
      return 0;
    }
    // Verifica primeiro se a fita é idêntica (k = 0)
    const cmp0 = Math.min(prev.length, curr.length);
    let same = true;
    for (let i = 0; i < cmp0; i++) {
      if (prev[i] !== curr[i]) {
        same = false;
        break;
      }
    }
    if (same) return 0;

    // Procura o menor deslocamento k (1..=6) tal que curr[k..] coincide com prev[0..]
    const maxShift = Math.min(6, curr.length - 3);
    for (let k = 1; k <= maxShift; k++) {
      const overlap = Math.min(prev.length, curr.length - k);
      if (overlap < 3) continue;
      let match = true;
      for (let i = 0; i < overlap; i++) {
        if (curr[i + k] !== prev[i]) {
          match = false;
          break;
        }
      }
      if (match) return k;
    }
    return 0;
  };

  let spinSeq = 0;
  let lastTelemetryJson = '';
  let lastTelemetryAt = 0;

  const inspectAndEmit = () => {
    if (typeof window.__emit !== 'function') return;
    const nums = extractStripNumbers();
    const tel = readTableTelemetry();

    // Só emite a partir do frame real da mesa Playtech (que possui a fita de números ou o saldo da mesa)
    if (nums.length === 0 && (!tel || tel.balanceCents === null)) {
      return;
    }

    const now = Date.now();
    if (nums.length >= 4) {
      if (!Array.isArray(window.__monster_last_nums) || window.__monster_last_nums.length === 0) {
        window.__monster_last_nums = nums.slice();
      } else {
        const added = countPrependedSpins(window.__monster_last_nums, nums);
        if (added > 0) {
          window.__monster_last_nums = nums.slice();
          // Emite do mais antigo dentre os novos (added - 1) até o mais recente (0)
          for (let idx = added - 1; idx >= 0; idx--) {
            spinSeq += 1;
            const num = nums[idx];
            const gc = (tel && tel.gameCode) ? tel.gameCode : String(now);
            const rid = `${gc}-${spinSeq}-${num}`;
            window.__emit(JSON.stringify({
              t: 'spin',
              n: num,
              rid,
              src: 'mo',
              at: now - idx * 55,
              nums,
              balance_cents: tel ? tel.balanceCents : null,
              bet_cents: tel ? tel.betCents : 0,
              last_win_cents: tel ? tel.lastWinCents : 0,
              game_code: tel ? tel.gameCode : '',
              round_open: tel ? tel.roundOpen : false
            }));
          }
        }
      }
    }

    const slipPayload = {
      t: 'slip',
      nums,
      bet_cents: tel ? tel.betCents : 0,
      balance_cents: tel ? tel.balanceCents : null,
      last_win_cents: tel ? tel.lastWinCents : 0,
      game_code: tel ? tel.gameCode : '',
      round_open: tel ? tel.roundOpen : false
    };
    const sig = JSON.stringify(slipPayload);
    if (sig !== lastTelemetryJson || (now - lastTelemetryAt) >= 1000) {
      lastTelemetryJson = sig;
      lastTelemetryAt = now;
      slipPayload.at = now;
      window.__emit(JSON.stringify(slipPayload));
    }
  };

  setInterval(inspectAndEmit, 350);

  const waitBody = () => {
    if (!document.body) return setTimeout(waitBody, 200);
    new MutationObserver(() => {
      inspectAndEmit();
    }).observe(document.body, { childList: true, subtree: true, characterData: true });
    inspectAndEmit();
  };
  waitBody();
})();
