// SPDX-License-Identifier: Apache-2.0
// Observer injetado no target OOPIF da Playtech (ptielive / playtech) via CDP.
// Observa mutações na timeline (.roulette-history_line / lastHistoryItem), deduplica e emite via __emit.
// Re-arma automaticamente caso o SPA recrie ou mova o container da timeline.
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

  let seeded = false;
  let lastSig = '';
  let lastTopNode = null;
  let lastEmitTs = 0;
  const seen = new Set();

  const readRoundCounter = () => {
    const roundSel = [
      '[data-automation-locator*="round"]',
      '[data-automation-locator*="gameId"]',
      '[class*="round-id"]',
      '[class*="game-id"]',
      '[class*="roundCounter"]',
      '[class*="limits-and-info"] span'
    ].join(', ');
    const el = document.querySelector(roundSel);
    if (el) {
      const m = (el.textContent || '').trim().match(/\b(\d{2,12})\b/);
      if (m) return m[1];
    }
    return '';
  };

  const emit = (n, node) => {
    const now = Date.now();
    const rid = readRoundCounter();
    const explicitKey = rid || node?.dataset?.ts || node?.id || '';
    if (explicitKey) {
      const key = n + '|' + explicitKey;
      if (seen.has(key)) return;
      seen.add(key);
    } else {
      if ((now - lastEmitTs) < 4000) return;
      lastEmitTs = now;
    }
    if (typeof window.__emit === 'function') {
      window.__emit(JSON.stringify({ t: 'spin', n, rid, src: 'mo', at: now }));
    }
  };

  const ITEM_SEL = [
    '[data-automation-locator="field.lastHistoryItem"] [class*="history-item-value__text"]',
    '[data-automation-locator="field.lastHistoryItem"] div',
    '.roulette-history_line [class*="history-item-value__text"]',
    '[class*="history-item-value__text"]',
    '[class*="history-item-value_last"]',
    '.history .number',
    '[class*="lastNumbers"] li',
    '[class*="result"] .num'
  ].join(', ');

  const HOST_SEL = [
    '.roulette-history_line',
    '[class*="roulette-history_line"]',
    '[class*="roulette-history_extended__items"]',
    '[class*="roulette-history_extended"]',
    '[data-testid="roulette-history-extended"]',
    '[class*="history"]',
    '[class*="lastNumbers"]'
  ].join(', ');

  const extractNumbers = (root) => {
    if (!root || !root.querySelectorAll) return { first: null, nums: [] };
    const nodes = root.querySelectorAll(ITEM_SEL);
    if (!nodes || nodes.length === 0) return { first: null, nums: [] };
    const nums = [];
    let firstNode = null;
    for (let i = 0; i < nodes.length && nums.length < 12; i++) {
      const txt = (nodes[i].textContent || '').trim();
      if (/^\d+$/.test(txt)) {
        const val = parseInt(txt, 10);
        if (val >= 0 && val <= 36) {
          if (!firstNode) firstNode = nodes[i];
          nums.push(val);
        }
      }
    }
    return { first: firstNode, nums };
  };

  const scan = (root, isInitialSeed) => {
    const { first, nums } = extractNumbers(root);
    if (!first || nums.length === 0) return;
    const sig = nums.join(',');
    if (!seeded || isInitialSeed) {
      seeded = true;
      lastSig = sig;
      lastTopNode = first;
      return;
    }
    if (sig === lastSig && first === lastTopNode) return;
    lastSig = sig;
    lastTopNode = first;
    emit(nums[0], first);
  };

  const arm = () => {
    const host = document.querySelector(HOST_SEL);
    if (!host) return setTimeout(arm, 500);
    if (window.__mo && window.__mo._host === host) return;
    if (window.__mo) {
      try { window.__mo.disconnect(); } catch (_) {}
    }
    window.__mo = new MutationObserver(() => scan(host, false));
    window.__mo._host = host;
    window.__mo.observe(host, { childList: true, subtree: true, characterData: true });
    scan(host, true);
  };
  arm();

  const waitBody = () => {
    if (!document.body) return setTimeout(waitBody, 200);
    new MutationObserver(() => {
      if (!window.__mo?._host || !document.contains(window.__mo._host)) arm();
    }).observe(document.body, { childList: true, subtree: true });
  };
  waitBody();
})();
