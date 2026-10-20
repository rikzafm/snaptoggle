//! SnapToggle – one-click tray toggle for Windows 11 "Snap windows"
//! (Settings > System > Multitasking > Snap windows).
//!
//! * Lives in the notification area (no taskbar button, no window)
//! * Left-click the tray icon  -> toggle Snap windows on/off
//! * Right-click               -> menu (toggle, open settings, start with Windows, exit)
//! * Registers itself to auto-start with Windows (can be disabled from the menu)
//! * Icon switches colour/grey so you can see the state at a glance

#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{mem, ptr};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ---------------------------------------------------------------- constants
const SPI_GETWINARRANGING: u32 = 0x0082;
const SPI_SETWINARRANGING: u32 = 0x0083;
const SPIF_UPDATEINIFILE: u32 = 0x0001;
const SPIF_SENDCHANGE: u32 = 0x0002;

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_SYNC: usize = 1;

const ID_TOGGLE: usize = 1001;
const ID_SETTINGS: usize = 1002;
const ID_AUTOSTART: usize = 1003;
const ID_EXIT: usize = 1099;

const ICON_ON: usize = 1; // resource IDs set in build.rs
const ICON_OFF: usize = 2;

const APP_NAME: &str = "SnapToggle";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const DESKTOP_KEY: &str = r"Control Panel\Desktop";
const APP_KEY: &str = r"Software\SnapToggle";

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static LAST_STATE: AtomicBool = AtomicBool::new(true);

// ------------------------------------------------------------ small helpers
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn reg_set_sz(subkey: &str, name: &str, value: &str) -> bool {
    let mut key: HKEY = ptr::null_mut();
    let sk = wide(subkey);
    if RegCreateKeyExW(
        HKEY_CURRENT_USER,
        sk.as_ptr(),
        0,
        ptr::null(),
        0,
        KEY_SET_VALUE,
        ptr::null(),
        &mut key,
        ptr::null_mut(),
    ) != 0
    {
        return false;
    }
    let n = wide(name);
    let v = wide(value);
    let r = RegSetValueExW(key, n.as_ptr(), 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32);
    RegCloseKey(key);
    r == 0
}

unsafe fn reg_get_sz(subkey: &str, name: &str) -> Option<String> {
    let sk = wide(subkey);
    let n = wide(name);
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        sk.as_ptr(),
        n.as_ptr(),
        RRF_RT_REG_SZ,
        ptr::null_mut(),
        buf.as_mut_ptr() as *mut c_void,
        &mut size,
    );
    if r != 0 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

unsafe fn reg_delete_value(subkey: &str, name: &str) {
    let sk = wide(subkey);
    let n = wide(name);
    RegDeleteKeyValueW(HKEY_CURRENT_USER, sk.as_ptr(), n.as_ptr());
}

// ------------------------------------------------------- Snap windows state
unsafe fn spi_get() -> Option<bool> {
    let mut v: i32 = 0;
    if SystemParametersInfoW(SPI_GETWINARRANGING, 0, &mut v as *mut i32 as *mut c_void, 0) != 0 {
        Some(v != 0)
    } else {
        None
    }
}

/// The value the Settings page persists (default is ON when missing).
unsafe fn registry_state() -> bool {
    reg_get_sz(DESKTOP_KEY, "WindowArrangementActive")
        .map(|s| s.trim() != "0")
        .unwrap_or(true)
}

unsafe fn snap_enabled() -> bool {
    spi_get().unwrap_or_else(|| registry_state())
}

unsafe fn spi_set(on: bool) -> bool {
    let flags = SPIF_UPDATEINIFILE | SPIF_SENDCHANGE;
    SystemParametersInfoW(
        SPI_SETWINARRANGING,
        on as u32,
        ptr::null_mut(),
        flags,
    ) != 0
}

/// Returns true if Windows reports the requested state afterwards.
unsafe fn set_snap(on: bool) -> bool {
    // 1) Apply live and persist via SystemParametersInfoW.
    // Windows expects uiParam = 1 (on) or 0 (off), pvParam = NULL,
    // and flags = SPIF_UPDATEINIFILE | SPIF_SENDCHANGE.
    let ok = spi_set(on);

    // 2) Explicitly persist to registry as well.
    reg_set_sz(DESKTOP_KEY, "WindowArrangementActive", if on { "1" } else { "0" });

    // 3) Tell Explorer / Settings the value changed.
    let area = wide("WindowArrangementActive");
    SendMessageTimeoutW(
        HWND_BROADCAST,
        WM_SETTINGCHANGE,
        SPI_SETWINARRANGING as usize,
        area.as_ptr() as LPARAM,
        SMTO_ABORTIFHUNG,
        300,
        ptr::null_mut(),
    );
    SendMessageTimeoutW(
        HWND_BROADCAST,
        WM_SETTINGCHANGE,
        0,
        area.as_ptr() as LPARAM,
        SMTO_ABORTIFHUNG,
        300,
        ptr::null_mut(),
    );

    ok && (spi_get() == Some(on))
}

// ---------------------------------------------------------------- autostart
unsafe fn autostart_enabled() -> bool {
    reg_get_sz(RUN_KEY, APP_NAME).is_some()
}

unsafe fn enable_autostart() {
    if let Ok(exe) = std::env::current_exe() {
        let cmd = format!("\"{}\"", exe.display());
        reg_set_sz(RUN_KEY, APP_NAME, &cmd);
        reg_set_sz(APP_KEY, "AutoStartOptOut", "0");
    }
}

unsafe fn disable_autostart() {
    reg_delete_value(RUN_KEY, APP_NAME);
    reg_set_sz(APP_KEY, "AutoStartOptOut", "1");
}

/// Auto-start by default; refreshes the path if the exe was moved,
/// unless the user explicitly opted out from the tray menu.
unsafe fn ensure_autostart_default() {
    if reg_get_sz(APP_KEY, "AutoStartOptOut").as_deref() != Some("1") {
        enable_autostart();
    }
}

// --------------------------------------------------------------- tray icon
unsafe fn load_icon(on: bool) -> HICON {
    let hinst = GetModuleHandleW(ptr::null());
    let id = if on { ICON_ON } else { ICON_OFF };
    let w = GetSystemMetrics(SM_CXSMICON);
    let h = GetSystemMetrics(SM_CYSMICON);
    let icon = LoadImageW(hinst, id as *const u16, IMAGE_ICON, w, h, LR_DEFAULTCOLOR | LR_SHARED) as HICON;
    if icon.is_null() {
        LoadIconW(ptr::null_mut(), IDI_APPLICATION)
    } else {
        icon
    }
}

unsafe fn tray(hwnd: HWND, action: u32, on: bool) {
    let mut n: NOTIFYICONDATAW = mem::zeroed();
    n.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    n.hWnd = hwnd;
    n.uID = 1;
    n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = load_icon(on);

    let tip = if on {
        "SnapToggle - Snap windows: ON (click to turn off)"
    } else {
        "SnapToggle - Snap windows: OFF (click to turn on)"
    };
    for (i, c) in tip.encode_utf16().take(127).enumerate() {
        n.szTip[i] = c;
    }
    Shell_NotifyIconW(action, &n);
}

unsafe fn tray_remove(hwnd: HWND) {
    let mut n: NOTIFYICONDATAW = mem::zeroed();
    n.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    n.hWnd = hwnd;
    n.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &n);
}

unsafe fn toggle(hwnd: HWND) {
    let want = !snap_enabled();
    let ok = set_snap(want);
    let on = snap_enabled();
    LAST_STATE.store(on, Ordering::Relaxed);
    tray(hwnd, NIM_MODIFY, on);

    if !ok {
        let text = wide(&format!(
            "Windows did not accept the change.\n\nRequested: {}\nSystem reports: {}\nRegistry says: {}",
            if want { "ON" } else { "OFF" },
            if on { "ON" } else { "OFF" },
            if registry_state() { "ON" } else { "OFF" },
        ));
        let title = wide("SnapToggle");
        MessageBoxW(hwnd, text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING);
    }
}

/// Picks up changes made elsewhere (e.g. from the Settings app).
unsafe fn refresh(hwnd: HWND) {
    let on = snap_enabled();
    if on != LAST_STATE.swap(on, Ordering::Relaxed) {
        tray(hwnd, NIM_MODIFY, on);
    }
}

// ------------------------------------------------------------- context menu
unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let on = snap_enabled();

    let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };
    let t_toggle = wide(if on { "Snap windows: On" } else { "Snap windows: Off" });
    let t_settings = wide("Open Multitasking settings");
    let t_auto = wide("Start with Windows");
    let t_exit = wide("Exit");

    AppendMenuW(menu, MF_STRING | check(on), ID_TOGGLE, t_toggle.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_SETTINGS, t_settings.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING | check(autostart_enabled()), ID_AUTOSTART, t_auto.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, t_exit.as_ptr());

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // required so the menu closes when clicking elsewhere
    TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, ptr::null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

// -------------------------------------------------------------- window proc
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lp as u32) & 0xFFFF {
                WM_LBUTTONUP => toggle(hwnd),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match wp & 0xFFFF {
                ID_TOGGLE => toggle(hwnd),
                ID_SETTINGS => {
                    let verb = wide("open");
                    let uri = wide("ms-settings:multitasking");
                    ShellExecuteW(ptr::null_mut(), verb.as_ptr(), uri.as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL);
                }
                ID_AUTOSTART => {
                    if autostart_enabled() {
                        disable_autostart();
                    } else {
                        enable_autostart();
                    }
                }
                ID_EXIT => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_SETTINGCHANGE => {
            refresh(hwnd);
            0
        }
        WM_TIMER if wp == TIMER_SYNC => {
            refresh(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, TIMER_SYNC);
            tray_remove(hwnd);
            PostQuitMessage(0);
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted – put the icon back.
            tray(hwnd, NIM_ADD, LAST_STATE.load(Ordering::Relaxed));
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// -------------------------------------------------------------------- main
fn main() {
    unsafe {
        // Single instance
        let mutex_name = wide("Local\\SnapToggle_SingleInstance");
        let _mutex = CreateMutexW(ptr::null(), 0, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        ensure_autostart_default();

        let hinst = GetModuleHandleW(ptr::null());
        let class_name = wide("SnapToggleHiddenWindow");

        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&wc);

        let tb = wide("TaskbarCreated");
        TASKBAR_CREATED.store(RegisterWindowMessageW(tb.as_ptr()), Ordering::Relaxed);

        // Hidden top-level window (needed to receive the TaskbarCreated broadcast).
        let title = wide(APP_NAME);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        let on = snap_enabled();
        LAST_STATE.store(on, Ordering::Relaxed);
        tray(hwnd, NIM_ADD, on);
        SetTimer(hwnd, TIMER_SYNC, 2000, None);

        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}