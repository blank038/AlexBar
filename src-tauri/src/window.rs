use std::sync::{
    atomic::{AtomicI64, AtomicU32, Ordering},
    Mutex,
};

use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position, Size};

const SUPPRESS_OPEN_AFTER_FOCUS_LOSS_MS: i64 = 250;
const POPOVER_WIDTH: f64 = 320.0;
const POPOVER_INITIAL_HEIGHT: u32 = 360;
const POPOVER_MIN_HEIGHT: f64 = 240.0;
const POPOVER_MAX_HEIGHT: f64 = 720.0;
const POPOVER_WORK_AREA_MARGIN: f64 = 80.0;
static LAST_FOCUS_LOSS_HIDE_AT: AtomicI64 = AtomicI64::new(0);
static REQUESTED_POPOVER_HEIGHT_LOGICAL: AtomicU32 = AtomicU32::new(POPOVER_INITIAL_HEIGHT);
static LAST_POPOVER_ANCHOR: Mutex<Option<PhysicalPosition<f64>>> = Mutex::new(None);

#[cfg(windows)]
pub fn apply_platform_chrome<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        DWM_WINDOW_CORNER_PREFERENCE,
    };

    let Ok(hwnd) = window.hwnd() else { return };
    let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn apply_platform_chrome<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>) {}

pub fn toggle_popover(app: &AppHandle, position: Option<PhysicalPosition<f64>>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    let visible = window.is_visible().unwrap_or(false);
    if visible {
        let _ = window.hide();
        return;
    }

    if recently_hidden_by_focus_loss() {
        return;
    }
    if let Some(position) = position {
        remember_popover_anchor(position);
        let _ = place_window_near(app, &window, position);
    }
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn open_settings_window<R: tauri::Runtime>(app: &AppHandle<R>) {
    if let Some(settings_window) = app.get_webview_window("settings") {
        let _ = settings_window.show();
        let _ = settings_window.set_focus();
    }

    if let Some(main_window) = app.get_webview_window("main") {
        let _ = main_window.hide();
    }
}

pub fn set_popover_height<R: tauri::Runtime>(
    app: &AppHandle<R>,
    height_logical: u32,
) -> tauri::Result<()> {
    REQUESTED_POPOVER_HEIGHT_LOGICAL.store(height_logical, Ordering::Relaxed);

    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };

    if window.is_visible().unwrap_or(false) {
        if let Some(position) = last_popover_anchor() {
            return place_window_near(app, &window, position);
        }
    }

    let monitor = match window.current_monitor()? {
        Some(monitor) => Some(monitor),
        None => app.primary_monitor()?,
    };
    let height = clamp_popover_height(height_logical as f64, monitor.as_ref());
    window.set_size(Size::Logical(LogicalSize::new(POPOVER_WIDTH, height)))
}

pub fn note_popover_hidden_by_focus_loss() {
    LAST_FOCUS_LOSS_HIDE_AT.store(now_millis(), Ordering::Relaxed);
}

fn recently_hidden_by_focus_loss() -> bool {
    let last = LAST_FOCUS_LOSS_HIDE_AT.load(Ordering::Relaxed);
    last > 0 && now_millis() - last <= SUPPRESS_OPEN_AFTER_FOCUS_LOSS_MS
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn remember_popover_anchor(position: PhysicalPosition<f64>) {
    *LAST_POPOVER_ANCHOR
        .lock()
        .expect("popover anchor mutex poisoned") = Some(position);
}

fn last_popover_anchor() -> Option<PhysicalPosition<f64>> {
    *LAST_POPOVER_ANCHOR
        .lock()
        .expect("popover anchor mutex poisoned")
}

fn place_window_near<R: tauri::Runtime>(
    app: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
    cursor_position: PhysicalPosition<f64>,
) -> tauri::Result<()> {
    let monitor = match app.monitor_from_point(cursor_position.x, cursor_position.y)? {
        Some(monitor) => Some(monitor),
        None => app.primary_monitor()?,
    };
    let scale_factor = monitor
        .as_ref()
        .map_or(1.0, |monitor| monitor.scale_factor());
    let height = clamp_popover_height(
        REQUESTED_POPOVER_HEIGHT_LOGICAL.load(Ordering::Relaxed) as f64,
        monitor.as_ref(),
    );
    let physical_w = (POPOVER_WIDTH * scale_factor).round() as u32;
    let physical_h = (height * scale_factor).round() as u32;
    window.set_size(Size::Physical(PhysicalSize::new(physical_w, physical_h)))?;

    let physical_w = physical_w as i32;
    let physical_h = physical_h as i32;
    let work_area = monitor.as_ref().map(|monitor| {
        let area = monitor.work_area();
        (
            area.position.x,
            area.position.y,
            area.size.width as i32,
            area.size.height as i32,
        )
    });

    let mut x = cursor_position.x.round() as i32 - physical_w / 2;
    let mut y = cursor_position.y.round() as i32 - physical_h - 12;

    if let Some((monitor_x, monitor_y, monitor_w, monitor_h)) = work_area {
        let min_x = monitor_x + 8;
        let max_x = monitor_x + monitor_w - physical_w - 8;
        let min_y = monitor_y + 8;
        let max_y = monitor_y + monitor_h - physical_h - 8;
        x = x.clamp(min_x, max_x.max(min_x));
        if y < min_y {
            y = (cursor_position.y.round() as i32 + 12).clamp(min_y, max_y.max(min_y));
        } else {
            y = y.clamp(min_y, max_y.max(min_y));
        }
    }

    window.set_position(Position::Physical(PhysicalPosition::new(x, y)))
}

fn clamp_popover_height(height: f64, monitor: Option<&tauri::Monitor>) -> f64 {
    let max_height = monitor
        .map(max_popover_height)
        .unwrap_or(POPOVER_MAX_HEIGHT)
        .max(POPOVER_MIN_HEIGHT);

    height.clamp(POPOVER_MIN_HEIGHT, max_height)
}

fn max_popover_height(monitor: &tauri::Monitor) -> f64 {
    let work_area = monitor.work_area();
    let work_area_height = work_area.size.height as f64 / monitor.scale_factor();
    (work_area_height - POPOVER_WORK_AREA_MARGIN).min(POPOVER_MAX_HEIGHT)
}
