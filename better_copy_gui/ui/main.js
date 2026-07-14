const { getCurrentWindow } = window.__TAURI__.window;
const { listen, emit } = window.__TAURI__.event;

const appWindow = getCurrentWindow();

// State for Speed Graph and Thread Animator
let speedHistory = [];
const maxHistory = 40;
let copyActive = false;
let threadInterval = null;

// Helper to get file basename
function getBasename(path) {
  if (!path) return '';
  const normalized = path.replace(/\\/g, '/');
  return normalized.split('/').pop() || path;
}

// Helper to format remaining time
function formatTime(seconds) {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const secs = Math.round(seconds % 60);
  return `${minutes}m ${secs}s`;
}

// Window control bindings
document.getElementById('close-btn').addEventListener('click', async () => {
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

document.getElementById('cancel-btn').addEventListener('click', async () => {
  try {
    await emit('copy-cancel');
  } catch (e) {
    console.error(e);
  }
});

document.getElementById('error-close-btn').addEventListener('click', () => {
  document.getElementById('error-overlay').style.display = 'none';
});

// Thread Visualization Animator
function startThreadAnimation(concurrency) {
  if (threadInterval) clearInterval(threadInterval);
  
  const threadGrid = document.getElementById('thread-grid');
  threadGrid.innerHTML = '';
  for (let i = 0; i < concurrency; i++) {
    const dot = document.createElement('div');
    dot.className = 'thread-dot';
    threadGrid.appendChild(dot);
  }

  copyActive = true;
  threadInterval = setInterval(() => {
    if (!copyActive) return;
    const dots = document.querySelectorAll('.thread-dot');
    dots.forEach((dot) => {
      // Simulate thread active state
      if (Math.random() > 0.35) {
        dot.classList.add('active');
        dot.style.opacity = Math.random() > 0.5 ? '1.0' : '0.7';
      } else {
        dot.classList.remove('active');
        dot.style.opacity = '0.2';
      }
    });
  }, 100);
}

function stopThreadAnimation() {
  copyActive = false;
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

// SVG/Canvas Speed Graph Drawer
function drawSpeedGraph(speed) {
  const canvas = document.getElementById('speed-canvas');
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  
  speedHistory.push(speed);
  if (speedHistory.length > maxHistory) {
    speedHistory.shift();
  }
  
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  
  // Draw grid lines
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.03)';
  ctx.lineWidth = 1;
  for (let i = 1; i < 3; i++) {
    const y = (canvas.height / 3) * i;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(canvas.width, y);
    ctx.stroke();
  }
  
  if (speedHistory.length < 2) return;
  
  const maxSpeed = Math.max(...speedHistory, 50.0); // Baseline scale of 50 MB/s
  
  // Draw gradient area under the line
  const gradient = ctx.createLinearGradient(0, 0, 0, canvas.height);
  gradient.addColorStop(0, 'rgba(16, 185, 129, 0.18)');
  gradient.addColorStop(1, 'rgba(16, 185, 129, 0.0)');
  
  ctx.beginPath();
  ctx.moveTo(0, canvas.height);
  
  for (let i = 0; i < speedHistory.length; i++) {
    const x = (canvas.width / (maxHistory - 1)) * i;
    const y = canvas.height - (speedHistory[i] / maxSpeed) * (canvas.height - 4) - 2;
    ctx.lineTo(x, y);
  }
  ctx.lineTo((canvas.width / (maxHistory - 1)) * (speedHistory.length - 1), canvas.height);
  ctx.closePath();
  ctx.fillStyle = gradient;
  ctx.fill();
  
  // Draw line
  ctx.beginPath();
  for (let i = 0; i < speedHistory.length; i++) {
    const x = (canvas.width / (maxHistory - 1)) * i;
    const y = canvas.height - (speedHistory[i] / maxSpeed) * (canvas.height - 4) - 2;
    if (i === 0) {
      ctx.moveTo(x, y);
    } else {
      ctx.lineTo(x, y);
    }
  }
  ctx.strokeStyle = '#34d399'; // Bright Emerald
  ctx.lineWidth = 1.5;
  ctx.stroke();
}

// Event listeners for copy progress updates
listen('copy-start', (event) => {
  const { sources, destination, description, concurrency, total_files, total_bytes } = event.payload;
  
  // Reset graph history and clear canvas
  speedHistory = [];
  const canvas = document.getElementById('speed-canvas');
  if (canvas) {
    const ctx = canvas.getContext('2d');
    ctx.clearRect(0, 0, canvas.width, canvas.height);
  }
  
  document.getElementById('profile-desc').innerText = description || "Copying files...";
  document.getElementById('profile-concurrency').innerText = `${concurrency} threads`;
  
  document.getElementById('stat-files').innerText = `0 / ${total_files}`;
  document.getElementById('stat-speed').innerText = '0.0 MB/s';
  document.getElementById('stat-eta').innerText = 'Calculating...';
  
  document.getElementById('progress-percent').innerText = '0%';
  document.getElementById('progress-fill').style.width = '0%';
  
  const total_mb = (total_bytes / 1048576).toFixed(1);
  document.getElementById('progress-bytes').innerText = `0.0 MB / ${total_mb} MB`;
  document.getElementById('status-msg').innerText = 'Initializing copy...';
  
  // Update active transfer rows
  const jobsList = document.getElementById('jobs-list');
  jobsList.innerHTML = '';
  
  sources.forEach((src) => {
    const item = document.createElement('div');
    item.className = 'job-item';
    item.innerHTML = `
      <span style="font-weight: 500; color: #f3f4f6; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px;" title="${src}">${getBasename(src)}</span>
      <span style="color: #6b7280; margin: 0 8px;">➔</span>
      <span style="color: #9ca3af; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 220px;" title="${destination}">${getBasename(destination)}</span>
    `;
    jobsList.appendChild(item);
  });
  
  startThreadAnimation(concurrency);
  document.getElementById('error-overlay').style.display = 'none';
});

listen('copy-progress', (event) => {
  const { files_completed, bytes_completed, speed_mbps, eta_seconds, total_files, total_bytes } = event.payload;
  
  document.getElementById('stat-files').innerText = `${files_completed} / ${total_files}`;
  document.getElementById('stat-speed').innerText = `${speed_mbps.toFixed(1)} MB/s`;
  
  if (eta_seconds < 0) {
    document.getElementById('stat-eta').innerText = 'Calculating...';
  } else if (eta_seconds === 0) {
    document.getElementById('stat-eta').innerText = 'Done';
  } else {
    document.getElementById('stat-eta').innerText = formatTime(eta_seconds);
  }
  
  const percent = total_bytes > 0 ? Math.round((bytes_completed / total_bytes) * 100) : 0;
  document.getElementById('progress-percent').innerText = `${percent}%`;
  document.getElementById('progress-fill').style.width = `${percent}%`;
  
  const bytes_mb = (bytes_completed / 1048576).toFixed(1);
  const total_mb = (total_bytes / 1048576).toFixed(1);
  document.getElementById('progress-bytes').innerText = `${bytes_mb} MB / ${total_mb} MB`;
  document.getElementById('status-msg').innerText = 'Transferring data...';
  
  drawSpeedGraph(speed_mbps);
});

listen('copy-complete', (event) => {
  const { files_copied, bytes_copied, failures, was_cancelled } = event.payload;
  
  stopThreadAnimation();
  
  if (failures && failures.length > 0) {
    const errorList = document.getElementById('error-list');
    errorList.innerHTML = '';
    failures.forEach(([path, err]) => {
      const p = document.createElement('div');
      p.style.marginBottom = '6px';
      p.innerHTML = `<span style="color: #ef4444; font-weight: 500;">${getBasename(path)}</span>: ${err}`;
      errorList.appendChild(p);
    });
    document.getElementById('error-overlay').style.display = 'flex';
    document.getElementById('status-msg').innerText = `Finished with ${failures.length} errors`;
  } else if (was_cancelled) {
    document.getElementById('status-msg').innerText = 'Cancelled by user.';
    setTimeout(async () => {
      try {
        await appWindow.hide();
      } catch (e) {
        console.error(e);
      }
    }, 1500);
  } else {
    document.getElementById('status-msg').innerText = 'Successfully completed!';
    document.getElementById('progress-percent').innerText = '100%';
    document.getElementById('progress-fill').style.width = '100%';
    setTimeout(async () => {
      try {
        await appWindow.hide();
      } catch (e) {
        console.error(e);
      }
    }, 1500);
  }
});
