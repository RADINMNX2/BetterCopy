use std::cell::RefCell;
use std::sync::{Arc, Mutex as SyncMutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, DrawTextW, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, EndPaint, FillRect, FrameRect,
    GetStockObject, HDC, HGDIOBJ, InvalidateRect, PAINTSTRUCT, RoundRect, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT, DRAW_TEXT_FORMAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, HMENU, KillTimer, RegisterClassW,
    ShowWindow, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WNDPROC,
    WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::{APP, UiMode};

const OVL_W: i32 = 440;
const OVL_H: i32 = 330;

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    TogglePause,
    Cancel,
    Close,
    ViewLog,
    RunElevated,
}

static ACTION_HANDLER: OnceLock<SyncMutex<Box<dyn Fn(OverlayAction) + Send + 'static>>> =
    OnceLock::new();

pub fn set_action_handler(f: impl Fn(OverlayAction) + Send + 'static) {
    let _ = ACTION_HANDLER.set(SyncMutex::new(Box::new(f)));
}

fn dispatch(action: OverlayAction) {
    if let Some(guard) = ACTION_HANDLER.get() {
        if let Ok(m) = guard.lock() {
            m(action);
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Buttons {
    pause: Option<RECT>,
    cancel: Option<RECT>,
    close: Option<RECT>,
    view_log: Option<RECT>,
    elevated: Option<RECT>,
}

thread_local! {
    static BTN: RefCell<Buttons> = RefCell::new(Buttons::default());
}

fn class_name() -> PCWSTR {
    w!("BetterCopyDashboard")
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | (r as u32))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn draw_text(hdc: HDC, text: &str, rect: &mut RECT, fmt: DRAW_TEXT_FORMAT, color: COLORREF) {
    let mut buf = wide(text);
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        let _ = DrawTextW(hdc, &mut buf, rect, fmt);
    }
}

fn fill_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let _ = FillRect(hdc, rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

fn round_fill(hdc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old = SelectObject(hdc, brush);
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            8,
            8,
        );
        let _ = SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

fn frame_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let _ = FrameRect(hdc, rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

fn draw_button(hdc: HDC, rect: &RECT, label: &str, active: bool) {
    let face = if active { rgb(56, 120, 255) } else { rgb(64, 68, 78) };
    fill_rect(hdc, rect, face);
    frame_rect(hdc, rect, rgb(95, 100, 112));
    let mut tr = *rect;
    tr.bottom += 20;
    let fmt = DT_CENTER | DT_VCENTER | DT_SINGLELINE;
    draw_text(hdc, label, &mut tr, fmt, rgb(235, 236, 240));
}

unsafe fn paint(hdc: HDC) {
    let app_guard = crate::app::APP.get();
    let Some(app) = app_guard else { return };
    let ui = app.ui.lock().unwrap().clone();
    let is_paused = app.pause.load(std::sync::atomic::Ordering::SeqCst);

    fill_rect(hdc, &RECT { left: 0, top: 0, right: OVL_W, bottom: OVL_H }, rgb(28, 29, 33));
    frame_rect(hdc, &RECT { left: 0, top: 0, right: OVL_W - 1, bottom: OVL_H - 1 }, rgb(70, 74, 84));

    let font = GetStockObject(DEFAULT_GUI_FONT);
    let old_font = SelectObject(hdc, font);

    let mut title_rect = RECT { left: 16, top: 18, right: OVL_W - 16, bottom: 46 };
    let title = if ui.title.is_empty() { "BetterCopy".to_string() } else { ui.title.clone() };
    draw_text(hdc, &title, &mut title_rect, DT_LEFT | DT_SINGLELINE, rgb(235, 236, 240));

    let mut detail_rect = RECT { left: 16, top: 50, right: OVL_W - 16, bottom: 70 };
    draw_text(hdc, &ui.detail, &mut detail_rect, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS, rgb(150, 155, 165));

    // Progress bar
    let bar = RECT { left: 16, top: 104, right: OVL_W - 16, bottom: 122 };
    round_fill(hdc, &bar, rgb(45, 47, 54));
    let pct = ui.percent();
    let fill_w = ((bar.right - bar.left) as f64 * pct / 100.0) as i32;
    if fill_w > 0 {
        let fill = RECT { left: bar.left, top: bar.top, right: bar.left + fill_w, bottom: bar.bottom };
        round_fill(hdc, &fill, rgb(56, 120, 255));
    }
    frame_rect(hdc, &bar, rgb(70, 74, 84));

    let mut stats_rect = RECT { left: 16, top: 130, right: OVL_W - 16, bottom: 148 };
    let stats = if ui.is_delete {
        format!(
            "{} / {} items",
            ui.files_done, ui.total_files
        )
    } else {
        let mb_done = ui.bytes_done as f64 / (1024.0 * 1024.0);
        let mb_total = ui.total_bytes as f64 / (1024.0 * 1024.0);
        format!(
            "{:.1} / {:.1} MB · {:.1} MB/s · {}",
            mb_done,
            mb_total,
            ui.speed_mbps,
            crate::format_eta(ui.eta_secs),
        )
    };
    draw_text(hdc, &stats, &mut stats_rect, DT_LEFT | DT_SINGLELINE, rgb(190, 193, 202));

    let mut pct_rect = RECT { left: 16, top: 154, right: OVL_W - 16, bottom: 172 };
    let pct_text = format!("{:.1}%", pct);
    draw_text(hdc, &pct_text, &mut pct_rect, DT_LEFT | DT_SINGLELINE, rgb(56, 120, 255));

    let mut status_rect = RECT { left: 16, top: 178, right: OVL_W - 16, bottom: 196 };
    let status = match ui.mode {
        UiMode::Analyzing => "Analyzing files…".to_string(),
        UiMode::Running if is_paused => "PAUSED".to_string(),
        UiMode::Running if ui.cancelled => "Cancelling…".to_string(),
        UiMode::Running => format!("Copying — {} failures so far", ui.failures.len()),
        UiMode::Done if ui.cancelled => "Cancelled — some items may be incomplete".to_string(),
        UiMode::Done if !ui.failures.is_empty() => format!("Completed with {} failure(s)", ui.failures.len()),
        UiMode::Done => "Completed".to_string(),
        _ => String::new(),
    };
    let status_color = if ui.mode == UiMode::Done && ui.failures.is_empty() && !ui.cancelled {
        rgb(92, 200, 120)
    } else if ui.mode == UiMode::Done || is_paused {
        rgb(240, 190, 80)
    } else {
        rgb(190, 193, 202)
    };
    draw_text(hdc, &status, &mut status_rect, DT_LEFT | DT_SINGLELINE, status_color);

    // Failure preview lines
    let mut top = 202;
    let failure_limit = ui.failures.iter().take(3).cloned().collect::<Vec<_>>();
    for (path, reason) in &failure_limit {
        if top > OVL_H - 76 {
            break;
        }
        let line = format!("• {} — {}", path.display(), reason);
        let mut lr = RECT { left: 16, top, right: OVL_W - 16, bottom: top + 16 };
        draw_text(hdc, &line, &mut lr, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS, rgb(220, 120, 120));
        top += 15;
    }
    if ui.failures.len() > 3 {
        let more = format!("… and {} more", ui.failures.len() - 3);
        let mut lr = RECT { left: 16, top, right: OVL_W - 16, bottom: top + 16 };
        draw_text(hdc, &more, &mut lr, DT_LEFT | DT_SINGLELINE, rgb(150, 155, 165));
    }

    // Buttons
    unsafe fn layout_button(hdc: HDC, x_right: i32, y: i32, w: i32, h: i32, label: &str) -> RECT {
        let rect = RECT { left: x_right - w, top: y, right: x_right, bottom: y + h };
        draw_button(hdc, &rect, label, true);
        rect
    }

    let by = OVL_H - 46;
    let bh = 30;
    let mut buttons = Buttons::default();
    let mut right = OVL_W - 16;
    match ui.mode {
        UiMode::Hidden | UiMode::Analyzing => {
            buttons.cancel = Some(layout_button(hdc, right, by, 96, bh, "Cancel"));
            right -= 108;
        }
        UiMode::Running => {
            buttons.cancel = Some(layout_button(hdc, right, by, 96, bh, "Cancel"));
            right -= 108;
            let label = if is_paused { "Resume" } else { "Pause" };
            buttons.pause = Some(layout_button(hdc, right, by, 96, bh, label));
        }
        UiMode::Done => {
            if !ui.failures.is_empty() && !ui.cancelled {
                buttons.elevated = Some(layout_button(hdc, right, by, 116, bh, "Run as Admin"));
                right -= 128;
            }
            buttons.view_log = Some(layout_button(hdc, right, by, 96, bh, "View Log"));
            right -= 108;
            buttons.close = Some(layout_button(hdc, right, by, 80, bh, "Close"));
        }
    }

    BTN.with(|b| *b.borrow_mut() = buttons);

    let _ = SelectObject(hdc, old_font);
}

unsafe fn hit_test(x: i32, y: i32) -> Option<OverlayAction> {
    BTN.with(|b| {
        let buttons = *b.borrow();
        let in_rect = |r: &Option<RECT>| match r {
            Some(rect) => x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom,
            None => false,
        };
        if in_rect(&buttons.pause) {
            Some(OverlayAction::TogglePause)
        } else if in_rect(&buttons.cancel) {
            Some(OverlayAction::Cancel)
        } else if in_rect(&buttons.close) {
            Some(OverlayAction::Close)
        } else if in_rect(&buttons.view_log) {
            Some(OverlayAction::ViewLog)
        } else if in_rect(&buttons.elevated) {
            Some(OverlayAction::RunElevated)
        } else {
            None
        }
    })
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_TIMER: u32 = 0x0113;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_PAINT: u32 = 0x000F;

    match msg {
        crate::app::WM_UI_REFRESH => {
            let _ = InvalidateRect(hwnd, None, BOOL(0));
            LRESULT(0)
        }
        crate::app::WM_UI_CLOSE_DELAY => {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, 2, 1600, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 as usize == 2 {
                let _ = KillTimer(hwnd, 2);
                hide(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let lp = lparam.0 as u32;
            let x = (lp & 0xffff) as i32;
            let y = ((lp >> 16) & 0xffff) as i32;
            if let Some(action) = hit_test(x, y) {
                dispatch(action);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub struct Overlay {
    pub hwnd: HWND,
}

impl Overlay {
    /// Creates the dashboard window (hidden until first job).
    pub fn create(instance: HINSTANCE) -> Overlay {
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: overlay_wnd_proc,
                hInstance: instance,
                lpszClassName: class_name(),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let x = (sw - OVL_W) / 2;
            let y = (sh - OVL_H) / 3;

            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class_name(),
                w!("BetterCopy"),
                WS_POPUP | WS_CLIPCHILDREN,
                x,
                y,
                OVL_W,
                OVL_H,
                HWND(std::ptr::null_mut()),
                HMENU(std::ptr::null_mut()),
                instance,
                None,
            )
            .unwrap_or(HWND(std::ptr::null_mut()));
            Overlay { hwnd }
        }
    }
}

pub fn show(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(hwnd, None, BOOL(0));
    }
    if let Some(app) = APP.get() {
        app.set_overlay_visible(true);
    }
}

pub fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    if let Some(app) = APP.get() {
        app.set_overlay_visible(false);
    }
}

#[allow(dead_code)]
pub fn update(_app: &Arc<crate::app::AppState>) {
    // Repaint requests are driven by AppState::refresh().
}