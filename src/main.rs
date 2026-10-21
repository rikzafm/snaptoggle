//! SnapToggle – one-click tray toggle for Windows 11 "Snap windows"
//! (Settings > System > Multitasking > Snap windows).
//!
//! * Lives in the notification area (no taskbar button, no window)
//! * Left-click the tray icon  -> toggle Snap windows on/off
//! * Right-click               -> menu (toggle, keybind selector, open settings, start with Windows, exit)
//! * Global hotkey             -> customizable combination keybind (e.g. Win+Alt+S, Ctrl+Shift+S)
//! * Registers itself to auto-start with Windows (can be disabled from the menu)
//! * Icon switches colour/grey so you can see the state at a glance

#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{mem, ptr};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
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
const ID_HOTKEY_CONFIG: usize = 1002;
const ID_SETTINGS: usize = 1003;
const ID_AUTOSTART: usize = 1004;
const ID_EXIT: usize = 1099;

const HOTKEY_SNAP_ID: i32 = 2001;

// Dialog control IDs
const ID_BTN_SAVE: usize = 3001;
const ID_BTN_CLEAR: usize = 3002;
const ID_BTN_CANCEL: usize = 3003;

const SS_LEFT: u32 = 0x00000000;
const SS_CENTER: u32 = 0x00000001;

const ICON_ON: usize = 1; // resource IDs set in build.rs
const ICON_OFF: usize = 2;

const APP_NAME: &str = "SnapToggle";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const DESKTOP_KEY: &str = r"Control Panel\Desktop";
const APP_KEY: &str = r"Software\SnapToggle";
const HOTKEY_KEY: &str = "Hotkey";

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
    let ok = spi_set(on);
    reg_set_sz(DESKTOP_KEY, "WindowArrangementActive", if on { "1" } else { "0" });

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

unsafe fn ensure_autostart_default() {
    if reg_get_sz(APP_KEY, "AutoStartOptOut").as_deref() != Some("1") {
        enable_autostart();
    }
}

// ------------------------------------------------------------- hotkey logic
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyConfig {
    pub modifiers: u32, // MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN
    pub vk: u32,        // Virtual key code
}

fn vk_to_string(vk: u32) -> String {
    match vk {
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x1B => "Esc".to_string(),
        0x20 => "Space".to_string(),
        0x21 => "PageUp".to_string(),
        0x22 => "PageDown".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x2C => "PrintScreen".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x30..=0x39 => format!("{}", (vk as u8) as char),
        0x41..=0x5A => format!("{}", (vk as u8) as char),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".to_string(),
        0x6B => "Num +".to_string(),
        0x6D => "Num -".to_string(),
        0x6E => "Num .".to_string(),
        0x6F => "Num /".to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0xBA => ";".to_string(),
        0xBB => "=".to_string(),
        0xBC => ",".to_string(),
        0xBD => "-".to_string(),
        0xBE => ".".to_string(),
        0xBF => "/".to_string(),
        0xC0 => "`".to_string(),
        0xDB => "[".to_string(),
        0xDC => "\\".to_string(),
        0xDD => "]".to_string(),
        0xDE => "'".to_string(),
        _ => {
            unsafe {
                let scancode = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
                if scancode != 0 {
                    let mut name = [0u16; 64];
                    let len = GetKeyNameTextW((scancode << 16) as i32, name.as_mut_ptr(), name.len() as i32);
                    if len > 0 {
                        return String::from_utf16_lossy(&name[..len as usize]);
                    }
                }
            }
            format!("0x{:02X}", vk)
        }
    }
}

impl HotkeyConfig {
    pub fn display(&self) -> String {
        if self.vk == 0 {
            return "None".to_string();
        }
        let mut parts = Vec::new();
        if (self.modifiers & MOD_WIN) != 0 {
            parts.push("Win");
        }
        if (self.modifiers & MOD_CONTROL) != 0 {
            parts.push("Ctrl");
        }
        if (self.modifiers & MOD_ALT) != 0 {
            parts.push("Alt");
        }
        if (self.modifiers & MOD_SHIFT) != 0 {
            parts.push("Shift");
        }
        let key_str = vk_to_string(self.vk);
        parts.push(&key_str);
        parts.join(" + ")
    }

    pub fn to_serialized(&self) -> String {
        if self.vk == 0 {
            String::new()
        } else {
            format!("{:X}:{:X}", self.modifiers, self.vk)
        }
    }

    pub fn from_serialized(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut parts = trimmed.split(':');
        let m = u32::from_str_radix(parts.next()?, 16).ok()?;
        let vk = u32::from_str_radix(parts.next()?, 16).ok()?;
        Some(HotkeyConfig { modifiers: m, vk })
    }
}

unsafe fn load_hotkey() -> Option<HotkeyConfig> {
    reg_get_sz(APP_KEY, HOTKEY_KEY).and_then(|s| HotkeyConfig::from_serialized(&s))
}

unsafe fn save_hotkey(cfg: Option<HotkeyConfig>) {
    match cfg {
        Some(h) if h.vk != 0 => {
            reg_set_sz(APP_KEY, HOTKEY_KEY, &h.to_serialized());
        }
        _ => {
            reg_delete_value(APP_KEY, HOTKEY_KEY);
        }
    }
}

unsafe fn update_registered_hotkey(hwnd: HWND, cfg: Option<HotkeyConfig>) -> bool {
    UnregisterHotKey(hwnd, HOTKEY_SNAP_ID);
    if let Some(h) = cfg {
        if h.vk != 0 {
            let flags = h.modifiers | MOD_NOREPEAT;
            return RegisterHotKey(hwnd, HOTKEY_SNAP_ID, flags, h.vk) != 0;
        }
    }
    true
}

// -------------------------------------------------------- Hotkey Picker Dialog
struct DialogState {
    current: Option<HotkeyConfig>,
    temp: Option<HotkeyConfig>,
    saved: bool,
    h_prompt: HWND,
    h_display: HWND,
    h_save: HWND,
    h_clear: HWND,
    h_cancel: HWND,
    font: HFONT,
    bold_font: HFONT,
}

unsafe fn update_dialog_labels(state: &DialogState) {
    let text = match state.temp {
        Some(h) if h.vk != 0 => h.display(),
        _ => "None".to_string(),
    };
    let wtext = wide(&text);
    SetWindowTextW(state.h_display, wtext.as_ptr());

    let can_save = state.temp != state.current;
    EnableWindow(state.h_save, if can_save { 1 } else { 0 });
    EnableWindow(state.h_clear, if state.temp.is_some() { 1 } else { 0 });
}

unsafe extern "system" fn dialog_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DialogState;

    match msg {
        WM_CREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            let state = cs.lpCreateParams as *mut DialogState;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            let hinst = GetModuleHandleW(ptr::null());

            // Create fonts
            let mut ncm: NONCLIENTMETRICSW = mem::zeroed();
            ncm.cbSize = mem::size_of::<NONCLIENTMETRICSW>() as u32;
            SystemParametersInfoW(
                SPI_GETNONCLIENTMETRICS,
                mem::size_of::<NONCLIENTMETRICSW>() as u32,
                &mut ncm as *mut _ as *mut c_void,
                0,
            );
            let font = CreateFontIndirectW(&ncm.lfMessageFont);
            let mut bold_lf = ncm.lfMessageFont;
            bold_lf.lfWeight = 700;
            bold_lf.lfHeight = (bold_lf.lfHeight * 5) / 4; // slightly larger for the keybind display
            let bold_font = CreateFontIndirectW(&bold_lf);

            let s = &mut *state;
            s.font = font;
            s.bold_font = bold_font;

            let prompt_text = wide("Press any key combination (e.g. Win+Alt+S, Ctrl+Shift+S, F10):");
            let static_class = wide("STATIC");
            s.h_prompt = CreateWindowExW(
                0,
                static_class.as_ptr(),
                prompt_text.as_ptr(),
                WS_CHILD | WS_VISIBLE | (SS_LEFT as u32),
                24,
                20,
                380,
                30,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null(),
            );
            SendMessageW(s.h_prompt, WM_SETFONT, font as usize, 1);

            let edit_class = wide("STATIC");
            s.h_display = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                edit_class.as_ptr(),
                ptr::null(),
                WS_CHILD | WS_VISIBLE | (SS_CENTER as u32),
                24,
                55,
                380,
                36,
                hwnd,
                ptr::null_mut(),
                hinst,
                ptr::null(),
            );
            SendMessageW(s.h_display, WM_SETFONT, bold_font as usize, 1);

            let btn_class = wide("BUTTON");
            let t_save = wide("Save Keybind");
            s.h_save = CreateWindowExW(
                0,
                btn_class.as_ptr(),
                t_save.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_DEFPUSHBUTTON as u32),
                24,
                110,
                115,
                32,
                hwnd,
                ID_BTN_SAVE as HMENU,
                hinst,
                ptr::null(),
            );
            SendMessageW(s.h_save, WM_SETFONT, font as usize, 1);

            let t_clear = wide("Clear");
            s.h_clear = CreateWindowExW(
                0,
                btn_class.as_ptr(),
                t_clear.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_PUSHBUTTON as u32),
                150,
                110,
                90,
                32,
                hwnd,
                ID_BTN_CLEAR as HMENU,
                hinst,
                ptr::null(),
            );
            SendMessageW(s.h_clear, WM_SETFONT, font as usize, 1);

            let t_cancel = wide("Cancel");
            s.h_cancel = CreateWindowExW(
                0,
                btn_class.as_ptr(),
                t_cancel.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_PUSHBUTTON as u32),
                250,
                110,
                90,
                32,
                hwnd,
                ID_BTN_CANCEL as HMENU,
                hinst,
                ptr::null(),
            );
            SendMessageW(s.h_cancel, WM_SETFONT, font as usize, 1);

            update_dialog_labels(s);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if state_ptr.is_null() {
                return 0;
            }
            let s = &mut *state_ptr;
            let vk = (wp & 0xFFFF) as u32;

            // Esc cancels if pressed alone
            if vk == VK_ESCAPE as u32 {
                let shift = (GetKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0;
                let ctrl = (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
                let alt = (GetKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
                if !shift && !ctrl && !alt {
                    DestroyWindow(hwnd);
                    return 0;
                }
            }

            // Ignore pure modifier presses until the actual combination key arrives
            if vk == VK_SHIFT as u32
                || vk == VK_LSHIFT as u32
                || vk == VK_RSHIFT as u32
                || vk == VK_CONTROL as u32
                || vk == VK_LCONTROL as u32
                || vk == VK_RCONTROL as u32
                || vk == VK_MENU as u32
                || vk == VK_LMENU as u32
                || vk == VK_RMENU as u32
                || vk == VK_LWIN as u32
                || vk == VK_RWIN as u32
            {
                return 0;
            }

            let mut modifiers = 0;
            if (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0 {
                modifiers |= MOD_CONTROL;
            }
            if (GetKeyState(VK_MENU as i32) as u16 & 0x8000) != 0 {
                modifiers |= MOD_ALT;
            }
            if (GetKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0 {
                modifiers |= MOD_SHIFT;
            }
            if ((GetKeyState(VK_LWIN as i32) as u16 & 0x8000) != 0)
                || ((GetKeyState(VK_RWIN as i32) as u16 & 0x8000) != 0)
            {
                modifiers |= MOD_WIN;
            }

            s.temp = Some(HotkeyConfig { modifiers, vk });
            update_dialog_labels(s);
            0
        }
        WM_COMMAND => {
            if state_ptr.is_null() {
                return 0;
            }
            let s = &mut *state_ptr;
            match (wp & 0xFFFF) as usize {
                ID_BTN_SAVE => {
                    s.saved = true;
                    DestroyWindow(hwnd);
                }
                ID_BTN_CLEAR => {
                    s.temp = None;
                    update_dialog_labels(s);
                }
                ID_BTN_CANCEL => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if !state_ptr.is_null() {
                let s = &*state_ptr;
                if !s.font.is_null() {
                    DeleteObject(s.font);
                }
                if !s.bold_font.is_null() {
                    DeleteObject(s.bold_font);
                }
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn prompt_hotkey_dialog(parent_hwnd: HWND, current: Option<HotkeyConfig>) -> (bool, Option<HotkeyConfig>) {
    let hinst = GetModuleHandleW(ptr::null());
    let class_name = wide("SnapToggleHotkeyDialog");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(dialog_wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: load_icon(snap_enabled()),
        hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    RegisterClassW(&wc);

    let mut state = DialogState {
        current,
        temp: current,
        saved: false,
        h_prompt: ptr::null_mut(),
        h_display: ptr::null_mut(),
        h_save: ptr::null_mut(),
        h_clear: ptr::null_mut(),
        h_cancel: ptr::null_mut(),
        font: ptr::null_mut(),
        bold_font: ptr::null_mut(),
    };

    let title = wide("Configure SnapToggle Keybind");
    let dlg_w = 445;
    let dlg_h = 195;

    let mut parent_rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetWindowRect(parent_hwnd, &mut parent_rc);

    let screen_w = GetSystemMetrics(SM_CXSCREEN);
    let screen_h = GetSystemMetrics(SM_CYSCREEN);
    let x = (screen_w - dlg_w) / 2;
    let y = (screen_h - dlg_h) / 2;

    let hwnd = CreateWindowExW(
        WS_EX_DLGMODALFRAME | WS_EX_TOPMOST,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
        x,
        y,
        dlg_w,
        dlg_h,
        parent_hwnd,
        ptr::null_mut(),
        hinst,
        &mut state as *mut DialogState as *const c_void,
    );

    if hwnd.is_null() {
        return (false, current);
    }

    SetForegroundWindow(hwnd);
    SetFocus(hwnd);

    let mut msg: MSG = mem::zeroed();
    while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    UnregisterClassW(class_name.as_ptr(), hinst);

    if state.saved {
        (true, state.temp)
    } else {
        (false, current)
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

    let hotkey_str = match load_hotkey() {
        Some(h) if h.vk != 0 => format!(" [{}]", h.display()),
        _ => String::new(),
    };

    let tip = format!("SnapToggle{}", hotkey_str);
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
    let t_toggle = wide(if on { "Windows Snap: On" } else { "Windows Snap: Off" });

    let current_hotkey = load_hotkey();
    let hotkey_label = match current_hotkey {
        Some(h) if h.vk != 0 => format!("Keybind ({})", h.display()),
        _ => "Set Keybind...".to_string(),
    };
    let t_hotkey = wide(&hotkey_label);
    let t_settings = wide("Setting");
    let t_auto = wide("Start Up");
    let t_exit = wide("Exit");

    AppendMenuW(menu, MF_STRING | check(on), ID_TOGGLE, t_toggle.as_ptr());
    AppendMenuW(menu, MF_STRING, ID_HOTKEY_CONFIG, t_hotkey.as_ptr());
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
        WM_HOTKEY => {
            if wp as i32 == HOTKEY_SNAP_ID {
                toggle(hwnd);
            }
            0
        }
        WM_COMMAND => {
            match wp & 0xFFFF {
                ID_TOGGLE => toggle(hwnd),
                ID_HOTKEY_CONFIG => {
                    let cur = load_hotkey();
                    let (saved, new_hotkey) = prompt_hotkey_dialog(hwnd, cur);
                    if saved {
                        save_hotkey(new_hotkey);
                        let ok = update_registered_hotkey(hwnd, new_hotkey);
                        tray(hwnd, NIM_MODIFY, snap_enabled());
                        if !ok && new_hotkey.is_some() {
                            let text = wide("Could not register this hotkey combination.\nIt might be in use by another application or reserved by Windows.");
                            let title = wide("SnapToggle Keybind");
                            MessageBoxW(hwnd, text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING);
                        }
                    }
                }
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
            UnregisterHotKey(hwnd, HOTKEY_SNAP_ID);
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

        // Hidden top-level window (needed to receive the TaskbarCreated broadcast & hotkey messages).
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

        // Register configured hotkey if present
        let hotkey = load_hotkey();
        update_registered_hotkey(hwnd, hotkey);

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