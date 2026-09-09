const { getCurrentWindow } = window.__TAURI__.window;
const { listen, emit } = window.__TAURI__.event;

const appWindow = getCurrentWindow();

// State for Speed Graph and Thread Animator
let speedHistory = []; // { percent, speed } samples, capped
let copyActive = false;
let threadInterval = null;
let animationFrameId = null;
let graphRafId = null;
let lastGraphFrame = 0;
let lastDomTick = 0;
let currentPercent = 0;   // animated percent shown by the UI
let targetPercent = 0;    // real percent from engine
let smoothSpeed = 0;      // EMA-smoothed speed
let targetSpeed = 0;      // raw speed from engine
let smoothEta = -1;       // smoothed ETA in seconds
let targetEta = -1;
let isDelete = false;
let totalBytes = 0;
let totalFiles = 0;

const SPEED_SMOOTH_RATE = 0.10;   // per-frame fraction towards raw speed
const PERCENT_EASE_RATE = 4.5;    // per-second easing towards target percent
const ETA_SMOOTH_RATE = 0.18;
const MAX_SAMPLES = 900;          // ~15s of history at 60fps

// HTML escaping helper to prevent XSS
function escapeHtml(str) {
  if (typeof str !== 'string') return '';
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// Helper to get file basename
function getBasename(path) {
  if (!path) return '';
  const normalized = path.replace(/\\/g, '/');
  return normalized.split('/').pop() || path;
}

// Helper to format remaining time
function formatTime(seconds) {
  if (seconds < 0 || !isFinite(seconds)) return '--';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const secs = Math.round(seconds % 60);
  return `${minutes}m ${secs}s`;
}

// Window control bindings
const closeBtn = document.getElementById('close-btn');
if (closeBtn) {
  closeBtn.addEventListener('click', async () => {
    try {
      await emit('copy-cancel');
    } catch (e) {
      console.error(e);
    }
    try {
      await appWindow.hide();
    } catch (e) {
      console.error(e);
    }
  });
}

const cancelBtn = document.getElementById('cancel-btn');
if (cancelBtn) {
  cancelBtn.addEventListener('click', async () => {
    try {
      await emit('copy-cancel');
    } catch (e) {
      console.error(e);
    }
  });
}

const prepCancelBtn = document.getElementById('prep-cancel-btn');
if (prepCancelBtn) {
  prepCancelBtn.addEventListener('click', async () => {
    try {
      await emit('copy-cancel');
    } catch (e) {
      console.error(e);
    }
  });
}

document.getElementById('error-close-btn').addEventListener('click', async () => {
  document.getElementById('error-overlay').style.display = 'none';
  try {
    await appWindow.hide();
  } catch (e) {
    console.error(e);
  }
});

// Thread Visualization Animator (continuous, rAF-driven wave)
function startThreadAnimation(concurrency) {
  stopThreadAnimation();

  const threadGrid = document.getElementById('thread-grid');
  if (threadGrid) {
    threadGrid.innerHTML = '';
    for (let i = 0; i < concurrency; i++) {
      const dot = document.createElement('div');
      dot.className = 'thread-dot';
      threadGrid.appendChild(dot);
    }
  }

  copyActive = true;
  let startTime = Date.now();

  function animate() {
    if (!copyActive) return;
    const dots = document.querySelectorAll('.thread-dot');
    if (dots && dots.length > 0) {
      const elapsed = (Date.now() - startTime) / 1000;
      dots.forEach((dot, index) => {
        const wave = Math.sin(elapsed * 5 + index * 0.5);
        const opacity = 0.2 + (wave + 1) * 0.4;
        dot.style.opacity = opacity;
        if (opacity > 0.5) {
          dot.classList.add('active');
        } else {
          dot.classList.remove('active');
        }
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
  if (threadInterval) {
    clearInterval(threadInterval);
    threadInterval = null;
  }
  const dots = document.querySelectorAll('.thread-dot');
  dots.forEach((dot) => {
    dot.classList.remove('active');
    dot.style.opacity = '0.15';
  });
}

// Draws a buttery Catmull-Rom bezier path through the given pixel points.
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

// Draws a single graph frame with the current (animated) percent & speed.
function drawSpeedGraphFrame() {
  const canvas = document.getElementById('speed-canvas');
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  const percent = currentPercent;
  const speed = smoothSpeed;

  // Add a sample each frame; cap history to keep the redraw cheap.
  if (speedHistory.length === 0 || speedHistory[speedHistory.length - 1].percent < percent - 0.001) {
    speedHistory.push({ percent, speed: speed || 0 });
  }
  if (speedHistory.length > MAX_SAMPLES) {
    speedHistory.shift();
  }

  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Grid lines
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.03)';
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
    const y = h - (speedHistory[i].speed / maxSpeed) * (h - 4) - 2;
    points.push({ x, y });
  }

  // Gradient area under the curve
  const gradient = ctx.createLinearGradient(0, 0, 0, h);
  gradient.addColorStop(0, 'rgba(16, 185, 129, 0.20)');
  gradient.addColorStop(1, 'rgba(16, 185, 129, 0.0)');
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
  ctx.strokeStyle = 'rgba(52, 211, 153, 0.25)';
  ctx.lineWidth = 4;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  drawSmoothPath(ctx, points);
  ctx.stroke();

  // Main line
  ctx.strokeStyle = '#34d399';
  ctx.lineWidth = 1.6;
  drawSmoothPath(ctx, points);
  ctx.stroke();

  // Leading dot
  if (points.length > 0) {
    const last = points[points.length - 1];
    ctx.beginPath();
    ctx.arc(last.x, last.y, 2.4, 0, Math.PI * 2);
    ctx.fillStyle = '#a7f3d0';
    ctx.shadowColor = 'rgba(52, 211, 153, 0.9)';
    ctx.shadowBlur = 6;
    ctx.fill();
    ctx.shadowBlur = 0;
  }
}

// Kindly eases our display values toward the latest engine numbers.
function tickAnimation(timestamp) {
  if (graphRafId === null) return;
  if (lastGraphFrame === 0) lastGraphFrame = timestamp;
  const dt = Math.min(0.05, (timestamp - lastGraphFrame) / 1000);
  lastGraphFrame = timestamp;

  // Percent fill: time-based easing, snaps when effectively arrived.
  const before = currentPercent;
  currentPercent += (targetPercent - currentPercent) * Math.min(1, PERCENT_EASE_RATE * dt);
  if (Math.abs(targetPercent - currentPercent) < 0.02) currentPercent = targetPercent;

  // Speed + ETA: exponential smoothing.
  const speedFactor = Math.min(1, SPEED_SMOOTH_RATE * 20 * dt);
  smoothSpeed += (targetSpeed - smoothSpeed) * speedFactor;
  const etaFactor = Math.min(1, ETA_SMOOTH_RATE * 10 * dt);
  smoothEta += (targetEta - smoothEta) * etaFactor;

  // Progress bar fill + graph for every frame.
  const fill = document.getElementById('progress-bar-fill');
  if (fill) {
    fill.style.width = `${Math.round(currentPercent)}%`;
  }
  drawSpeedGraphFrame();

  // DOM text updates are throttled (~10 Hz) so the UI stays cheap & calm.
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

function refreshText() {
  const p = document.getElementById('progress-percent');
  if (p) p.innerText = `${Math.round(currentPercent)}%`;

  if (isDelete) {
    const doneFiles = Math.round((currentPercent / 100) * totalFiles);
    const pb = document.getElementById('progress-bytes');
    if (pb) pb.innerText = `${doneFiles.toLocaleString()} / ${totalFiles.toLocaleString()} files`;
    const sf = document.getElementById('stat-files');
    if (sf) sf.innerText = `${doneFiles.toLocaleString()} / ${totalFiles.toLocaleString()}`;
  } else {
    const bytesMb = (totalBytes / 1048576) * (currentPercent / 100);
    const totalMb = totalBytes / 1048576;
    const pb = document.getElementById('progress-bytes');
    if (pb) pb.innerText = `${bytesMb.toFixed(1)} MB / ${totalMb.toFixed(1)} MB`;
    const sf = document.getElementById('stat-files');
    if (sf) sf.innerText = `${Math.round((currentPercent / 100) * totalFiles).toLocaleString()} / ${totalFiles.toLocaleString()}`;
  }

  const ss = document.getElementById('stat-speed');
  if (ss) {
    if (isDelete) {
      ss.innerText = `${Math.round(smoothSpeed).toLocaleString()} files/s`;
    } else {
      ss.innerText = `${smoothSpeed.toFixed(1)} MB/s`;
    }
  }

  const se = document.getElementById('stat-eta');
  if (se) {
    if (smoothEta < 0) {
      se.innerText = 'Calculating...';
    } else if (smoothEta === 0) {
      se.innerText = 'Done';
    } else {
      se.innerText = formatTime(smoothEta);
    }
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

// Event listeners for copy progress updates
listen('copy-start', (event) => {
  const { sources, destination, description, concurrency, total_files, total_bytes } = event.payload;

  // Detect if this is a delete operation (no destination path)
  isDelete = !destination;
  totalFiles = total_files || 0;
  totalBytes = total_bytes || 0;

  // Show preparing loader, hide main dashboard
  document.getElementById('preparing-view').style.display = 'flex';
  document.getElementById('dashboard').style.display = 'none';

  const labelSpeed = document.getElementById('label-speed');
  const preparingText = document.getElementById('preparing-text');

  if (preparingText) {
    preparingText.innerText = "Indexing files to speed up operation";
  }
  const preparingCount = document.getElementById('preparing-count');
  if (preparingCount) {
    preparingCount.innerText = "0 files indexed";
  }

  if (isDelete) {
    if (labelSpeed) labelSpeed.innerText = "Delete Rate";
    document.getElementById('stat-speed').innerText = '0 files/s';
    document.getElementById('progress-bytes').innerText = `0 / ${totalFiles.toLocaleString()} files`;
    document.getElementById('stat-files').innerText = `0 / ${totalFiles.toLocaleString()}`;
  } else {
    if (labelSpeed) labelSpeed.innerText = "Speed";
    document.getElementById('stat-speed').innerText = '0.0 MB/s';
    const total_mb = (totalBytes / 1048576).toFixed(1);
    document.getElementById('progress-bytes').innerText = `0.0 MB / ${total_mb} MB`;
    document.getElementById('stat-files').innerText = `0 / ${totalFiles.toLocaleString()}`;
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
  const canvas = document.getElementById('speed-canvas');
  if (canvas) {
    const ctx = canvas.getContext('2d');
    ctx.clearRect(0, 0, canvas.width, canvas.height);
  }
  const fill = document.getElementById('progress-bar-fill');
  if (fill) fill.style.width = '0%';

  const profileDesc = document.getElementById('profile-desc');
  if (profileDesc) {
    profileDesc.innerText = description || (isDelete ? "Deleting files..." : "Copying files...");
  }
  const profileConcurrency = document.getElementById('profile-concurrency');
  if (profileConcurrency) {
    profileConcurrency.innerText = `${concurrency} threads`;
  }
  document.getElementById('stat-eta').innerText = 'Calculating...';
  document.getElementById('progress-percent').innerText = '0%';
  document.getElementById('status-msg').innerText = 'Initializing...';
  document.getElementById('cancel-btn').style.display = 'inline-block';

  // Update active transfer rows
  const jobsList = document.getElementById('jobs-list');
  jobsList.innerHTML = '';

  sources.forEach((src) => {
    const item = document.createElement('div');
    item.className = 'job-item';
    if (isDelete) {
      item.innerHTML = `
        <span style="font-weight: 500; color: #f87171; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 440px;" title="${escapeHtml(src)}">🗑️ Delete: ${escapeHtml(getBasename(src))}</span>
      `;
    } else {
      item.innerHTML = `
        <span style="font-weight: 500; color: #f3f4f6; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px;" title="${escapeHtml(src)}">${escapeHtml(getBasename(src))}</span>
        <span style="color: #6b7280; margin: 0 8px;">➔</span>
        <span style="color: #9ca3af; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px;" title="${escapeHtml(destination)}">${escapeHtml(getBasename(destination))}</span>
      `;
    }
    jobsList.appendChild(item);
  });

  startThreadAnimation(concurrency);
  document.getElementById('error-overlay').style.display = 'none';
});

listen('copy-progress', (event) => {
  const { files_completed, bytes_completed, speed_mbps, eta_seconds, total_files, total_bytes } = event.payload;

  // Hide preparing loader, show main dashboard
  document.getElementById('preparing-view').style.display = 'none';
  document.getElementById('dashboard').style.display = 'flex';

  totalFiles = total_files || 0;
  totalBytes = total_bytes || 0;

  const isCountBased = isDelete || totalBytes === 0;

  // Raw numbers from the engine become smoooth targets for the animation.
  targetPercent = totalBytes > 0
    ? Math.min(100, (bytes_completed / totalBytes) * 100)
    : (totalFiles > 0 ? Math.min(100, (files_completed / totalFiles) * 100) : 0);
  targetSpeed = speed_mbps || 0;
  targetEta = eta_seconds;

  document.getElementById('stat-files').innerText = `${files_completed.toLocaleString()} / ${totalFiles.toLocaleString()}`;

  if (targetEta < 0) {
    document.getElementById('stat-eta').innerText = 'Calculating...';
  } else if (targetEta === 0) {
    document.getElementById('stat-eta').innerText = 'Done';
  }

  const statusMsg = document.getElementById('status-msg');
  if (statusMsg) statusMsg.innerText = isCountBased ? (isDelete ? 'Deleting...' : 'Copying...') : 'Copying...';

  startGraphRendering();
});

listen('copy-complete', (event) => {
  const { files_copied, bytes_copied, failures, was_cancelled } = event.payload;

  // Ensure main dashboard/overlay is shown on completion
  document.getElementById('preparing-view').style.display = 'none';
  document.getElementById('dashboard').style.display = 'flex';

  stopThreadAnimation();

  // Let the fill glide the final few percent before freezing.
  targetPercent = 100;
  targetSpeed = 0;
  targetEta = 0;
  setTimeout(() => stopGraphRendering(), 700);

  if (failures && failures.length > 0) {
    const errorList = document.getElementById('error-list');
    errorList.innerHTML = '';
    failures.forEach(([path, err]) => {
      const p = document.createElement('div');
      p.style.marginBottom = '6px';
      p.innerHTML = `<span style="color: #ef4444; font-weight: 500;">${escapeHtml(getBasename(path))}</span>: ${escapeHtml(err)}`;
      errorList.appendChild(p);
    });
    document.getElementById('error-overlay').style.display = 'flex';
    document.getElementById('status-msg').innerText = `Failed (${failures.length})`;
  } else if (was_cancelled) {
    document.getElementById('status-msg').innerText = 'Cancelled';
    document.getElementById('cancel-btn').style.display = 'none';
    setTimeout(async () => {
      try {
        await appWindow.hide();
      } catch (e) {
        console.error(e);
      }
    }, 1500);
  } else {
    document.getElementById('status-msg').innerText = 'Done';
    document.getElementById('cancel-btn').style.display = 'none';
    document.getElementById('progress-percent').innerText = '100%';
    setTimeout(async () => {
      try {
        await appWindow.hide();
      } catch (e) {
        console.error(e);
      }
    }, 1500);
  }
});

listen('indexing-progress', (event) => {
  const count = event.payload;
  const preparingCount = document.getElementById('preparing-count');
  if (preparingCount) {
    preparingCount.innerText = `${count.toLocaleString()} files indexed`;
  }
});