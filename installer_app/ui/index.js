/* ============================================================
   BetterCopy custom installer — neon-glass wizard front-end
   Steps: welcome -> destination -> installing -> done / error
   ============================================================ */
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const appWindow = getCurrentWindow();

const $ = (id) => document.getElementById(id);

const els = {
  closeBtn: $('btn-close'),
  backBtn: $('btn-back'),
  primaryBtn: $('btn-primary'),
  browseBtn: $('btn-browse'),
  dirInput: $('install-dir'),
  ringFill: $('ring-fill'),
  pct: $('pct'),
  statusMsg: $('status-msg'),
  doneSub: $('done-sub'),
  errorMsg: $('error-msg'),
};

const RING_C = 2 * Math.PI * 52;

let targetDir = '';
let installing = false;

function setStep(name) {
  document.querySelectorAll('.step').forEach((s) => {
    s.classList.toggle('is-active', s.dataset.step === name);
  });
  els.backBtn.hidden = name === 'welcome' || name === 'install' || name === 'done' || name === 'error';
}

function setPrimary(text, disabled) {
  els.primaryBtn.textContent = text;
  els.primaryBtn.disabled = !!disabled;
}

function setProgress(pct, msg) {
  const n = Math.max(0, Math.min(100, pct));
  els.ringFill.style.strokeDashoffset = String(RING_C * (1 - n / 100));
  els.pct.textContent = `${Math.round(n)}%`;
  if (msg) els.statusMsg.textContent = msg;
}

async function defaultDir() {
  try {
    targetDir = await invoke('get_target_dir');
  } catch (e) {
    targetDir = '';
    console.error(e);
  }
  els.dirInput.value = targetDir;
}

els.browseBtn.addEventListener('click', async () => {
  let picked;
  try {
    picked = await invoke('plugin:dialog|open', {
      options: { title: 'Choose install folder', directory: true, multiple: false },
    });
  } catch (e) {
    console.error(e);
  }
  if (picked && typeof picked === 'string') {
    targetDir = picked;
    els.dirInput.value = picked;
  }
});

els.closeBtn.addEventListener('click', async () => {
  if (installing) return;
  try { await appWindow.close(); } catch (e) { console.error(e); }
});

els.backBtn.addEventListener('click', () => setStep('welcome'));

els.primaryBtn.addEventListener('click', async () => {
  if (installing) return;
  const step = document.querySelector('.step.is-active').dataset.step;

  if (step === 'welcome') {
    targetDir = els.dirInput.value || targetDir;
    setStep('dest');
    setPrimary('Install', false);
    return;
  }

  if (step === 'dest') {
    if (!targetDir) return;
    setStep('install');
    setPrimary('Installing…', true);
    els.backBtn.hidden = true;
    setProgress(0, 'Preparing…');
    installing = true;
    try {
      await invoke('install_payload', { targetDir });
    } catch (e) {
      els.errorMsg.textContent = String(e);
      setStep('error');
      setPrimary('Close', false);
      installing = false;
    }
    return;
  }

  if (step === 'error') {
    try { await appWindow.close(); } catch (e) { /* noop */ }
    return;
  }

  if (step === 'done') {
    try { await appWindow.close(); } catch (e) { /* noop */ }
  }
});

listen('install-progress', (event) => {
  const { percent, message } = event.payload;
  setProgress(percent, message);
  if (percent >= 100) {
    installing = false;
    setProgress(100, 'Installation complete');
    setStep('done');
    setPrimary('Finish', false);
    try { appWindow.setFocus(); } catch (e) { /* noop */ }
  } else if (percent === 0 && message && !message.startsWith('Preparing')) {
    els.errorMsg.textContent = message;
    setStep('error');
    setPrimary('Close', false);
    installing = false;
  }
});

(async function init() {
  await defaultDir();
  setStep('welcome');
  setPrimary('Install', false);
})();