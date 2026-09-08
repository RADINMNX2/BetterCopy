use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering, AtomicU64};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{Manager, Emitter, Listener};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;

#[derive(Clone, serde::Serialize)]
struct StartPayload {
    sources: Vec<String>,
    destination: String,
    description: String,
    concurrency: usize,
    total_files: usize,
    total_bytes: u64,
}

#[derive(Clone, serde::Serialize)]
struct ProgressPayload {
    files_completed: usize,
    bytes_completed: u64,
    speed_mbps: f64,
    eta_seconds: f64,
    total_files: usize,
    total_bytes: u64,
}

#[derive(Clone, serde::Serialize)]
struct CompletePayload {
    files_copied: usize,
    bytes_copied: u64,
    failures: Vec<(String, String)>,
    was_cancelled: bool,
}

enum Job {
    CopyMove {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        is_move: bool,
    },
    Delete {
        sources: Vec<PathBuf>,
    },
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.center();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }))
    .setup(|app| {
      let app_handle = app.handle().clone();
      let window = app.get_webview_window("main").unwrap();
      
      #[cfg(target_os = "windows")]
      {
          if window_vibrancy::apply_mica(&window, None).is_err() {
              let _ = window_vibrancy::apply_blur(&window, Some((15, 17, 23, 120)));
          }
      }
      
      // Setup tray icon menu
      let quit_item = MenuItemBuilder::new("Quit")
          .id("quit")
          .build(app)?;

      let tray_menu = MenuBuilder::new(app)
          .item(&quit_item)
          .build()?;

      let _tray = TrayIconBuilder::new()
          .icon(app.default_window_icon().unwrap().clone())
          .menu(&tray_menu)
          .on_menu_event(|app, event| {
              if event.id() == "quit" {
                  app.exit(0);
              }
          })
          .build(app)?;
      
      // Create channel for copy jobs
      let (copy_tx, copy_rx) = std::sync::mpsc::channel::<Job>();
      
      // Shared cancellation flag
      let cancel_flag = Arc::new(AtomicBool::new(false));
      let cancel_flag_clone = cancel_flag.clone();
      
      // Listen for cancel event from frontend UI
      let _id = app_handle.listen("copy-cancel", move |_event| {
          cancel_flag_clone.store(true, Ordering::SeqCst);
      });
      
      // Listen for window hide request from frontend UI
      let window_hide = window.clone();
      let _id_hide = app_handle.listen("window-hide-request", move |_event| {
          let _ = window_hide.hide();
      });
      
      let window_worker = window.clone();
      let cancel_flag_worker = cancel_flag.clone();
      
      // Spawn background worker thread to process copy queue sequential execution
      thread::spawn(move || {
          while let Ok(job) = copy_rx.recv() {
              cancel_flag_worker.store(false, Ordering::SeqCst);
              
               match job {
                  Job::CopyMove { sources, dest, is_move } => {
                      // Emit early copy-start to show the window instantly
                      let _ = window_worker.emit("copy-start", StartPayload {
                          sources: sources.iter().map(|p| p.display().to_string()).collect(),
                          destination: dest.display().to_string(),
                          description: "Analyzing files...".to_string(),
                          concurrency: 4, // placeholder
                          total_files: 0,
                          total_bytes: 0,
                      });
                      
                      let _ = window_worker.center();
                      let _ = window_worker.show();
                      let _ = window_worker.set_progress_bar(tauri::window::ProgressBarState {
                          status: Some(tauri::window::ProgressBarStatus::Normal),
                          progress: Some(0),
                      });

                      let last_emit = std::cell::Cell::new(Instant::now());
                      let window_clone = window_worker.clone();
                      let progress_cb = |count| {
                          let now = Instant::now();
                          let last = last_emit.get();
                          if now.duration_since(last) >= Duration::from_millis(100) {
                              let _ = window_clone.emit("indexing-progress", count);
                              last_emit.set(now);
                          }
                      };

                      // Walk tree to build work list
                      let work_list = match better_copy_core::walker::build_work_list(&sources, &dest, Some(&cancel_flag_worker), Some(&progress_cb)) {
                          Ok(wl) => wl,
                          Err(e) => {
                              let is_cancelled = e.kind() == std::io::ErrorKind::Interrupted || cancel_flag_worker.load(Ordering::SeqCst);
                              let _ = window_worker.emit("copy-complete", CompletePayload {
                                  files_copied: 0,
                                  bytes_copied: 0,
                                  failures: if is_cancelled { vec![] } else { vec![(dest.display().to_string(), format!("Failed to build work list: {}", e))] },
                                  was_cancelled: is_cancelled,
                              });
                              continue;
                          }
                      };
                      
                      let total_files = work_list.total_files;
                      let _ = window_worker.emit("indexing-progress", total_files);
                      let total_bytes = work_list.total_bytes;
                      
                      if cancel_flag_worker.load(Ordering::SeqCst) {
                          let _ = window_worker.emit("copy-complete", CompletePayload {
                              files_copied: 0,
                              bytes_copied: 0,
                              failures: vec![],
                              was_cancelled: true,
                          });
                          continue;
                      }

                      // Device profiling
                      let profile = better_copy_core::profiler::profile_device(&dest);
                      let concurrency = profile.concurrency;
                      
                      // Start progress window setup with actual values
                      let _ = window_worker.emit("copy-start", StartPayload {
                          sources: sources.iter().map(|p| p.display().to_string()).collect(),
                          destination: dest.display().to_string(),
                          description: profile.description.clone(),
                          concurrency,
                          total_files,
                          total_bytes,
                      });
                      
                      // Preflight checks
                      let mut preflight_failed = false;
                      let mut preflight_errors = Vec::new();
                      
                      // 2. Copy-into-self check
                      for src in &sources {
                          if dest.starts_with(src) {
                              preflight_failed = true;
                              preflight_errors.push((src.display().to_string(), "Cannot copy a directory inside itself.".to_string()));
                          }
                      }
                      
                      // 3. Free space check
                      let dest_full = match std::fs::canonicalize(&dest).or_else(|_| std::fs::create_dir_all(&dest).and_then(|_| std::fs::canonicalize(&dest))) {
                          Ok(path) => path,
                          Err(_) => dest.clone(),
                      };
                      if let Some(vp) = better_copy_core::profiler::get_volume_path(&dest_full) {
                          if let Some(free_bytes) = better_copy_core::profiler::get_disk_free_space(&vp) {
                              if free_bytes < total_bytes {
                                  preflight_failed = true;
                                  preflight_errors.push((dest.display().to_string(), format!("Insufficient disk space. Required: {:.2} GB, Available: {:.2} GB", total_bytes as f64 / 1_073_741_824.0, free_bytes as f64 / 1_073_741_824.0)));
                              }
                          }
                      }
                      
                      // 4. Writability probe
                      let time_num = std::time::SystemTime::now()
                          .duration_since(std::time::UNIX_EPOCH)
                          .unwrap_or_default()
                          .as_nanos();
                      let probe_file = dest.join(format!("better_copy_probe_{}.tmp", time_num));
                      if let Err(e) = std::fs::File::create(&probe_file).and_then(|_| std::fs::remove_file(&probe_file)) {
                          preflight_failed = true;
                          preflight_errors.push((dest.display().to_string(), format!("Destination is not writable: {}", e)));
                      }
                      
                      if preflight_failed {
                          let _ = window_worker.set_progress_bar(tauri::window::ProgressBarState {
                              status: Some(tauri::window::ProgressBarStatus::Error),
                              progress: Some(100),
                          });
                          let _ = window_worker.emit("copy-complete", CompletePayload {
                              files_copied: 0,
                              bytes_copied: 0,
                              failures: preflight_errors,
                              was_cancelled: false,
                          });
                          continue;
                      }
                      
                      if cancel_flag_worker.load(Ordering::SeqCst) {
                          let _ = window_worker.emit("copy-complete", CompletePayload {
                              files_copied: 0,
                              bytes_copied: 0,
                              failures: vec![],
                              was_cancelled: true,
                          });
                          continue;
                      }

                      // Run copy engine
                      let window_progress = window_worker.clone();
                      let start_time = Instant::now();
                      let last_update = Mutex::new(Instant::now());
                      let last_bytes = AtomicU64::new(0);
                      let last_files = std::sync::atomic::AtomicUsize::new(0);
                      
                      let progress_cb = Box::new(move |completed_files, completed_bytes| {
                          let now = Instant::now();
                          let mut last_up = last_update.lock().unwrap();
                          let elapsed_since_update = now.duration_since(*last_up);
                          
                          if elapsed_since_update >= Duration::from_millis(250) {
                              let total_elapsed = now.duration_since(start_time);
                              
                              let (speed, eta) = if total_bytes > 0 {
                                  let prev_bytes = last_bytes.load(Ordering::Relaxed);
                                  let delta_bytes = completed_bytes - prev_bytes;
                                  let speed = if elapsed_since_update.as_secs_f64() > 0.0 {
                                      (delta_bytes as f64 / 1_048_576.0) / elapsed_since_update.as_secs_f64()
                                  } else {
                                      0.0
                                  };
                                  let avg_speed = if total_elapsed.as_secs_f64() > 0.0 {
                                      completed_bytes as f64 / total_elapsed.as_secs_f64()
                                  } else {
                                      0.0
                                  };
                                  let remaining_bytes = if total_bytes > completed_bytes {
                                      total_bytes - completed_bytes
                                  } else {
                                      0
                                  };
                                  let eta = if avg_speed > 0.0 {
                                      remaining_bytes as f64 / avg_speed
                                  } else {
                                      -1.0
                                  };
                                  last_bytes.store(completed_bytes, Ordering::Relaxed);
                                  (speed, eta)
                              } else {
                                  let prev_files = last_files.load(Ordering::Relaxed);
                                  let delta_files = completed_files - prev_files;
                                  let speed = if elapsed_since_update.as_secs_f64() > 0.0 {
                                      delta_files as f64 / elapsed_since_update.as_secs_f64()
                                  } else {
                                      0.0
                                  };
                                  let avg_rate = if total_elapsed.as_secs_f64() > 0.0 {
                                      completed_files as f64 / total_elapsed.as_secs_f64()
                                  } else {
                                      0.0
                                  };
                                  let remaining_files = if total_files > completed_files {
                                      total_files - completed_files
                                  } else {
                                      0
                                  };
                                  let eta = if avg_rate > 0.0 {
                                      remaining_files as f64 / avg_rate
                                  } else {
                                      -1.0
                                  };
                                  last_files.store(completed_files, Ordering::Relaxed);
                                  (speed, eta)
                              };
                              
                              *last_up = now;
                              
                              let percent = if total_bytes > 0 {
                                  ((completed_bytes as f64 / total_bytes as f64) * 100.0) as u64
                              } else if total_files > 0 {
                                  ((completed_files as f64 / total_files as f64) * 100.0) as u64
                              } else {
                                  0
                              };
                              
                              let _ = window_progress.set_progress_bar(tauri::window::ProgressBarState {
                                  status: Some(tauri::window::ProgressBarStatus::Normal),
                                  progress: Some(percent),
                              });
                              
                              let _ = window_progress.emit("copy-progress", ProgressPayload {
                                  files_completed: completed_files,
                                  bytes_completed: completed_bytes,
                                  speed_mbps: speed,
                                  eta_seconds: eta,
                                  total_files,
                                  total_bytes,
                              });
                          }
                      });
                      
                      let summary = better_copy_core::engine::run_engine_with_work_list(
                          work_list,
                          &sources,
                          &dest,
                          is_move,
                          None,
                          cancel_flag_worker.clone(),
                          Arc::new(AtomicBool::new(false)), // pause not wired to Tauri UI yet
                          false,                            // verified-move hash check off for Tauri
                          Some(progress_cb),
                      );
                      
                      let has_failures = !summary.failures.is_empty();
                      let was_cancelled = summary.was_cancelled;
                      
                      let final_status = if was_cancelled {
                          tauri::window::ProgressBarStatus::None
                      } else if has_failures {
                          tauri::window::ProgressBarStatus::Error
                      } else {
                          tauri::window::ProgressBarStatus::None
                      };
                      
                      let _ = window_worker.set_progress_bar(tauri::window::ProgressBarState {
                          status: Some(final_status),
                          progress: Some(100),
                      });
                      
                      let _ = window_worker.emit("copy-complete", CompletePayload {
                          files_copied: summary.files_copied,
                          bytes_copied: summary.bytes_copied,
                          failures: summary.failures.iter().map(|(p, e)| (p.display().to_string(), e.clone())).collect(),
                          was_cancelled,
                      });
                  }
                  Job::Delete { sources } => {
                      if sources.is_empty() {
                          continue;
                      }
                      
                      // Emit early copy-start to show the window instantly
                      let _ = window_worker.emit("copy-start", StartPayload {
                          sources: sources.iter().map(|p| p.display().to_string()).collect(),
                          destination: String::new(),
                          description: "Analyzing files...".to_string(),
                          concurrency: 4, // placeholder
                          total_files: 0,
                          total_bytes: 0,
                      });
                      
                      let _ = window_worker.center();
                      let _ = window_worker.show();
                      let _ = window_worker.set_progress_bar(tauri::window::ProgressBarState {
                          status: Some(tauri::window::ProgressBarStatus::Normal),
                          progress: Some(0),
                      });

                      let first_source = &sources[0];
                      
                      let last_emit = std::cell::Cell::new(Instant::now());
                      let window_clone = window_worker.clone();
                      let progress_cb = |count| {
                          let now = Instant::now();
                          let last = last_emit.get();
                          if now.duration_since(last) >= Duration::from_millis(100) {
                              let _ = window_clone.emit("indexing-progress", count);
                              last_emit.set(now);
                          }
                      };
                      
                      // Walk tree to build delete list
                      let delete_list = match better_copy_core::walker::build_delete_list(&sources, Some(&cancel_flag_worker), Some(&progress_cb)) {
                          Ok(dl) => dl,
                          Err(e) => {
                              let is_cancelled = e.kind() == std::io::ErrorKind::Interrupted || cancel_flag_worker.load(Ordering::SeqCst);
                              let _ = window_worker.emit("copy-complete", CompletePayload {
                                  files_copied: 0,
                                  bytes_copied: 0,
                                  failures: if is_cancelled { vec![] } else { vec![(first_source.display().to_string(), format!("Failed to build delete list: {}", e))] },
                                  was_cancelled: is_cancelled,
                              });
                              continue;
                          }
                      };
                      
                      let total_files = delete_list.files.len();
                      let _ = window_worker.emit("indexing-progress", total_files);
                      
                      if cancel_flag_worker.load(Ordering::SeqCst) {
                          let _ = window_worker.emit("copy-complete", CompletePayload {
                              files_copied: 0,
                              bytes_copied: 0,
                              failures: vec![],
                              was_cancelled: true,
                          });
                          continue;
                      }

                      // Device profiling
                      let profile = better_copy_core::profiler::profile_device(first_source);
                      let concurrency = profile.concurrency;
                      
                      // Start progress window setup with actual values
                      let _ = window_worker.emit("copy-start", StartPayload {
                          sources: sources.iter().map(|p| p.display().to_string()).collect(),
                          destination: String::new(),
                          description: format!("Deleting selected files ({})...", profile.description),
                          concurrency,
                          total_files,
                          total_bytes: total_files as u64,
                      });
                      
                      // Run delete engine
                      let window_progress = window_worker.clone();
                      let start_time = Instant::now();
                      let last_update = Mutex::new(Instant::now());
                      let last_files = std::sync::atomic::AtomicUsize::new(0);
                      
                      let progress_cb = Box::new(move |completed_files, _completed_bytes| {
                          let now = Instant::now();
                          let mut last_up = last_update.lock().unwrap();
                          let elapsed_since_update = now.duration_since(*last_up);
                          
                          if elapsed_since_update >= Duration::from_millis(250) {
                              let total_elapsed = now.duration_since(start_time);
                              let prev_files = last_files.load(Ordering::Relaxed);
                              let delta_files = completed_files - prev_files;
                              
                              let rate = if elapsed_since_update.as_secs_f64() > 0.0 {
                                  delta_files as f64 / elapsed_since_update.as_secs_f64()
                              } else {
                                  0.0
                              };
                              
                              let avg_rate = if total_elapsed.as_secs_f64() > 0.0 {
                                  completed_files as f64 / total_elapsed.as_secs_f64()
                              } else {
                                  0.0
                              };
                              
                              let remaining_files = if total_files > completed_files {
                                  total_files - completed_files
                              } else {
                                  0
                              };
                              
                              let eta = if avg_rate > 0.0 {
                                  remaining_files as f64 / avg_rate
                              } else {
                                  -1.0
                              };
                              
                              *last_up = now;
                              last_files.store(completed_files, Ordering::Relaxed);
                              
                              let percent = if total_files > 0 {
                                  ((completed_files as f64 / total_files as f64) * 100.0) as u64
                              } else {
                                  0
                              };
                              
                              let _ = window_progress.set_progress_bar(tauri::window::ProgressBarState {
                                  status: Some(tauri::window::ProgressBarStatus::Normal),
                                  progress: Some(percent),
                              });
                              
                              let _ = window_progress.emit("copy-progress", ProgressPayload {
                                  files_completed: completed_files,
                                  bytes_completed: completed_files as u64,
                                  speed_mbps: rate, // Reuse speed_mbps for files/sec
                                  eta_seconds: eta,
                                  total_files,
                                  total_bytes: total_files as u64,
                              });
                          }
                      });
                      
                      let summary = better_copy_core::engine::run_delete_engine_with_delete_list(
                          delete_list,
                          &sources,
                          concurrency,
                          cancel_flag_worker.clone(),
                          Arc::new(AtomicBool::new(false)), // pause not wired to Tauri UI yet
                          Some(progress_cb),
                      );
                      
                      let has_failures = !summary.failures.is_empty();
                      let was_cancelled = summary.was_cancelled;
                      
                      let final_status = if was_cancelled {
                          tauri::window::ProgressBarStatus::None
                      } else if has_failures {
                          tauri::window::ProgressBarStatus::Error
                      } else {
                          tauri::window::ProgressBarStatus::None
                      };
                      
                      let _ = window_worker.set_progress_bar(tauri::window::ProgressBarState {
                          status: Some(final_status),
                          progress: Some(100),
                      });
                      
                      let _ = window_worker.emit("copy-complete", CompletePayload {
                          files_copied: summary.files_copied,
                          bytes_copied: summary.bytes_copied,
                          failures: summary.failures.iter().map(|(p, e)| (p.display().to_string(), e.clone())).collect(),
                          was_cancelled,
                      });
                  }
              }
          }
      });
      
      // Start the global hotkey focus-gated trigger loop
      let trigger = better_copy_core::trigger::HotkeyTrigger::start(move |event| {
          match event {
              better_copy_core::trigger::HotkeyEvent::Paste { clipboard, destination } => {
                  let _ = copy_tx.send(Job::CopyMove {
                      sources: clipboard.paths,
                      dest: destination,
                      is_move: clipboard.is_move,
                  });
              }
              better_copy_core::trigger::HotkeyEvent::Delete { sources } => {
                  let _ = copy_tx.send(Job::Delete { sources });
              }
          }
      }).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to start hotkey trigger: {}", e)))?;
      
      // Manage trigger handle lifecycle
      app.manage(trigger);
      
      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
