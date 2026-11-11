// SPDX-License-Identifier: Apache-2.0
// Observer injetado no target OOPIF da Playtech via Runtime.evaluate.
// Observa mutações na timeline, deduplica por (número, data-ts) e emite via __emit.
// Re-arma automaticamente caso o SPA recrie ou mova o container da timeline.
(() => {
  if (window.__mo) return;
  const seen = new Set();

  const emit = (n, node) => {
    const key = n + '|' + (node?.dataset?.ts || node?.id || '');
    if (seen.has(key)) return;
    seen.add(key);
    window.__emit(JSON.stringify({ t: 'spin', n, src: 'mo', at: Date.now() }));
  };

  const scan = (root) => {
    const sel = '.history .number, [class*="lastNumbers"] li, [class*="result"] .num';
    root.querySelectorAll?.(sel).forEach(el => {
      const n = parseInt(el.textContent, 10);
      if (n >= 0 && n <= 36) emit(n, el);
    });
  };

  const arm = () => {
    const host = document.querySelector('[class*="history"], [class*="lastNumbers"]');
    if (!host) return setTimeout(arm, 500);
    window.__mo = new MutationObserver(() => scan(host));
    window.__mo._host = host;
    window.__mo.observe(host, { childList: true, subtree: true, characterData: true });
    scan(host);
  };
  arm();

  // re-arma se o SPA trocar a timeline de lugar
  new MutationObserver(() => { if (!document.contains(window.__mo?._host)) arm(); })
    .observe(document.body, { childList: true, subtree: true });
})();
