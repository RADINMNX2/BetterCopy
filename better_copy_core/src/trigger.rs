use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use windows::core::{Interface, PCWSTR, GUID, VARIANT};
use windows::Win32::Foundation::{
    HWND, LPARAM, WPARAM, LRESULT, HGLOBAL
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, IServiceProvider,
    COINIT_APARTMENTTHREADED
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    RegisterClassW, TranslateMessage, WNDCLASSW, HWND_MESSAGE,
    WM_HOTKEY, WM_USER, GetForegroundWindow, GetClassNameW,
    GetWindowThreadProcessId, PostMessageW,
    WINEVENT_OUTOFCONTEXT, EVENT_SYSTEM_FOREGROUND, MSG,
    GetGUIThreadInfo, GUITHREADINFO
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey,
    SendInput, INPUT, KEYBDINPUT,
    MOD_CONTROL, MOD_SHIFT, VIRTUAL_KEY, KEYBD_EVENT_FLAGS, INPUT_KEYBOARD
};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK
};
use windows::Win32::UI::Shell::{
    IShellWindows, ShellWindows, IWebBrowserApp, IShellBrowser, IFolderView2,
    IPersistFolder2, SHGetPathFromIDListW, SHGetKnownFolderPath, FOLDERID_Desktop,
    KF_FLAG_DEFAULT
};
use windows::Win32::System::DataExchange::{
    OpenClipboard, CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    RegisterClipboardFormatW
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

const SID_S_TOP_LEVEL_BROWSER: GUID = GUID::from_u128(0x4C96BE40_915C_11CF_99D3_00AA004AE837);
const HOTKEY_ID: i32 = 1;
const MSG_RE_REGISTER: u32 = WM_USER + 1;

thread_local! {
    static REGISTERED: std::cell::Cell<bool> = std::cell::Cell::new(false);
    static WINDOW_HWND: std::cell::Cell<HWND> = std::cell::Cell::new(HWND::default());
}

/// Structure representing the clipboard source paths and the cut/copy drop effect.
#[derive(Debug, Clone)]
pub struct ClipboardSources {
    pub paths: Vec<PathBuf>,
    pub is_move: bool,
}

/// Reads CF_HDROP and Preferred DropEffect from the clipboard.
pub fn read_clipboard_sources() -> Result<ClipboardSources, String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return Err("Failed to open clipboard".to_string());
        }
        
        let file_format_available = IsClipboardFormatAvailable(15).is_ok();
            
        if !file_format_available {
            let _ = CloseClipboard();
            return Err("Clipboard does not contain files".to_string());
        }
        
        let h_drop_data = match GetClipboardData(15) {
            Ok(h) if !h.0.is_null() => h,
            _ => {
                let _ = CloseClipboard();
                return Err("Failed to get clipboard data".to_string());
            }
        };
        
        let h_drop = HDROP(h_drop_data.0);
        let count = DragQueryFileW(h_drop, 0xFFFFFFFF, None);
        let mut paths = Vec::with_capacity(count as usize);
        
        for i in 0..count {
            let len = DragQueryFileW(h_drop, i, None);
            if len > 0 {
                let mut buf = vec![0u16; (len + 1) as usize];
                DragQueryFileW(h_drop, i, Some(&mut buf));
                buf.pop(); // Remove null-terminator
                let path_str = String::from_utf16_lossy(&buf);
                paths.push(PathBuf::from(path_str));
            }
        }
        
        // Check for Preferred DropEffect (Cut vs Copy)
        let mut is_move = false;
        let preferred_format_name: Vec<u16> = "Preferred DropEffect".encode_utf16().chain(std::iter::once(0)).collect();
        let format_id = RegisterClipboardFormatW(PCWSTR(preferred_format_name.as_ptr()));
        if format_id != 0 {
            let effect_format_available = IsClipboardFormatAvailable(format_id).is_ok();
                
            if effect_format_available {
                if let Ok(h_effect_data) = GetClipboardData(format_id) {
                    if !h_effect_data.0.is_null() {
                        let h_global = HGLOBAL(h_effect_data.0);
                        let ptr = GlobalLock(h_global);
                        if !ptr.is_null() {
                            let effect = *(ptr as *const u32);
                            if effect == 2 {
                                is_move = true;
                            }
                            let _ = GlobalUnlock(h_global);
                        }
                    }
                }
            }
        }
        
        let _ = CloseClipboard();
        
        if paths.is_empty() {
            Err("No file paths found in clipboard".to_string())
        } else {
            Ok(ClipboardSources { paths, is_move })
        }
    }
}

/// Retrieves the Desktop folder path.
fn get_desktop_path() -> Result<PathBuf, String> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None);
        match path {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr() as *const _));
                if s.is_empty() {
                    Err("Desktop path is empty".to_string())
                } else {
                    Ok(PathBuf::from(s))
                }
            }
            Err(e) => Err(format!("Failed to get desktop path: {}", e)),
        }
    }
}

/// Resolves the current directory of the active Explorer window or the Desktop.
pub fn resolve_active_explorer_path(foreground_hwnd: HWND) -> Result<PathBuf, String> {
    unsafe {
        let mut class_name = [0u16; 256];
        let len = GetClassNameW(foreground_hwnd, &mut class_name);
        if len > 0 {
            let class_str = String::from_utf16_lossy(&class_name[..len as usize]);
            if class_str == "Progman" || class_str == "WorkerW" {
                return get_desktop_path();
            }
        }

        let shell_windows: IShellWindows = match CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) {
            Ok(sw) => sw,
            Err(e) => return Err(format!("Failed to create ShellWindows: {}", e)),
        };
        
        let count = match shell_windows.Count() {
            Ok(c) => c,
            Err(e) => return Err(format!("Failed to get shell windows count: {}", e)),
        };
        
        for i in 0..count {
            let variant = VARIANT::from(i as i32);
            let dispatch = match shell_windows.Item(&variant) {
                Ok(d) => d,
                Err(_) => continue,
            };
            
            let web_browser: IWebBrowserApp = match dispatch.cast() {
                Ok(wb) => wb,
                Err(_) => continue,
            };
            
            let hwnd = match web_browser.HWND() {
                Ok(h) => h,
                Err(_) => continue,
            };
            
            if hwnd.0 == foreground_hwnd.0 as isize {
                let service_provider: IServiceProvider = web_browser.cast().map_err(|e| format!("Cast to IServiceProvider failed: {}", e))?;
                let shell_browser: IShellBrowser = service_provider.QueryService(&SID_S_TOP_LEVEL_BROWSER).map_err(|e| format!("QueryService for IShellBrowser failed: {}", e))?;
                let shell_view = shell_browser.QueryActiveShellView().map_err(|e| format!("QueryActiveShellView failed: {}", e))?;
                let folder_view: IFolderView2 = shell_view.cast().map_err(|e| format!("Cast to IFolderView2 failed: {}", e))?;
                let folder = folder_view.GetFolder::<IPersistFolder2>().map_err(|e| format!("GetFolder for IPersistFolder2 failed: {}", e))?;
                
                let pidl = folder.GetCurFolder().map_err(|e| format!("GetCurFolder failed: {}", e))?;
                
                let mut path_buf = [0u16; 260];
                if SHGetPathFromIDListW(pidl, &mut path_buf).as_bool() {
                    let path_len = path_buf.iter().position(|&x| x == 0).unwrap_or(path_buf.len());
                    let path_str = String::from_utf16_lossy(&path_buf[..path_len]);
                    return Ok(PathBuf::from(path_str));
                } else {
                    return Err("SHGetPathFromIDListW failed".to_string());
                }
            }
        }
        
        Err("Active Explorer window path could not be resolved".to_string())
    }
}

/// Evaluates if the foreground window is Explorer or Desktop.
fn is_explorer_or_desktop(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        let mut class_name = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut class_name);
        if len > 0 {
            let class_str = String::from_utf16_lossy(&class_name[..len as usize]);
            class_str == "CabinetWClass" || class_str == "Progman" || class_str == "WorkerW"
        } else {
            false
        }
    }
}

/// Dynamically updates the hotkey registration state.
fn update_hotkey_registration(hwnd: HWND, force_unregister: bool) {
    unsafe {
        let fg = GetForegroundWindow();
        let eligible = !force_unregister && is_explorer_or_desktop(fg);
        
        REGISTERED.with(|reg| {
            let currently_registered = reg.get();
            if eligible && !currently_registered {
                let res = RegisterHotKey(hwnd, HOTKEY_ID, MOD_CONTROL | MOD_SHIFT, 0x56);
                println!("[Trigger] Registering hotkey: Result={:?}", res);
                if res.is_ok() {
                    reg.set(true);
                }
            } else if !eligible && currently_registered {
                let res = UnregisterHotKey(hwnd, HOTKEY_ID);
                println!("[Trigger] Unregistering hotkey: Result={:?}", res);
                reg.set(false);
            }
        });
    }
}

/// Checks if the currently focused control is a rename or edit box.
fn check_rename_box_focus(foreground_hwnd: HWND) -> bool {
    unsafe {
        let mut process_id = 0u32;
        let thread_id = GetWindowThreadProcessId(foreground_hwnd, Some(&mut process_id));
        if thread_id == 0 {
            return false;
        }
        
        let mut gui_info = GUITHREADINFO::default();
        gui_info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        
        if GetGUIThreadInfo(thread_id, &mut gui_info).is_ok() {
            let focus_hwnd = gui_info.hwndFocus;
            if !focus_hwnd.0.is_null() {
                let mut class_name = [0u16; 256];
                let len = GetClassNameW(focus_hwnd, &mut class_name);
                if len > 0 {
                    let class_str = String::from_utf16_lossy(&class_name[..len as usize]);
                    return class_str == "Edit";
                }
            }
        }
        false
    }
}

/// Replays Ctrl+Shift+V to the system using SendInput.
fn replay_hotkey(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
        REGISTERED.with(|reg| reg.set(false));
        
        let mut inputs = [INPUT::default(); 6];
        
        // Ctrl Down
        inputs[0].r#type = INPUT_KEYBOARD;
        inputs[0].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x11), // VK_CONTROL
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(0),
            time: 0,
            dwExtraInfo: 0,
        };
        
        // Shift Down
        inputs[1].r#type = INPUT_KEYBOARD;
        inputs[1].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x10), // VK_SHIFT
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(0),
            time: 0,
            dwExtraInfo: 0,
        };
        
        // V Down
        inputs[2].r#type = INPUT_KEYBOARD;
        inputs[2].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x56), // V
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(0),
            time: 0,
            dwExtraInfo: 0,
        };
        
        // V Up
        inputs[3].r#type = INPUT_KEYBOARD;
        inputs[3].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x56),
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(2), // KEYEVENTF_KEYUP
            time: 0,
            dwExtraInfo: 0,
        };
        
        // Shift Up
        inputs[4].r#type = INPUT_KEYBOARD;
        inputs[4].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x10),
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(2), // KEYEVENTF_KEYUP
            time: 0,
            dwExtraInfo: 0,
        };
        
        // Ctrl Up
        inputs[5].r#type = INPUT_KEYBOARD;
        inputs[5].Anonymous.ki = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0x11),
            wScan: 0,
            dwFlags: KEYBD_EVENT_FLAGS(2), // KEYEVENTF_KEYUP
            time: 0,
            dwExtraInfo: 0,
        };
        
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        
        let _ = PostMessageW(hwnd, MSG_RE_REGISTER, WPARAM(0), LPARAM(0));
    }
}

/// Out-of-context hook callback for foreground window changes.
unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _dw_event_thread: u32,
    _dwms_event_time: u32,
) {
    unsafe {
        WINDOW_HWND.with(|w_hwnd| {
            let hwnd = w_hwnd.get();
            if !hwnd.0.is_null() {
                let fg = GetForegroundWindow();
                let mut class_name = [0u16; 256];
                let len = GetClassNameW(fg, &mut class_name);
                let class_str = String::from_utf16_lossy(&class_name[..len as usize]);
                println!("[Trigger] Focus shifted to window class: {}", class_str);
                update_hotkey_registration(hwnd, false);
            }
        });
    }
}

/// Window procedure for the message-only trigger window.
unsafe extern "system" fn trigger_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_HOTKEY => {
                println!("[Trigger] WM_HOTKEY message received by window!");
                if wparam.0 as i32 == HOTKEY_ID {
                    let fg = GetForegroundWindow();
                    if check_rename_box_focus(fg) {
                        println!("[Trigger] Focus is inside a rename/edit box, replaying native keys.");
                        replay_hotkey(hwnd);
                    } else {
                        if let Some(cb_mutex) = TRIGGER_CALLBACK.get() {
                            if let Some(cb) = cb_mutex.lock().unwrap().as_ref() {
                                cb();
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            MSG_RE_REGISTER => {
                update_hotkey_registration(hwnd, false);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// Global thread-safe slot to hold trigger callback.
static TRIGGER_CALLBACK: OnceLock<Mutex<Option<Box<dyn Fn() + Send + Sync + 'static>>>> = OnceLock::new();

/// Handler for the active trigger loop.
pub struct HotkeyTrigger {
    exit_flag: Arc<AtomicBool>,
    hwnd: HWND,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl HotkeyTrigger {
    /// Starts the hotkey monitoring thread.
    pub fn start<F>(trigger_callback: F) -> Result<Self, String>
    where
        F: Fn(ClipboardSources, PathBuf) + Send + Sync + 'static,
    {
        let exit_flag = Arc::new(AtomicBool::new(false));
        let exit_flag_clone = exit_flag.clone();
        
        let (hwnd_tx, hwnd_rx) = std::sync::mpsc::channel();
        
        // Wrap the user-facing callback to resolve clipboard and Explorer path upon trigger
        let wrapped_cb = move || {
            let fg = unsafe { GetForegroundWindow() };
            let dest = match resolve_active_explorer_path(fg) {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("Trigger error: {}", e);
                    return;
                }
            };
            
            let clipboard = match read_clipboard_sources() {
                Ok(sources) => sources,
                Err(e) => {
                    eprintln!("Trigger error: {}", e);
                    return;
                }
            };
            
            trigger_callback(clipboard, dest);
        };
        
        let cb_mutex = TRIGGER_CALLBACK.get_or_init(|| Mutex::new(None));
        *cb_mutex.lock().unwrap() = Some(Box::new(wrapped_cb));

        let thread_handle = thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                
                let class_name: Vec<u16> = "BetterCopyTriggerClass".encode_utf16().chain(std::iter::once(0)).collect();
                let wnd_class = WNDCLASSW {
                    lpfnWndProc: Some(trigger_window_proc),
                    hInstance: windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default().into(),
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                
                let _atom = RegisterClassW(&wnd_class);
                
                let hwnd = CreateWindowExW(
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                    PCWSTR(class_name.as_ptr()),
                    PCWSTR(std::ptr::null()),
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
                    0, 0, 0, 0,
                    HWND_MESSAGE,
                    None,
                    None,
                    None
                );
                
                let hwnd = match hwnd {
                    Ok(h) if !h.0.is_null() => h,
                    _ => {
                        let _ = hwnd_tx.send(Err("Failed to create message-only window".to_string()));
                        CoUninitialize();
                        return;
                    }
                };
                
                WINDOW_HWND.with(|w_hwnd| w_hwnd.set(hwnd));
                let _ = hwnd_tx.send(Ok(hwnd.0 as isize));
                
                // Hook foreground focus changes
                let hook = SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT
                );
                
                // Initialize hotkey state based on current focus
                update_hotkey_registration(hwnd, false);
                
                let mut msg = MSG::default();
                while !exit_flag_clone.load(Ordering::Relaxed) {
                    let has_msg = windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                        &mut msg,
                        HWND::default(),
                        0,
                        0,
                        windows::Win32::UI::WindowsAndMessaging::PM_REMOVE
                    );
                    
                    if has_msg.as_bool() {
                        if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                            break;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    } else {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                
                // Cleanup
                if !hook.0.is_null() {
                    let _ = UnhookWinEvent(hook);
                }
                update_hotkey_registration(hwnd, true);
                let _ = DestroyWindow(hwnd);
                CoUninitialize();
            }
        });
        
        let hwnd_val = match hwnd_rx.recv() {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("Trigger thread channel disconnected".to_string()),
        };
        let hwnd = HWND(hwnd_val as *mut _);
        
        Ok(HotkeyTrigger {
            exit_flag,
            hwnd,
            thread_handle: Some(thread_handle),
        })
    }
}

impl Drop for HotkeyTrigger {
    fn drop(&mut self) {
        self.exit_flag.store(true, Ordering::Relaxed);
        unsafe {
            let _ = PostMessageW(self.hwnd, windows::Win32::UI::WindowsAndMessaging::WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread_handle.take() {
            let _ = t.join();
        }
        if let Some(cb_mutex) = TRIGGER_CALLBACK.get() {
            *cb_mutex.lock().unwrap() = None;
        }
    }
}

unsafe impl Send for HotkeyTrigger {}
unsafe impl Sync for HotkeyTrigger {}
