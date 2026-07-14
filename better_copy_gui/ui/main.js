const { getCurrentWindow } = window.__TAURI__.window;
const { listen, emit } = window.__TAURI__.event;

const appWindow = getCurrentWindow();

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
  await emit('copy-cancel');
  await appWindow.hide();
});

document.getElementById('cancel-btn').addEventListener('click', async () => {
  await emit('copy-cancel');
});

document.getElementById('error-close-btn').addEventListener('click', () => {
  document.getElementById('error-overlay').style.display = 'none';
});

// Event listeners for copy progress updates
listen('copy-start', (event) => {
  const { sources, destination, description, concurrency, total_files, total_bytes } = event.payload;
  
  document.getElementById('profile-desc').innerText = description || "Copying files...";
  document.getElementById('profile-concurrency').innerText = `${concurrency} worker threads active`;
  
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
});

listen('copy-complete', (event) => {
  const { files_copied, bytes_copied, failures, was_cancelled } = event.payload;
  
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
      await appWindow.hide();
    }, 1500);
  } else {
    document.getElementById('status-msg').innerText = 'Successfully completed!';
    document.getElementById('progress-percent').innerText = '100%';
    document.getElementById('progress-fill').style.width = '100%';
    setTimeout(async () => {
      await appWindow.hide();
    }, 1500);
  }
});
