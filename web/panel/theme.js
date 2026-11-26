'use strict';
// Set the palette before rendering. Only the appearance preference is persisted.
(() => {
  const key = 'hmesh.panel.appearance.v1';
  let theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  try {
    const saved = localStorage.getItem(key);
    if (saved === 'light' || saved === 'dark') theme = saved;
  } catch {}
  document.documentElement.dataset.theme = theme;
  document.addEventListener('DOMContentLoaded', () => {
    for (const option of document.querySelectorAll('input[name="panel-theme"]')) {
      option.checked = option.value === theme;
      option.addEventListener('change', () => {
        if (!option.checked) return;
        document.documentElement.dataset.theme = option.value;
        try { localStorage.setItem(key, option.value); } catch {}
      });
    }
  });
})();
