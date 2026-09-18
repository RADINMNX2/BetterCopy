/* ============================================================
   BetterCopy - Tray menu window
   Icon-driven popup shown from the system tray. Reads backend
   state via get_tray_state, dispatches actions via tray_action.
   ============================================================ */
const { getCurrentWindow } = window.__TAURI__.window;
const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

const appWindow = getCurrentWindow();

const $ = (id) => document.getElementById(id);

const els = {
  startup: $('tray-startup'),
  admin: $('tray-admin'),
  closeToTray: $('tray-close-to-tray'),
};

async function refreshState() {
  try {
    const s = await invoke('get_tray_state');
    els.startup.classList.toggle('on', s.startup);
    els.admin.classList.toggle('on', s.start_as_admin);
    els.closeToTray.classList.toggle('on', s.close_to_tray);
    document.body.dataset.accent = s.accent || document.body.dataset.accent;
  } catch (e) { console.error(e); }
}

document.querySelectorAll('.tray-action').forEach((btn) => {
  btn.addEventListener('click', async () => {
    const action = btn.dataset.act;
    try {
      await invoke('tray_action', { action });
      if (action === 'open' || action === 'settings' || action === 'quit') {
        try { await appWindow.hide(); } catch (e) { console.error(e); }
      }
      await refreshState();
    } catch (e) { console.error(e); }
  });
});

listen('tray-state-changed', () => refreshState());

// Keep the menu from stealing clicks meant for the tray icon after it closes.
document.addEventListener('click', () => {});
listen('toggle-visibility', async () => {
  if (await appWindow.isVisible()) {
    await appWindow.hide();
  } else {
    await refreshState();
    await appWindow.show();
    await appWindow.setFocus();
  }
});

// Horizontal slide-in so the tiny frameless window opens with a hint of life.
requestAnimationFrame(() => document.body.classList.add('loaded'));