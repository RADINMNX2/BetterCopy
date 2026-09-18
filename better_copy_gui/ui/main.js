/* ============================================================
   BetterCopy - NEON/GLASS front-end
   Smooth graph, pause/resume, theme accents, toasts + settings.
   ============================================================ */
const { getCurrentWindow } = window.__TAURI__.window;
const { listen, emit } = window.__TAURI__.event;

const appWindow = getCurrentWindow();

// ----- Persistent preferences -----
const PREF_KEYS = { accent: 'bc.accent', motion: 'bc.motion' };
const ACCENTS = ['emerald', 'cyan', 'violet', 'gold', 'rose'];

// ----- State for Speed Graph and Thread Animator -----
let speedHistory = [];   // { percent, speed } samples, capped
let copyActive = false;
let animationFrameId = null;
let graphRafId = null;
let lastGraphFrame = 0;
let lastDomTick = 0;
let currentPercent = 0;  // animated percent shown by the UI
let targetPercent = 0;   // real percent from engine
let smoothSpeed = 0;     // EMA-smoothed speed
let targetSpeed = 0;     // raw speed from engine
let smoothEta = -1;      // smoothed ETA in seconds
let targetEta = -1;
let isDelete = false;
let totalBytes = 0;
let totalFiles = 0;
let paused = false;      // UI pause state
let lastConcurrency = 0; // thread count for resume

const SPEED_SMOOTH_RATE = 0.10;
const PERCENT_EASE_RATE = 4.5;
const ETA_SMOOTH_RATE = 0.18;
const MAX_SAMPLES = 900;
const MAX_THREAD_DOTS = 6;

// Canvas is sized in device pixels; drawing happens in device px.

/* ---------- element lookup ---------- */
const $ = (id) => document.getElementById(id);

const els = {
  preparing: $('preparing-view'),
  dashboard: $('dashboard'),
  canvas: $('speed-canvas'),
  graphBox: document.querySelector('.graph-container'),
  fill: $('progress-bar-fill'),
  percent: $('progress-percent'),
  bytes: $('progress-bytes'),
  statSpeed: $('stat-speed'),
  statEta: $('stat-eta'),
  statFiles: $('stat-files'),
  labelSpeed: $('label-speed'),
  status: $('status-msg'),
  brandSub: $('brand-sub'),
  cancelBtn: $('cancel-btn'),
  prepCancelBtn: $('prep-cancel-btn'),
  closeBtn: $('close-btn'),
  pauseBtn: $('pause-btn'),
  threadGrid: $('thread-grid'),
  jobsList: $('jobs-list'),
  errorOverlay: $('error-overlay'),
  errorList: $('error-list'),
  errorCloseBtn: $('error-close-btn'),
  preparingText: $('preparing-text'),
  preparingCount: $('preparing-count'),
  settingsBtn: $('settings-btn'),
  settingsPop: $('settings-popover'),
  swatches: $('accent-swatches'),
  motionToggle: $('motion-toggle'),
  toastContainer: $('toast-container'),
};

/* ---------- HTML escaping / helpers ---------- */
function escapeHtml(str) {
  if (typeof str !== 'string') return '';
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function getBasename(path) {
  if (!path) return '';
  const normalized = path.replace(/\\/g, '/');
  return normalized.split('/').pop() || path;
}

function formatTime(seconds) {
  if (seconds < 0 || !isFinite(seconds)) return '--';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const secs = Math.round(seconds % 60);
  return `${minutes}m ${secs}s`;
}

/* ---------- Toasts ---------- */
let toastTimer = null;

function showToast(message, type = 'info') {
  if (!els.toastContainer) return;
  const old = els.toastContainer.querySelector('.toast');
  if (old) old.remove();

  const toast = document.createElement('div');
  toast.className = `toast ${type}`;
  toast.textContent = message;
  els.toastContainer.appendChild(toast);

  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toast.classList.add('out');
    setTimeout(() => toast.remove(), 320);
  }, 2600);
}

/* ---------- Theme / accent management ---------- */
function readPref(key, fallback) {
  try { return localStorage.getItem(key) || fallback; } catch (e) { return fallback; }
}
function writePref(key, value) {
  try { localStorage.setItem(key, value); } catch (e) { /* ignore */ }
}

function applyAccent(accent) {
  if (!ACCENTS.includes(accent)) accent = 'emerald';
  document.body.dataset.accent = accent;
  writePref(PREF_KEYS.accent, accent);
  els.swatches.querySelectorAll('.swatch').forEach((s) => {
    s.classList.toggle('active', s.dataset.accent === accent);
  });
  updateGraphTheme();
}

function toggleSettings(force) {
  const willShow = force !== undefined ? force : els.settingsPop.style.display === 'none';
  els.settingsPop.style.display = willShow ? 'block' : 'none';
}

/* ---------- Reduced motion ---------- */
function applyMotionPreference(reduce) {
  document.body.classList.toggle('reduce-motion', reduce);
  writePref(PREF_KEYS.motion, reduce ? '1' : '0');
  if (els.motionToggle) els.motionToggle.checked = reduce;
}

/* ---------- Canvas sizing (device-pixel aware) ---------- */
function resizeCanvas() {
  if (!els.canvas) return;
  const rect = els.canvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(32, Math.round(rect.width * dpr));
  const h = Math.max(8, Math.round(rect.height * dpr));
  if (els.canvas.width !== w || els.canvas.height !== h) {
    els.canvas.width = w;
    els.canvas.height = h;
  }
}

/* ---------- Live graph theme colors (read from CSS vars) ---------- */
let graphTheme = { rgb: '52, 245, 160' };
let graphThemeInited = false;

function updateGraphTheme() {
  const rgb = getComputedStyle(document.body).getPropertyValue('--accent-rgb').trim();
  graphTheme.rgb = rgb || '52, 245, 160';
  graphThemeInited = true;
}

/* ---------- Thread Visualization Animator ---------- */
function startThreadAnimation(concurrency) {
  stopThreadAnimation();

  const count = Math.min(Math.max(concurrency || 1, 1), MAX_THREAD_DOTS);
  els.threadGrid.innerHTML = '';
  for (let i = 0; i < count; i++) {
    const dot = document.createElement('div');
    dot.className = 'thread-dot';
    els.threadGrid.appendChild(dot);
  }

  copyActive = true;
  let startTime = Date.now();

  function animate() {
    if (!copyActive) return;
    const dots = els.threadGrid.querySelectorAll('.thread-dot');
    if (dots.length > 0) {
      const elapsed = (Date.now() - startTime) / 1000;
      dots.forEach((dot, index) => {
        const wave = Math.sin(elapsed * 5 + index * 0.5);
        dot.style.opacity = `${0.15 + (wave + 1) * 0.4}`;
      });
    }
    animationFrameId = requestAnimationFrame(animate);
  }
  animate();
}

function stopThreadAnimation() {
  copyActive = false;
  if (animationFrameId) {
    cancelAnimationFrame(animationFrameId);
    animationFrameId = null;
  }
  els.threadGrid.querySelectorAll('.thread-dot').forEach((dot) => {
    dot.style.opacity = '0.14';
  });
}

/* ---------- Speed graph rendering ---------- */
function drawSmoothPath(ctx, points) {
  const n = points.length;
  if (n === 0) return;
  ctx.beginPath();
  ctx.moveTo(points[0].x, points[0].y);
  if (n === 1) return;
  for (let i = 0; i < n - 1; i++) {
    const p0 = points[i - 1] || points[i];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2] || p2;
    const cp1x = p1.x + (p2.x - p0.x) / 6;
    const cp1y = p1.y + (p2.y - p0.y) / 6;
    const cp2x = p2.x - (p3.x - p1.x) / 6;
    const cp2y = p2.y - (p3.y - p1.y) / 6;
    ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, p2.x, p2.y);
  }
}

function drawSpeedGraphFrame() {
  const canvas = els.canvas;
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  const percent = currentPercent;
  const speed = smoothSpeed;
  const rgb = graphTheme.rgb;

  if (speedHistory.length === 0 || speedHistory[speedHistory.length - 1].percent < percent - 0.001) {
    speedHistory.push({ percent, speed: speed || 0 });
  }
  if (speedHistory.length > MAX_SAMPLES) speedHistory.shift();

  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Grid lines
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.035)';
  ctx.lineWidth = 1;
  for (let i = 1; i < 3; i++) {
    const y = (canvas.height / 3) * i;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(canvas.width, y);
    ctx.stroke();
  }

  let maxSpeed = 50;
  for (let i = 0; i < speedHistory.length; i++) {
    if (speedHistory[i].speed > maxSpeed) maxSpeed = speedHistory[i].speed;
  }

  const w = canvas.width;
  const h = canvas.height;
  const points = [];
  for (let i = 0; i < speedHistory.length; i++) {
    const x = w * (speedHistory[i].percent / 100);
    const y = h - (speedHistory[i].speed / maxSpeed) * (h - 6) - 2;
    points.push({ x, y });
  }

  // Gradient area under the curve
  const gradient = ctx.createLinearGradient(0, 0, 0, h);
  gradient.addColorStop(0, `rgba(${rgb}, 0.22)`);
  gradient.addColorStop(1, `rgba(${rgb}, 0.0)`);
  ctx.beginPath();
  ctx.moveTo(0, h);
  for (let i = 0; i < points.length; i++) {
    ctx.lineTo(points[i].x, points[i].y);
  }
  const lastX = w * (Math.max(percent, 0.0001) / 100);
  ctx.lineTo(lastX, h);
  ctx.closePath();
  ctx.fillStyle = gradient;
  ctx.fill();

  // Glow layer
  ctx.strokeStyle = `rgba(${rgb}, 0.28)`;
  ctx.lineWidth = 5;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  drawSmoothPath(ctx, points);
  ctx.stroke();

  // Main line
  ctx.strokeStyle = `rgb(${rgb})`;
  ctx.lineWidth = 1.8;
  drawSmoothPath(ctx, points);
  ctx.stroke();

  // Leading dot
  if (points.length > 0) {
    const last = points[points.length - 1];
    ctx.beginPath();
    ctx.arc(last.x, last.y, 2.6, 0, Math.PI * 2);
    ctx.fillStyle = '#ffffff';
    ctx.shadowColor = `rgba(${rgb}, 0.9)`;
    ctx.shadowBlur = 7;
    ctx.fill();
    ctx.shadowBlur = 0;
  }
}

/* ---------- Animation tick ---------- */
function tickAnimation(timestamp) {
  if (graphRafId === null) return;
  if (lastGraphFrame === 0) lastGraphFrame = timestamp;
  const dt = Math.min(0.05, (timestamp - lastGraphFrame) / 1000);
  lastGraphFrame = timestamp;

  const before = currentPercent;
  currentPercent += (targetPercent - currentPercent) * Math.min(1, PERCENT_EASE_RATE * dt);
  if (Math.abs(targetPercent - currentPercent) < 0.02) currentPercent = targetPercent;

  const speedFactor = Math.min(1, SPEED_SMOOTH_RATE * 20 * dt);
  smoothSpeed += (targetSpeed - smoothSpeed) * speedFactor;
  const etaFactor = Math.min(1, ETA_SMOOTH_RATE * 10 * dt);
  smoothEta += (targetEta - smoothEta) * etaFactor;

  if (els.fill) els.fill.style.width = `${Math.round(currentPercent)}%`;
  drawSpeedGraphFrame();

  const now = performance.now();
  if (now - lastDomTick >= 80 || Math.abs(currentPercent - before) < 0.001) {
    lastDomTick = now;
    refreshText();
  }

  if (targetPercent >= 100 && currentPercent >= 99.98) {
    graphRafId = null;
    return;
  }
  graphRafId = requestAnimationFrame(tickAnimation);
}

/* ---------- Pretty speed formatting ---------- */
function formatSpeed(mbps) {
  if (!isFinite(mbps) || mbps <= 0) return '0.0 MB/s';
  if (mbps >= 1024) return `${(mbps / 1024).toFixed(2)} GB/s`;
  return `${mbps.toFixed(1)} MB/s`;
}

function refreshText() {
  if (els.percent) els.percent.innerText = `${Math.round(currentPercent)}%`;

  if (isDelete) {
    const doneFiles = Math.round((currentPercent / 100) * totalFiles);
    if (els.bytes) els.bytes.innerText = `${doneFiles.toLocaleString()} / ${totalFiles.toLocaleString()} files`;
    if (els.statFiles) els.statFiles.innerText = `${doneFiles.toLocaleString()} / ${totalFiles.toLocaleString()}`;
  } else {
    const bytesMb = (totalBytes / 1048576) * (currentPercent / 100);
    const totalMb = totalBytes / 1048576;
    if (els.bytes) els.bytes.innerText = `${bytesMb.toFixed(1)} MB / ${totalMb.toFixed(1)} MB`;
    if (els.statFiles) els.statFiles.innerText = `${Math.round((currentPercent / 100) * totalFiles).toLocaleString()} / ${totalFiles.toLocaleString()}`;
  }

  if (els.statSpeed) {
    els.statSpeed.innerText = isDelete
      ? `${Math.round(smoothSpeed).toLocaleString()} files/s`
      : formatSpeed(smoothSpeed);
  }

  if (els.statEta) {
    if (smoothEta < 0) els.statEta.innerText = 'Calculating...';
    else if (smoothEta === 0) els.statEta.innerText = 'Done';
    else els.statEta.innerText = formatTime(smoothEta);
  }
}

function startGraphRendering() {
  if (graphRafId !== null) return;
  graphRafId = requestAnimationFrame(tickAnimation);
}

function stopGraphRendering() {
  if (graphRafId !== null) {
    cancelAnimationFrame(graphRafId);
    graphRafId = null;
  }
}

/* ---------- Status / header state helpers ---------- */
function setBrandStatus(text, cls = '') {
  els.brandSub.textContent = text;
  els.brandSub.classList.toggle('paused', cls === 'paused');
  els.brandSub.classList.toggle('done', cls === 'done');
}

function setGraphLabel(text) {
  if (els.graphBox) els.graphBox.dataset.label = text;
}

/* ---------- Pause / Resume ---------- */
async function emitCopyPause(pause) {
  try {
    await emit(pause ? 'copy-pause' : 'copy-resume');
  } catch (e) {
    console.error(e);
  }
}

function applyPausedUI(state) {
  paused = state;
  document.body.classList.toggle('paused', state);
  els.pauseBtn.title = state ? 'Resume' : 'Pause';
  els.pauseBtn.setAttribute('aria-label', state ? 'Resume transfer' : 'Pause transfer');
  els.status.classList.toggle('paused', state);
  if (state) {
    els.status.innerText = 'Paused';
    setBrandStatus('Paused', 'paused');
    stopThreadAnimation();
    stopGraphRendering();
    showToast('Transfer paused', 'paused');
  } else {
    setBrandStatus('Copying');
    if (isDelete) els.status.innerText = 'Deleting...';
    else els.status.innerText = 'Copying...';
    startThreadAnimation(lastConcurrency);
    showToast('Transfer resumed', 'success');
    startGraphRendering();
  }
}

async function togglePause() {
  if (els.pauseBtn.disabled) return;
  const next = !paused;
  applyPausedUI(next);
  await emitCopyPause(next);
}

/* ============================================================
   Window control bindings
   ============================================================ */
els.closeBtn.addEventListener('click', async () => {
  try { await emit('copy-cancel'); } catch (e) { console.error(e); }
  try { await appWindow.hide(); } catch (e) { console.error(e); }
});

els.cancelBtn.addEventListener('click', async () => {
  try { await emit('copy-cancel'); } catch (e) { console.error(e); }
});

els.prepCancelBtn.addEventListener('click', async () => {
  try { await emit('copy-cancel'); } catch (e) { console.error(e); }
});

els.pauseBtn.addEventListener('click', togglePause);

els.errorCloseBtn.addEventListener('click', async () => {
  els.errorOverlay.style.display = 'none';
  try { await appWindow.hide(); } catch (e) { console.error(e); }
});

/* Settings popover */
els.settingsBtn.addEventListener('click', (e) => {
  e.stopPropagation();
  toggleSettings();
});

document.addEventListener('click', (e) => {
  if (els.settingsPop.style.display !== 'none' &&
      !els.settingsPop.contains(e.target) &&
      e.target !== els.settingsBtn) {
    toggleSettings(false);
  }
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    toggleSettings(false);
  }
  // Ctrl+P toggles pause during an active transfer
  if (e.ctrlKey && (e.key === 'p' || e.key === 'P')) {
    e.preventDefault();
    if (!paused && graphRafId !== null) togglePause();
    else if (paused) togglePause();
  }
});

/* Accent swatches */
els.swatches.addEventListener('click', (e) => {
  const sw = e.target.closest('.swatch');
  if (!sw) return;
  applyAccent(sw.dataset.accent);
  showToast(`Accent: ${sw.dataset.accent}`, 'info');
});

/* Reduced motion */
els.motionToggle.addEventListener('change', () => {
  applyMotionPreference(els.motionToggle.checked);
});

/* ============================================================
   Tauri event listeners
   ============================================================ */
listen('copy-start', (event) => {
  const { sources, destination, description, concurrency, total_files, total_bytes } = event.payload;

  isDelete = !destination;
  totalFiles = total_files || 0;
  totalBytes = total_bytes || 0;
  lastConcurrency = concurrency || 4;

  // Reset pause state
  paused = false;
  document.body.classList.remove('paused');
  els.pauseBtn.disabled = false;
  els.pauseBtn.title = 'Pause';
  els.pauseBtn.setAttribute('aria-label', 'Pause transfer');

  els.preparing.style.display = 'flex';
  els.dashboard.style.display = 'none';

  if (els.preparingText) {
    els.preparingText.innerText = 'Indexing files to speed up operation';
  }
  if (els.preparingCount) {
    els.preparingCount.innerText = '0 files indexed';
  }

  if (isDelete) {
    if (els.labelSpeed) els.labelSpeed.innerText = 'Delete Rate';
    setGraphLabel('Delete Rate');
    els.statSpeed.innerText = '0 files/s';
    els.bytes.innerText = `0 / ${totalFiles.toLocaleString()} files`;
    els.statFiles.innerText = `0 / ${totalFiles.toLocaleString()}`;
  } else {
    if (els.labelSpeed) els.labelSpeed.innerText = 'Speed';
    setGraphLabel('Speed');
    els.statSpeed.innerText = '0.0 MB/s';
    const total_mb = (totalBytes / 1048576).toFixed(1);
    els.bytes.innerText = `0.0 MB / ${total_mb} MB`;
    els.statFiles.innerText = `0 / ${totalFiles.toLocaleString()}`;
  }

  // Reset animation state
  speedHistory = [];
  currentPercent = 0;
  targetPercent = 0;
  smoothSpeed = 0;
  targetSpeed = 0;
  smoothEta = -1;
  targetEta = -1;
  lastGraphFrame = 0;
  lastDomTick = 0;
  const ctx = els.canvas ? els.canvas.getContext('2d') : null;
  if (ctx) ctx.clearRect(0, 0, els.canvas.width, els.canvas.height);
  if (els.fill) els.fill.style.width = '0%';

  els.statEta.innerText = 'Calculating...';
  els.percent.innerText = '0%';
  els.status.innerText = 'Initializing...';
  els.status.classList.remove('paused', 'error');
  els.cancelBtn.style.display = 'inline-block';
  setBrandStatus(isDelete ? 'Preparing' : 'Preparing');

  // Update active transfer rows
  els.jobsList.innerHTML = '';
  sources.forEach((src) => {
    const item = document.createElement('div');
    item.className = 'job-item';
    if (isDelete) {
      item.innerHTML = `
        <span style="font-weight: 500; color: #ff8d97; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 290px;" title="${escapeHtml(src)}">Delete: ${escapeHtml(getBasename(src))}</span>
      `;
    } else {
      item.innerHTML = `
        <span style="font-weight: 500; color: var(--text-1); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 130px;" title="${escapeHtml(src)}">${escapeHtml(getBasename(src))}</span>
        <span style="color: var(--accent); margin: 0 6px;">></span>
        <span style="color: var(--text-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 130px;" title="${escapeHtml(destination)}">${escapeHtml(getBasename(destination))}</span>
      `;
    }
    els.jobsList.appendChild(item);
  });

  startThreadAnimation(concurrency);
  els.errorOverlay.style.display = 'none';
});

listen('copy-progress', (event) => {
  const { files_completed, bytes_completed, speed_mbps, eta_seconds, total_files, total_bytes } = event.payload;

  els.preparing.style.display = 'none';
  els.dashboard.style.display = 'flex';

  totalFiles = total_files || 0;
  totalBytes = total_bytes || 0;

  const isCountBased = isDelete || totalBytes === 0;

  targetPercent = totalBytes > 0
    ? Math.min(100, (bytes_completed / totalBytes) * 100)
    : (totalFiles > 0 ? Math.min(100, (files_completed / totalFiles) * 100) : 0);
  targetSpeed = speed_mbps || 0;
  targetEta = eta_seconds;

  els.statFiles.innerText = `${files_completed.toLocaleString()} / ${totalFiles.toLocaleString()}`;

  if (targetEta < 0) {
    els.statEta.innerText = 'Calculating...';
  } else if (targetEta === 0) {
    els.statEta.innerText = 'Done';
  }

  if (!paused) {
    els.status.innerText = isCountBased ? (isDelete ? 'Deleting...' : 'Copying...') : 'Copying...';
    setBrandStatus(isDelete ? 'Deleting' : 'Copying');
  }

  startGraphRendering();
});

listen('copy-complete', (event) => {
  const { files_copied, bytes_copied, failures, was_cancelled } = event.payload;

  els.preparing.style.display = 'none';
  els.dashboard.style.display = 'flex';

  // Finish pause state
  paused = false;
  document.body.classList.remove('paused');
  els.pauseBtn.disabled = true;
  els.pauseBtn.title = 'Pause';
  els.pauseBtn.setAttribute('aria-label', 'Pause transfer');
  els.status.classList.remove('paused');

  stopThreadAnimation();

  targetPercent = 100;
  targetSpeed = 0;
  targetEta = 0;
  setTimeout(() => stopGraphRendering(), 700);

  if (failures && failures.length > 0) {
    els.errorList.innerHTML = '';
    failures.forEach(([path, err]) => {
      const p = document.createElement('div');
      p.style.marginBottom = '6px';
      p.innerHTML = `<span style="color: var(--danger); font-weight: 600;">${escapeHtml(getBasename(path))}</span>: ${escapeHtml(err)}`;
      els.errorList.appendChild(p);
    });
    els.errorOverlay.style.display = 'flex';
    els.status.innerText = `Failed (${failures.length})`;
    els.status.classList.add('error');
    setBrandStatus('Errors');
  } else if (was_cancelled) {
    els.status.innerText = 'Cancelled';
    els.cancelBtn.style.display = 'none';
    setBrandStatus('Cancelled');
    setTimeout(async () => {
      try { await appWindow.hide(); } catch (e) { console.error(e); }
    }, 1500);
  } else {
    els.status.innerText = 'Done';
    els.cancelBtn.style.display = 'none';
    els.percent.innerText = '100%';
    setBrandStatus('Done', 'done');
    showToast(isDelete ? 'Deletion finished' : 'Copy finished', 'success');
    setTimeout(async () => {
      try { await appWindow.hide(); } catch (e) { console.error(e); }
    }, 1800);
  }
});

listen('indexing-progress', (event) => {
  const count = event.payload;
  if (els.preparingCount) {
    els.preparingCount.innerText = `${count.toLocaleString()} files indexed`;
  }
});

/* ============================================================
   Boot
   ============================================================ */
(function init() {
  // First frame show prep view (matches previous behaviour)
  els.preparing.style.display = 'flex';
  els.dashboard.style.display = 'none';

  // Restore preferences
  if (window.localStorage) {
    applyAccent(readPref(PREF_KEYS.accent, 'emerald'));
    applyMotionPreference(readPref(PREF_KEYS.motion, '0') === '1');
  } else {
    applyAccent('emerald');
    applyMotionPreference(false);
  }

  // DPI-aware canvas sizing
  resizeCanvas();
  if (typeof ResizeObserver !== 'undefined') {
    new ResizeObserver(resizeCanvas).observe(els.canvas);
  } else {
    window.addEventListener('resize', resizeCanvas);
  }

  setGraphLabel('Speed');

  // Fade the window in
  requestAnimationFrame(() => document.body.classList.add('loaded'));
})();