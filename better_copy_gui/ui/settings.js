/* ============================================================
   BetterCopy - Settings window
   Reads/writes the same persistent prefs as the main window and
   syncs backend toggles (startup, close-to-tray, admin) via commands.
   ============================================================ */
const { getCurrentWindow } = window.__TAURI__.window;
const { emit } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

const appWindow = getCurrentWindow();

const PREF_KEYS = { accent: 'bc.accent', motion: 'bc.motion' };

const ACCENTS = ['emerald', 'cyan', 'violet', 'gold', 'rose'];

const $ = (id) => document.getElementById(id);

const els = {
  closeBtn: $('settings-close-btn'),
  swatches: $('accent-swatches'),
  startupToggle: $('startup-toggle'),
  closeToTrayToggle: $('close-to-tray-toggle'),
  adminToggle: $('admin-toggle'),
  motionToggle: $('motion-toggle'),
};

function readPref(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : JSON.parse(v);
  } catch (e) { return fallback; }
}

function writePref(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch (e) { /* noop */ }
}

// ----- Accent swatches -----
function renderSwatches() {
  const current = readPref(PREF_KEYS.accent, 'emerald');
  els.swatches.innerHTML = ACCENTS
    .map((a) => `<span class="swatch swatch-${a}${a === current ? ' active' : ''}" data-accent="${a}" title="${a}"></span>`)
    .join('');
}

els.swatches.addEventListener('click', (e) => {
  const sw = e.target.closest('.swatch');
  if (!sw || sw.classList.contains('active')) return;
  const accent = sw.dataset.accent;
  writePref(PREF_KEYS.accent, accent);
  document.body.dataset.accent = accent;
  els.swatches.querySelectorAll('.swatch').forEach((s) => {
    s.classList.toggle('active', s === sw);
  });
  emit('prefs-changed', { accent });
});

// ----- Motion toggle -----
els.motionToggle.addEventListener('change', () => {
  writePref(PREF_KEYS.motion, els.motionToggle.checked);
  emit('prefs-changed', { motion: els.motionToggle.checked });
});

// ----- Backend toggle (settings.js reads them, tray/menu actions flip them) -----
async function refreshToggles() {
  try {
    const state = await invoke('get_tray_state');
    els.startupToggle.checked = state.startup;
    els.closeToTrayToggle.checked = state.close_to_tray;
    els.adminToggle.checked = state.start_as_admin;
  } catch (e) { console.error(e); }
}
refreshToggles();

async function flip(field, checked) {
  try {
    await invoke('tray_action', { action: field, enabled: checked });
    await refreshToggles();
  } catch (e) { console.error(e); }
}

els.startupToggle.addEventListener('change', () => flip('startup', els.startupToggle.checked));
els.closeToTrayToggle.addEventListener('change', () => flip('close_to_tray', els.closeToTrayToggle.checked));
els.adminToggle.addEventListener('change', () => flip('admin', els.adminToggle.checked));

// After toggling admin, the app relaunches elevated — this window dies with it.
listen('tray-state-changed', () => refreshToggles());

// ----- Close -----
els.closeBtn.addEventListener('click', async () => {
  try { await appWindow.hide(); } catch (e) { console.error(e); }
});

// ----- Init -----
(function init() {
  renderSwatches();
  els.motionToggle.checked = !!readPref(PREF_KEYS.motion, false);
  requestAnimationFrame(() => document.body.classList.add('loaded'));
})();