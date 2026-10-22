//! Win32 wiring: low-level hooks, the worker that injects, IME access, hook liveness, selftest.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::classifier::Mode;
use crate::gate::{Decision, Gate, Input, Item, Key, MouseButton};
use crate::tracker::{Env, Options, Out, Plan, Tracker};

/// High 32 bits of `dwExtraInfo` on everything we inject ("HFX"); low 32 bits carry the gate epoch.
const TAG_SIG: u64 = 0x0048_4658;
const WM_IME_CONTROL: u32 = 0x0283;
const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_SETCONVERSIONMODE: usize = 0x0002;
const IMC_GETOPENSTATUS: usize = 0x0005;
const IMC_SETOPENSTATUS: usize = 0x0006;
const IME_CMODE_NATIVE: usize = 0x0001;
const WM_APP_REINSTALL: u32 = WM_APP + 1;
const HOTKEY_PAUSE: i32 = 1;

static GATE: LazyLock<Mutex<Gate>> = LazyLock::new(|| Mutex::new(Gate::new()));
static WAKE: LazyLock<(Mutex<bool>, Condvar)> = LazyLock::new(|| (Mutex::new(false), Condvar::new()));
static HOOK_TID: AtomicU32 = AtomicU32::new(0);
static KB_HOOK: AtomicUsize = AtomicUsize::new(0);
static MOUSE_HOOK: AtomicUsize = AtomicUsize::new(0);
static LAST_HOOK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_RAW_MS: AtomicU64 = AtomicU64::new(0);
static LAST_REINSTALL_MS: AtomicU64 = AtomicU64::new(0);
static RAW_KEYS: AtomicU64 = AtomicU64::new(0);
static HOOK_KEYS: AtomicU64 = AtomicU64::new(0);
static CAPS: AtomicBool = AtomicBool::new(false);
static CAPS_DOWN: AtomicBool = AtomicBool::new(false);
/// Last time the worker loop ran; the hook fails open if it goes stale while input is held.
static WORKER_BEAT_MS: AtomicU64 = AtomicU64::new(0);
static FAILED_OPEN: AtomicBool = AtomicBool::new(false);
/// Pause after each injected key-up, chosen per foreground app.
static FG_EXE: Mutex<String> = Mutex::new(String::new());
static KEY_GAP_MS: AtomicU64 = AtomicU64::new(3);
/// Apps that read input in chunks (terminals, console TUIs): longer gaps between injected keys.
const CHUNKED_INPUT_APPS: [&str; 14] = [
    "windowsterminal.exe", "openconsole.exe", "conhost.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "wsl.exe",
    "mintty.exe", "alacritty.exe", "wezterm-gui.exe", "code.exe", "cursor.exe", "tabby.exe", "hyper.exe",
];
const WORKER_STALE_MS: u64 = 1500;
static PAUSED: AtomicBool = AtomicBool::new(false);
static FG_OK: AtomicBool = AtomicBool::new(true);
pub static TO_ENGLISH: AtomicBool = AtomicBool::new(false);
pub static DEBUG: AtomicBool = AtomicBool::new(false);
static HOOK_NS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HOOK_NS_MAX: AtomicU64 = AtomicU64::new(0);
static HOOK_CALLS: AtomicU64 = AtomicU64::new(0);

fn gate() -> MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> u64 {
    unsafe { GetTickCount64() }
}

fn wake() {
    let (m, c) = &*WAKE;
    *m.lock().unwrap_or_else(|e| e.into_inner()) = true;
    c.notify_one();
}

fn tag(epoch: u32) -> usize {
    ((TAG_SIG << 32) | epoch as u64) as usize
}

fn untag(extra: usize) -> Option<u32> {
    let e = extra as u64;
    (e >> 32 == TAG_SIG).then_some(e as u32)
}

pub fn data_dir() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(Into::into).unwrap_or_else(std::env::temp_dir);
    let dir: std::path::PathBuf = [base, "hanfix".into()].iter().collect();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn log(msg: &str) {
    let line = format!("[{:>10}] {msg}\n", now());
    eprint!("{line}");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(data_dir().join("hanfix.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ---- Hooks (hook thread) -----------------------------------------------------------------------

fn record_hook_time(t0: Instant) {
    let ns = t0.elapsed().as_nanos() as u64;
    HOOK_NS_TOTAL.fetch_add(ns, Relaxed);
    HOOK_CALLS.fetch_add(1, Relaxed);
    HOOK_NS_MAX.fetch_max(ns, Relaxed);
}

unsafe extern "system" fn keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(null_mut(), code, wp, lp) };
    }
    let t0 = Instant::now();
    let kb = unsafe { &*(lp as *const KBDLLHOOKSTRUCT) };
    let t = now();
    LAST_HOOK_MS.store(t, Relaxed);
    HOOK_KEYS.fetch_add(1, Relaxed);
    let injected = kb.flags & LLKHF_INJECTED != 0;
    let decision = match untag(kb.dwExtraInfo).filter(|_| injected) {
        Some(epoch) => {
            gate().on_echo(epoch, t);
            Decision::Pass
        }
        None => {
            let up = kb.flags & LLKHF_UP != 0;
            // Caps Lock toggles on the first down only (auto-repeat sends more downs).
            if kb.vkCode == VK_CAPITAL as u32 {
                let was_down = CAPS_DOWN.swap(!up, Relaxed);
                if !up && !was_down {
                    CAPS.fetch_xor(true, Relaxed);
                }
            }
            let key = Key { vk: kb.vkCode as u16, scan: kb.scanCode as u16, extended: kb.flags & LLKHF_EXTENDED != 0, up };
            let d = {
                let mut g = gate();
                fail_open_if_worker_stale(&mut g, t);
                g.on_key(key, t)
            };
            wake();
            d
        }
    };
    record_hook_time(t0);
    match decision {
        Decision::Swallow => 1,
        Decision::Pass => unsafe { CallNextHookEx(null_mut(), code, wp, lp) },
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(null_mut(), code, wp, lp) };
    }
    let ms = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
    let flags = match wp as u32 {
        WM_LBUTTONDOWN => MOUSEEVENTF_LEFTDOWN,
        WM_LBUTTONUP => MOUSEEVENTF_LEFTUP,
        WM_RBUTTONDOWN => MOUSEEVENTF_RIGHTDOWN,
        WM_RBUTTONUP => MOUSEEVENTF_RIGHTUP,
        WM_MBUTTONDOWN => MOUSEEVENTF_MIDDLEDOWN,
        WM_MBUTTONUP => MOUSEEVENTF_MIDDLEUP,
        WM_XBUTTONDOWN => MOUSEEVENTF_XDOWN,
        WM_XBUTTONUP => MOUSEEVENTF_XUP,
        _ => return unsafe { CallNextHookEx(null_mut(), code, wp, lp) }, // moves and wheel always pass
    };
    let t = now();
    LAST_HOOK_MS.store(t, Relaxed);
    let injected = ms.flags & LLMHF_INJECTED != 0;
    let decision = match untag(ms.dwExtraInfo).filter(|_| injected) {
        Some(epoch) => {
            gate().on_echo(epoch, t);
            Decision::Pass
        }
        None => {
            let m = MouseButton { flags, data: ms.mouseData >> 16, x: ms.pt.x, y: ms.pt.y };
            let d = {
                let mut g = gate();
                fail_open_if_worker_stale(&mut g, t);
                g.on_mouse_button(m, t)
            };
            wake();
            d
        }
    };
    match decision {
        Decision::Swallow => 1,
        Decision::Pass => unsafe { CallNextHookEx(null_mut(), code, wp, lp) },
    }
}

/// R9 "fail quietly": a dead or stuck worker must never leave the keyboard swallowed.
fn fail_open_if_worker_stale(g: &mut Gate, now_ms: u64) {
    if now_ms > WORKER_BEAT_MS.load(Relaxed) + WORKER_STALE_MS && g.fail_open() {
        FAILED_OPEN.store(true, Relaxed); // logged by the monitor thread; no IO in the hook
    }
}

fn install_hooks() {
    unsafe {
        let module = GetModuleHandleW(null_mut());
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        if kb.is_null() || mouse.is_null() {
            log(&format!("hook install failed (error {})", GetLastError()));
        }
        LAST_HOOK_MS.store(now(), Relaxed);
        LAST_RAW_MS.store(0, Relaxed);
        KB_HOOK.store(kb as usize, Relaxed);
        MOUSE_HOOK.store(mouse as usize, Relaxed);
    }
}

fn remove_hooks() {
    unsafe {
        UnhookWindowsHookEx(KB_HOOK.swap(0, Relaxed) as HHOOK);
        UnhookWindowsHookEx(MOUSE_HOOK.swap(0, Relaxed) as HHOOK);
    }
}

fn set_enabled() {
    gate().enabled = FG_OK.load(Relaxed) && !PAUSED.load(Relaxed);
}

fn hook_thread() {
    unsafe {
        HOOK_TID.store(GetCurrentThreadId(), Relaxed);
        CAPS.store(GetKeyState(VK_CAPITAL as i32) & 1 != 0, Relaxed);
        install_hooks();
        if RegisterHotKey(null_mut(), HOTKEY_PAUSE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_F12 as u32) == 0 {
            log("Ctrl+Alt+F12 is taken by another program; pause hotkey unavailable");
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_HOTKEY if msg.wParam as i32 == HOTKEY_PAUSE => {
                    let paused = !PAUSED.fetch_xor(true, Relaxed);
                    set_enabled();
                    log(if paused { "paused (Ctrl+Alt+F12 to resume)" } else { "resumed" });
                }
                WM_APP_REINSTALL => {
                    remove_hooks();
                    install_hooks();
                    log("hook was silent while keys were typed: reinstalled");
                }
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        remove_hooks();
    }
}

// ---- Hook liveness (Raw Input, no injection) ------------------------------------------------------

unsafe extern "system" fn monitor_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            LAST_RAW_MS.store(now(), Relaxed);
            RAW_KEYS.fetch_add(1, Relaxed);
        }
        WM_TIMER => {
            if FAILED_OPEN.swap(false, Relaxed) {
                log("worker stopped responding while input was held: keyboard released (fail-open), held keys dropped");
            }
            if !FG_OK.load(Relaxed) {
                // Elevated window: the hook may legitimately not see its keys.
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            let raw = LAST_RAW_MS.load(Relaxed);
            let hook = LAST_HOOK_MS.load(Relaxed);
            let t = now();
            // At most one reinstall per 10 s, tracked separately (hook calls overwrite LAST_HOOK_MS).
            if raw > hook + 1000 && t > raw + 200 && t > LAST_REINSTALL_MS.load(Relaxed) + 10_000 {
                LAST_REINSTALL_MS.store(t, Relaxed);
                log(&format!(
                    "liveness: last raw key {} ms ago, last hook call {} ms ago (raw keys {}, hook keys {})",
                    t - raw,
                    t.saturating_sub(hook),
                    RAW_KEYS.load(Relaxed),
                    HOOK_KEYS.load(Relaxed)
                ));
                unsafe { PostThreadMessageW(HOOK_TID.load(Relaxed), WM_APP_REINSTALL, 0, 0) };
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn monitor_thread() {
    unsafe {
        let class = wide("hanfix.monitor");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(monitor_wndproc),
            hInstance: GetModuleHandleW(null_mut()),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), wc.hInstance, null_mut());
        let rid = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        if RegisterRawInputDevices(&rid, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            log("raw input unavailable: hook liveness check disabled");
            return;
        }
        SetTimer(hwnd, 1, 500, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

// ---- System access for the tracker -------------------------------------------------------------

struct Focus {
    thread: u32,
    control: HWND,
}

fn focus() -> Option<Focus> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let thread = GetWindowThreadProcessId(window, null_mut());
        let mut gti: GUITHREADINFO = std::mem::zeroed();
        gti.cbSize = size_of::<GUITHREADINFO>() as u32;
        let control = if GetGUIThreadInfo(thread, &mut gti) != 0 && !gti.hwndFocus.is_null() { gti.hwndFocus } else { window };
        Some(Focus { thread, control })
    }
}

fn ime_query(ime: HWND, cmd: usize) -> Option<usize> {
    let mut out = 0usize;
    let ok = unsafe { SendMessageTimeoutW(ime, WM_IME_CONTROL, cmd, 0, SMTO_ABORTIFHUNG, 30, &mut out) };
    (ok != 0).then_some(out)
}

fn ime_window() -> Option<HWND> {
    let f = focus()?;
    let hkl = unsafe { GetKeyboardLayout(f.thread) } as usize;
    if hkl & 0xFFFF != 0x0412 {
        return None; // not a Korean layout
    }
    let ime = unsafe { ImmGetDefaultIMEWnd(f.control) };
    (!ime.is_null()).then_some(ime)
}

const IME_CMODE_FULLSHAPE: usize = 0x0008;

/// `--ime-status`: raw IME state of the foreground window; `fix` also clears full-width (전자) mode.
pub fn ime_status(fix: bool) {
    let Some(ime) = ime_window() else {
        println!("no Korean IME window for the foreground window");
        return;
    };
    let open = ime_query(ime, IMC_GETOPENSTATUS);
    let conv = ime_query(ime, IMC_GETCONVERSIONMODE);
    println!("open={open:?} conversion={conv:#x?} (NATIVE=0x1 Hangul, FULLSHAPE=0x8 전자)");
    if let (true, Some(c)) = (fix, conv) {
        if c & IME_CMODE_FULLSHAPE != 0 {
            let mut out = 0usize;
            unsafe {
                SendMessageTimeoutW(ime, WM_IME_CONTROL, IMC_SETCONVERSIONMODE, (c & !IME_CMODE_FULLSHAPE) as isize, SMTO_ABORTIFHUNG, 30, &mut out);
            }
            println!("full-width cleared -> conversion={:#x?}", ime_query(ime, IMC_GETCONVERSIONMODE));
        }
    }
}

pub fn ime_mode() -> Option<Mode> {
    let ime = ime_window()?;
    if ime_query(ime, IMC_GETOPENSTATUS)? == 0 {
        return Some(Mode::English);
    }
    let conv = ime_query(ime, IMC_GETCONVERSIONMODE)?;
    Some(if conv & IME_CMODE_NATIVE != 0 { Mode::Hangul } else { Mode::English })
}

pub fn set_ime_mode(mode: Mode) -> bool {
    let Some(ime) = ime_window() else { return false };
    let mut out = 0usize;
    unsafe {
        SendMessageTimeoutW(ime, WM_IME_CONTROL, IMC_SETOPENSTATUS, 1, SMTO_ABORTIFHUNG, 30, &mut out);
        // Only the NATIVE (Hangul) bit changes; full-shape and other bits stay as they were.
        let current = ime_query(ime, IMC_GETCONVERSIONMODE).unwrap_or(0);
        let value = if mode == Mode::Hangul { current | IME_CMODE_NATIVE } else { current & !IME_CMODE_NATIVE };
        SendMessageTimeoutW(ime, WM_IME_CONTROL, IMC_SETCONVERSIONMODE, value as isize, SMTO_ABORTIFHUNG, 30, &mut out) != 0
    }
}

/// UI Automation: is the focused element a password field? `None` if UIA could not tell.
/// How long the worker waits for UI Automation before treating the field as a password field.
const UIA_WAIT_MS: u64 = 150;

/// UI Automation runs on its own thread: a slow provider (seen: Windows 11 Notepad, >1.5 s) must
/// never stall the worker while input is held — that ended in fail-open and dropped keys.
static UIA_REQUESTS: LazyLock<Mutex<std::sync::mpsc::Sender<std::sync::mpsc::Sender<Option<bool>>>>> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<std::sync::mpsc::Sender<Option<bool>>>();
    std::thread::spawn(move || {
        use windows::core::Interface;
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
        use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomation2};
        let uia: Option<IUIAutomation> = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
        };
        if let Some(uia2) = uia.as_ref().and_then(|u| u.cast::<IUIAutomation2>().ok()) {
            unsafe {
                let _ = uia2.SetConnectionTimeout(1000);
                let _ = uia2.SetTransactionTimeout(1000);
            }
        }
        for reply in rx {
            let answer = uia.as_ref().and_then(|uia| unsafe {
                let element = uia.GetFocusedElement().ok()?;
                element.CurrentIsPassword().ok().map(|b| b.as_bool())
            });
            let _ = reply.send(answer);
        }
    });
    Mutex::new(tx)
});

/// UI Automation: is the focused element a password field? `None` if UIA could not tell in time.
fn uia_focused_is_password() -> Option<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    UIA_REQUESTS.lock().unwrap_or_else(|e| e.into_inner()).send(tx).ok()?;
    match rx.recv_timeout(Duration::from_millis(UIA_WAIT_MS)) {
        Ok(answer) => answer,
        Err(_) => {
            log("password check (UI Automation) too slow: word left as typed");
            None
        }
    }
}

struct WinEnv;

impl Env for WinEnv {
    fn ime_mode(&mut self) -> Option<Mode> {
        ime_mode()
    }

    fn focus(&mut self) -> u64 {
        focus().map_or(0, |f| f.control as u64)
    }

    fn is_password(&mut self) -> bool {
        let Some(f) = focus() else { return false };
        let win32_password = unsafe {
            let mut class = [0u16; 64];
            let n = GetClassNameW(f.control, class.as_mut_ptr(), class.len() as i32) as usize;
            let class = String::from_utf16_lossy(&class[..n]).to_ascii_lowercase();
            class.contains("edit") && GetWindowLongW(f.control, GWL_STYLE) as u32 & ES_PASSWORD as u32 != 0
        };
        // Browser and Electron password fields have no ES_PASSWORD: ask UI Automation. If it
        // cannot answer, assume a password field (never rewrite text we cannot see).
        win32_password || uia_focused_is_password().unwrap_or(true)
    }

    fn caps_lock(&mut self) -> bool {
        CAPS.load(Relaxed)
    }

    fn now_ms(&mut self) -> u64 {
        now()
    }
}

fn process_info(window: HWND) -> Option<(String, bool)> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut name = [0u16; 520];
        let mut len = name.len() as u32;
        if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, name.as_mut_ptr(), &mut len) == 0 {
            len = 0;
        }
        let exe = String::from_utf16_lossy(&name[..len as usize]);
        let exe = exe.rsplit('\\').next().unwrap_or_default().to_ascii_lowercase();
        let elevated = token_elevated(process);
        CloseHandle(process);
        Some((exe, elevated?))
    }
}

fn token_elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut te: TOKEN_ELEVATION = std::mem::zeroed();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, &mut te as *mut _ as *mut _, size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        (ok != 0).then_some(te.TokenIsElevated != 0)
    }
}

/// Should corrections run in this foreground window? Not when SendInput would be blocked by UIPI.
fn foreground_allowed(window: HWND, config: &Config, self_elevated: bool) -> bool {
    match process_info(window) {
        Some((exe, elevated)) => (!elevated || self_elevated) && !config.exclude.iter().any(|x| *x == exe),
        None => self_elevated,
    }
}

// ---- Worker ------------------------------------------------------------------------------------

fn unicode_input(c: char, up: bool) -> INPUT {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_KEYBOARD;
    let flags = KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 };
    // Hangul syllables and ASCII are in the BMP: one UTF-16 unit each.
    input.Anonymous.ki = KEYBDINPUT { wVk: 0, wScan: c as u16, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    input
}

/// Low-level inputs for a run of plan steps (`AwaitMode` steps must already be split out).
fn to_inputs(events: &[Out]) -> Vec<INPUT> {
    events
        .iter()
        .flat_map(|e| -> Vec<INPUT> {
            let mut input: INPUT = unsafe { std::mem::zeroed() };
            match *e {
                Out::Text(c) => return vec![unicode_input(c, false), unicode_input(c, true)],
                Out::AwaitMode(_) | Out::SetMode(_) => return Vec::new(),
                Out::Key(k) => {
                    input.r#type = INPUT_KEYBOARD;
                    let mut scan = if k.scan != 0 { k.scan } else { (unsafe { MapVirtualKeyW(k.vk as u32, MAPVK_VK_TO_VSC) }) as u16 };
                    if scan == 0 && k.vk == VK_HANGUL {
                        scan = 0x72;
                    }
                    let mut flags = 0;
                    if k.up {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    if k.extended {
                        flags |= KEYEVENTF_EXTENDEDKEY;
                    }
                    input.Anonymous.ki = KEYBDINPUT { wVk: k.vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
                }
                Out::Mouse(m) => {
                    // At the original click point (the cursor may have moved while the click was held).
                    // The move part is not echo-counted: `mouse_proc` passes moves before any accounting.
                    let (dx, dy) = normalize_point(m.x, m.y);
                    input.r#type = INPUT_MOUSE;
                    input.Anonymous.mi = MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: m.data,
                        dwFlags: m.flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                        time: 0,
                        dwExtraInfo: 0,
                    };
                }
            }
            vec![input]
        })
        .collect()
}

/// Screen point -> 0..65535 absolute coordinates over the virtual desktop.
fn normalize_point(x: i32, y: i32) -> (i32, i32) {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let scale = |p: i32, origin: i32, size: i32| (((p - origin) as i64 * 65535) / (size - 1) as i64) as i32;
    (scale(x, vx, vw), scale(y, vy, vh))
}

fn set_tag(inputs: &mut [INPUT], epoch: u32) {
    for input in inputs {
        // Writing a union field is safe; the variant was chosen in `to_inputs`.
        if input.r#type == INPUT_KEYBOARD {
            input.Anonymous.ki.dwExtraInfo = tag(epoch);
        } else {
            input.Anonymous.mi.dwExtraInfo = tag(epoch);
        }
    }
}

/// Inject a plan. Later input stays held until the IME is confirmed in the expected mode.
/// `generation` is the gate generation read when the held item was peeked.
/// `unsolicited`: a mid-word correction; the gate is already blocked (`begin_unsolicited`) and no
/// held item is consumed.
fn execute(plan: &Plan, generation: u64, unsolicited: bool) {
    let fallback: Vec<Out> = plan.fallback_text.iter().flat_map(|t| t.chars().map(Out::Text)).collect();
    // Reserve room for the fallback text too; whatever is not sent is returned via `sent`.
    let n: u32 = plan.events.iter().chain(&fallback).map(Out::event_count).sum();
    let started = Instant::now();
    let epoch = {
        let mut g = gate();
        let reserved = if unsolicited { g.reserve(n, now(), generation) } else { g.commit(n, now(), generation) };
        let Some(e) = reserved else {
            if unsolicited {
                g.set_blocked(false);
            }
            drop(g);
            log("queue was reset (fail-open) while deciding; stale decision dropped");
            return;
        };
        if plan.expect_mode.is_some() {
            g.set_blocked(true);
        }
        if plan.live {
            g.live_word = true;
        }
        if plan.undo_until_ms > 0 {
            g.undo_until_ms = plan.undo_until_ms;
        }
        e
    };
    // Send in runs separated by `AwaitMode` steps; everything later stays queued in between.
    // Paced: terminals and some apps read input in chunks and lose Backspaces that arrive in the
    // same chunk as other keys (seen: `dkssud` -> `dk녕`). A short gap after each key-up keeps
    // every key in its own read. Order is unaffected: later input stays held the whole time.
    let gap = Duration::from_millis(KEY_GAP_MS.load(Relaxed));
    let attempted = std::cell::Cell::new(0u32);
    let send_run = |run: &[Out]| -> u32 {
        let mut inputs = to_inputs(run);
        set_tag(&mut inputs, epoch);
        attempted.set(attempted.get() + inputs.len() as u32);
        if inputs.is_empty() {
            return 0;
        }
        if gap.is_zero() {
            // One atomic batch (app_gap_ms=0, opt-in): redraws once, but some apps (Windows 11
            // Notepad, terminals) drop part of a batch — seen: `dkssud` -> `dk녕`.
            return unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        }
        let mut sent = 0;
        for input in &inputs {
            sent += unsafe { SendInput(1, input, size_of::<INPUT>() as i32) };
            let key_up = input.r#type == INPUT_KEYBOARD && unsafe { input.Anonymous.ki.dwFlags } & KEYEVENTF_KEYUP != 0;
            if key_up && !gap.is_zero() {
                WORKER_BEAT_MS.store(now(), Relaxed);
                std::thread::sleep(gap);
            }
        }
        sent
    };
    let mut inserted = 0u32;
    let mut start = 0;
    let unsent_after = |i: usize| -> u32 { plan.events[i..].iter().chain(&fallback).map(Out::event_count).sum() };
    let user_tail = &plan.events[plan.events.len() - plan.user_tail.min(plan.events.len())..];
    let mut finished = false;
    for (i, step) in plan.events.iter().enumerate() {
        match *step {
            Out::AwaitMode(mode) => {
                inserted += send_run(&plan.events[start..i]);
                if !await_mode(mode, unsent_after(i)) {
                    // Composition state unknown: drop the rest of the correction, keep the user's key.
                    inserted += send_run(user_tail);
                    finished = true;
                    break;
                }
                start = i + 1;
            }
            Out::SetMode(mode) => {
                inserted += send_run(&plan.events[start..i]);
                if !ensure_mode(mode, unsent_after(i)) {
                    // Replayed keys would come out as English letters: type the syllable as text.
                    log("IME mode not confirmed: last syllable typed as text instead of replayed");
                    inserted += send_run(&fallback);
                    inserted += send_run(user_tail);
                    finished = true;
                    break;
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if !finished {
        inserted += send_run(&plan.events[start..]);
    }
    gate().sent(epoch, n, inserted, now());
    if inserted < attempted.get() {
        log(&format!("SendInput inserted {inserted}/{} (blocked by UIPI or another program?)", attempted.get()));
    }
    if let Some(mode) = plan.expect_mode {
        if inserted > 0 {
            ensure_mode(mode, 0);
        }
    }
    if plan.expect_mode.is_some() || unsolicited {
        gate().set_blocked(false);
    }
    if let Some(note) = &plan.note {
        let app = FG_EXE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        log(&format!("{note} in {app}, injected {n} events in {:?}", started.elapsed()));
    }
}

/// Wait until everything sent so far has echoed. `unsent` events of the plan are reserved in
/// `in_flight` but not sent yet (they come after an AwaitMode/SetMode step), so they are not waited for.
fn wait_for_echoes(unsent: u32) {
    let deadline = Instant::now() + Duration::from_millis(300);
    while gate().in_flight() > unsent && Instant::now() < deadline {
        WORKER_BEAT_MS.store(now(), Relaxed);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// After an injected 한/영 key: an echo only means the app's queue has the key, not that the app
/// ran it, so poll until the IME reports the mode. Falls back to setting it directly.
/// Returns false if the app never acted on the key: the composition was then not committed either,
/// so the caller must not send Backspaces that assume it was.
fn await_mode(expected: Mode, unsent: u32) -> bool {
    wait_for_echoes(unsent);
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        if ime_mode() == Some(expected) {
            return true;
        }
        WORKER_BEAT_MS.store(now(), Relaxed);
        std::thread::sleep(Duration::from_millis(5));
    }
    let fixed = set_ime_mode(expected);
    log(&format!("app did not act on the injected 한/영 key within 300 ms; correction abandoned, set {expected:?} directly: {fixed}"));
    false
}

/// Leave the IME in `expected` mode for whatever the user types next. Set directly by message
/// (no 한/영 key), then confirm.
fn ensure_mode(expected: Mode, unsent: u32) -> bool {
    wait_for_echoes(unsent);
    if ime_mode() == Some(expected) {
        return true;
    }
    set_ime_mode(expected);
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        if ime_mode() == Some(expected) {
            return true;
        }
        WORKER_BEAT_MS.store(now(), Relaxed);
        std::thread::sleep(Duration::from_millis(5));
    }
    log(&format!("could not confirm the IME switched to {expected:?} (app may not report its mode)"));
    false
}

fn worker_thread(config: Config) {
    std::thread::spawn(uia_focused_is_password); // warm up: the first UI Automation call is slow
    let mut tracker = Tracker::new(Options::default());
    let mut env = WinEnv;
    let self_elevated = unsafe { token_elevated(GetCurrentProcess()) }.unwrap_or(false);
    let mut last_fg: HWND = null_mut();
    let mut last_stats = now();
    loop {
        WORKER_BEAT_MS.store(now(), Relaxed);
        if gate().take_failed() {
            tracker = Tracker::new(Options::default());
        }
        {
            let (m, c) = &*WAKE;
            let mut woken = m.lock().unwrap_or_else(|e| e.into_inner());
            if !*woken {
                woken = c.wait_timeout(woken, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
            }
            *woken = false;
        }
        tracker.options.to_english = TO_ENGLISH.load(Relaxed);
        // Mid-word conversion has no Space to reconsider: in terminals and code editors a wrong
        // one is a wrong command, so there it only happens at Space.
        let fg_chunked = CHUNKED_INPUT_APPS.contains(&FG_EXE.lock().unwrap_or_else(|e| e.into_inner()).as_str());
        tracker.options.live = config.live && !(fg_chunked && !config.live_in_terminals);
        if gate().watchdog(now()) {
            log("watchdog: injected events never echoed back; counter reset (R10 failure, check for other input hooks)");
        }
        let fg = unsafe { GetForegroundWindow() };
        if fg != last_fg {
            last_fg = fg;
            let ok = foreground_allowed(fg, &config, self_elevated);
            FG_OK.store(ok, Relaxed);
            let exe = process_info(fg).map(|p| p.0).unwrap_or_default();
            let gap = if CHUNKED_INPUT_APPS.contains(&exe.as_str()) { config.terminal_gap_ms } else { config.app_gap_ms };
            KEY_GAP_MS.store(gap, Relaxed);
            FG_EXE.lock().unwrap_or_else(|e| e.into_inner()).clone_from(&exe);
            set_enabled();
            gate().reset_word();
        }
        loop {
            WORKER_BEAT_MS.store(now(), Relaxed);
            let (item, generation) = {
                let mut g = gate();
                (g.next(), g.generation())
            };
            match item {
                None => break,
                Some(passed @ Item::Passed(_)) => {
                    let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        tracker.observe(&passed, &mut env);
                        if !tracker.live_candidate() {
                            return None;
                        }
                        // Mid-word conversion: hold everything from now on, take in the keys that
                        // already reached the app, then decide on the word as it is on screen.
                        let generation = gate().begin_unsolicited();
                        loop {
                            let next = {
                                let mut g = gate();
                                match g.next() {
                                    Some(p @ Item::Passed(_)) => Some(p),
                                    _ => None,
                                }
                            };
                            match next {
                                Some(p) => tracker.observe(&p, &mut env),
                                None => break,
                            }
                        }
                        match tracker.live_plan(&mut env) {
                            Some(plan) => Some((plan, generation)),
                            None => {
                                gate().set_blocked(false);
                                None
                            }
                        }
                    }));
                    match observed {
                        Ok(Some((plan, generation))) => {
                            if std::panic::catch_unwind(|| execute(&plan, generation, true)).is_err() {
                                gate().abort_injection();
                                log("internal error while injecting; gate flags reset");
                            }
                        }
                        Ok(None) => {}
                        Err(_) => {
                            gate().set_blocked(false);
                            tracker = Tracker::new(Options::default());
                        }
                    }
                }
                Some(Item::Held(input)) => {
                    WORKER_BEAT_MS.store(now(), Relaxed);
                    // A bug in the decision must not lose the key: on panic, inject it unchanged.
                    let decided = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tracker.held(input, &mut env)));
                    let plan = decided.unwrap_or_else(|_| {
                        log("internal error while deciding; key passed through unchanged");
                        tracker = Tracker::new(Options::default());
                        let out = match input {
                            Input::Key(k) => Out::Key(k),
                            Input::Mouse(m) => Out::Mouse(m),
                        };
                        Plan { events: vec![out], ..Plan::default() }
                    });
                    if DEBUG.load(Relaxed) {
                        log(&format!("held {input:?} -> {} events {:?}", plan.events.len(), plan.note));
                    }
                    if std::panic::catch_unwind(|| execute(&plan, generation, false)).is_err() {
                        gate().abort_injection();
                        log("internal error while injecting; gate flags reset");
                    }
                }
            }
        }
        if now() > last_stats + 60_000 {
            last_stats = now();
            let calls = HOOK_CALLS.swap(0, Relaxed);
            let total = HOOK_NS_TOTAL.swap(0, Relaxed);
            let max = HOOK_NS_MAX.swap(0, Relaxed);
            if calls > 0 {
                log(&format!("hook callback avg {} µs, max {} µs over {calls} key events", total / calls / 1000, max / 1000));
            }
        }
    }
}

/// When started by double-click the console closes on exit; wait for Enter so results stay readable.
pub fn pause_if_own_console() {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut pids = [0u32; 4];
    let attached = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if attached == 1 {
        println!("Enter 를 누르면 닫힙니다 (press Enter to close)");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

// ---- Single instance ---------------------------------------------------------------------------

/// Two hanfix hooks would re-tag each other's keys just like two different converters do.
/// Returns false if another hanfix (normal run or selftest) already holds the instance mutex.
/// The handle is intentionally leaked: the mutex lives as long as the process.
pub fn claim_single_instance() -> bool {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = wide("Local\\hanfix.single-instance");
    unsafe {
        let handle = CreateMutexW(null_mut(), 0, name.as_ptr());
        !(handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS)
    }
}

// ---- Other converters ----------------------------------------------------------------------------

/// Programs that also swallow and re-inject keys. Running two such hooks at once re-tags each
/// other's injected keys, which breaks echo counting and makes both converters fight over words.
const CONFLICTING: [&str; 2] = ["catchtype.exe", "hanyeong.exe"];

pub fn conflicting_programs() -> Vec<String> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    let mut found = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]).to_ascii_lowercase();
            if CONFLICTING.contains(&exe.as_str()) && !found.contains(&exe) {
                found.push(exe);
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    found
}

// ---- Config & entry points ---------------------------------------------------------------------

#[derive(Clone)]
pub struct Config {
    pub to_english: bool,
    /// Convert English-mode Korean while typing (not only at Space).
    pub live: bool,
    /// Also while typing in terminals / code editors (off: there only at Space).
    pub live_in_terminals: bool,
    pub exclude: Vec<String>,
    /// Pause after each injected key-up in ordinary apps / in terminals.
    pub app_gap_ms: u64,
    pub terminal_gap_ms: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config { to_english: false, live: true, live_in_terminals: false, exclude: Vec::new(), app_gap_ms: 3, terminal_gap_ms: 12 }
    }
}

impl Config {
    pub fn path() -> std::path::PathBuf {
        data_dir().join("config.txt")
    }

    pub fn load() -> Config {
        let mut c = Config::default();
        for line in std::fs::read_to_string(Self::path()).unwrap_or_default().lines() {
            match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("to_english", v)) => c.to_english = v == "true",
                Some(("live", v)) => c.live = v != "false",
                Some(("live_in_terminals", v)) => c.live_in_terminals = v == "true",
                Some(("app_gap_ms", v)) => c.app_gap_ms = v.parse().unwrap_or(c.app_gap_ms),
                Some(("terminal_gap_ms", v)) => c.terminal_gap_ms = v.parse().unwrap_or(c.terminal_gap_ms),
                Some(("exclude", v)) => c.exclude = v.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let text = format!(
            "# hanfix settings\n# to_english: set by `hanfix --selftest` (needs the IME to commit composition on 한/영)\nto_english={}\n# live: convert Korean typed in English mode while typing (false = only at Space)\nlive={}\n# live_in_terminals: also while typing in terminals / code editors\nlive_in_terminals={}\n# exclude: exe names where hanfix stays out, comma separated (e.g. game.exe,photoshop.exe)\nexclude={}\n# pause (ms) after each injected key in ordinary apps (0 = whole correction in one batch) / terminals; raise if letters get lost\napp_gap_ms={}\nterminal_gap_ms={}\n",
            self.to_english,
            self.live,
            self.live_in_terminals,
            self.exclude.join(","),
            self.app_gap_ms,
            self.terminal_gap_ms
        );
        let _ = std::fs::write(Self::path(), text);
    }
}

pub fn start(config: Config) {
    TO_ENGLISH.store(config.to_english, Relaxed);
    WORKER_BEAT_MS.store(now(), Relaxed);
    std::thread::spawn(hook_thread);
    std::thread::spawn(monitor_thread);
    std::thread::spawn(move || worker_thread(config));
}

pub fn run(config: Config) {
    log(&format!(
        "hanfix running. Space fixes the word before it. Ctrl+Alt+F12 pauses. English fixes: {}. Log: {}",
        if config.to_english { "on" } else { "off (run --selftest)" },
        data_dir().join("hanfix.log").display()
    ));
    let others = conflicting_programs();
    if !others.is_empty() {
        log(&format!("WARNING: {} is running. Two key-converting hooks fight each other; quit it for hanfix to work reliably.", others.join(", ")));
    }
    start(config);
    loop {
        std::thread::park();
    }
}

// ---- Hook diagnostics -------------------------------------------------------------------------

/// Inject F24 (a key no application uses) `n` times, untagged.
pub fn inject_f24(n: usize) {
    for _ in 0..n {
        for up in [false, true] {
            let mut input: INPUT = unsafe { std::mem::zeroed() };
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki = KEYBDINPUT { wVk: VK_F24, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
            unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Which injected keys reach our own low-level hook: same process vs. another process?
pub fn diag_hook() {
    start(Config::default());
    std::thread::sleep(Duration::from_millis(500));
    let before = HOOK_KEYS.load(Relaxed);
    inject_f24(5);
    std::thread::sleep(Duration::from_millis(500));
    let same = HOOK_KEYS.load(Relaxed) - before;
    let exe = std::env::current_exe().unwrap();
    let before = HOOK_KEYS.load(Relaxed);
    let _ = std::process::Command::new(exe).arg("--diag-inject").status();
    std::thread::sleep(Duration::from_millis(500));
    let other = HOOK_KEYS.load(Relaxed) - before;
    println!("hook calls for 10 injected key events: same process = {same}, other process = {other}");
}

// ---- Selftest ----------------------------------------------------------------------------------

pub mod selftest {
    use super::*;

    static EDIT: AtomicUsize = AtomicUsize::new(0);
    static MAIN: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        if msg == WM_SETFOCUS {
            // Clicking the window anywhere sends typing to the test field.
            unsafe { SetFocus(edit()) };
            return 0;
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    fn edit() -> HWND {
        EDIT.load(Relaxed) as HWND
    }

    fn text() -> String {
        let mut buf = [0u16; 512];
        let mut out = 0usize;
        unsafe {
            SendMessageTimeoutW(edit(), WM_GETTEXT, buf.len(), buf.as_mut_ptr() as LPARAM, SMTO_ABORTIFHUNG, 500, &mut out);
        }
        String::from_utf16_lossy(&buf[..out])
    }

    fn clear() {
        let empty = [0u16];
        let mut out = 0usize;
        unsafe { SendMessageTimeoutW(edit(), WM_SETTEXT, 0, empty.as_ptr() as LPARAM, SMTO_ABORTIFHUNG, 500, &mut out) };
    }

    fn focused() -> bool {
        unsafe { GetForegroundWindow() == MAIN.load(Relaxed) as HWND }
    }

    /// Inject untagged ("as if typed") key events for `s`: lowercase/uppercase letters, ' ' and
    /// '^' for the 한/영 key. Shift is pressed around uppercase letters unless `hold_shift`.
    /// Set as soon as the test window loses focus: no further key-downs go out (they would land in
    /// whatever window the user switched to). Key-ups still go out so nothing stays pressed.
    static LOST_FOCUS: AtomicBool = AtomicBool::new(false);

    fn type_keys(s: &str, gap_ms: u64, hold_shift: bool) {
        let send = |vk: u16, up: bool| unsafe {
            if !focused() {
                LOST_FOCUS.store(true, Relaxed);
            }
            if LOST_FOCUS.load(Relaxed) && !up {
                return;
            }
            let mut input: INPUT = std::mem::zeroed();
            input.r#type = INPUT_KEYBOARD;
            let scan = if vk == VK_HANGUL { 0x72 } else { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u16 };
            input.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
            SendInput(1, &input, size_of::<INPUT>() as i32);
            if gap_ms > 0 {
                std::thread::sleep(Duration::from_millis(gap_ms));
            }
        };
        if hold_shift {
            send(VK_LSHIFT, false);
        }
        for c in s.chars() {
            if c == '|' {
                // The user switching back to English between words (not via a key, so no undo).
                std::thread::sleep(Duration::from_millis(400));
                set_ime_mode(Mode::English);
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            if c == ' ' && hold_shift {
                // Shift+Space is the IME's half/full-width (반자/전자) toggle: let go of Shift for it.
                send(VK_LSHIFT, true);
                send(VK_SPACE, false);
                send(VK_SPACE, true);
                send(VK_LSHIFT, false);
                continue;
            }
            let (vk, shift) = match c {
                ' ' => (VK_SPACE, false),
                '~' => (VK_ESCAPE, false),
                '^' => (VK_HANGUL, false),
                c => (c.to_ascii_uppercase() as u16, c.is_ascii_uppercase() && !hold_shift),
            };
            if shift {
                send(VK_LSHIFT, false);
            }
            send(vk, false);
            send(vk, true);
            if shift {
                send(VK_LSHIFT, true);
            }
        }
        if hold_shift {
            send(VK_LSHIFT, true);
        }
    }

    /// Print a result line and keep it in `selftest.txt` (the console may close right after).
    fn say(line: &str) {
        println!("{line}");
        log(&format!("selftest: {line}"));
        let path = data_dir().join("selftest.txt");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }

    fn settle() {
        std::thread::sleep(Duration::from_millis(600));
    }

    struct Case {
        name: &'static str,
        mode: Mode,
        keys: &'static str,
        gap_ms: u64,
        hold_shift: bool,
        expect: &'static str,
        needs_english: bool,
    }

    fn script() -> i32 {
        settle();
        if !focused() {
            println!("테스트 창을 한 번 클릭해 주세요 (click the test window to start; waiting 30 s)");
            let deadline = Instant::now() + Duration::from_secs(30);
            while !focused() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            if !focused() {
                say("selftest: the test window never came to the front. Run it again and click the window.");
                return 2;
            }
            settle();
        }
        {
            let before = HOOK_KEYS.load(Relaxed);
            inject_f24(3);
            std::thread::sleep(Duration::from_millis(300));
            let same = HOOK_KEYS.load(Relaxed) - before;
            let before = HOOK_KEYS.load(Relaxed);
            let exe = std::env::current_exe().unwrap();
            let _ = std::process::Command::new(exe).arg("--diag-inject").status();
            std::thread::sleep(Duration::from_millis(300));
            let other = HOOK_KEYS.load(Relaxed) - before;
            say(&format!("diag: hook calls with test window in front: same-process 6 events -> {same}, other-process 10 events -> {other}"));
        }
        if ime_mode().is_none() {
            say("selftest: the Korean IME is not active for this window (input language must be 한국어 Microsoft IME).");
            return 2;
        }
        // Caps Lock changes what letters type; switch it off for the test and restore it after.
        let caps_was_on = unsafe { GetKeyState(VK_CAPITAL as i32) } & 1 != 0 || CAPS.load(Relaxed);
        if caps_was_on {
            say("Caps Lock is on: turning it off during the test (restored afterwards)");
            type_caps();
        }
        // The IME's conversion state (Hangul, full-width, ...) is shared with other apps: put it back
        // exactly as it was, whatever the test cases did to it.
        let ime_before = ime_window().and_then(|ime| ime_query(ime, IMC_GETCONVERSIONMODE));
        let code = run_cases();
        if let (Some(mode), Some(ime)) = (ime_before, ime_window()) {
            let mut out = 0usize;
            unsafe { SendMessageTimeoutW(ime, WM_IME_CONTROL, IMC_SETCONVERSIONMODE, mode as isize, SMTO_ABORTIFHUNG, 30, &mut out) };
        }
        if caps_was_on {
            type_caps();
        }
        code
    }

    fn type_caps() {
        let send = |up: bool| unsafe {
            let mut input: INPUT = std::mem::zeroed();
            input.r#type = INPUT_KEYBOARD;
            input.Anonymous.ki = KEYBDINPUT { wVk: VK_CAPITAL, wScan: 0x3A, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 };
            SendInput(1, &input, size_of::<INPUT>() as i32);
        };
        send(false);
        send(true);
        std::thread::sleep(Duration::from_millis(200));
    }

    fn run_cases() -> i32 {
        // Probe: does an injected 한/영 key commit the syllable being composed?
        clear();
        set_ime_mode(Mode::Hangul);
        settle();
        type_keys("gk^a", 30, false);
        settle();
        let probe = text();
        let commits = probe == "하a";
        say(&format!("probe: Hangul 'gk' + 한/영 + 'a' -> {probe:?} => composition {}", if commits { "commits (English fixes enabled)" } else { "does NOT commit (English fixes stay off)" }));
        TO_ENGLISH.store(commits, Relaxed);

        let cases = [
            Case { name: "EN mode Korean -> fixed", mode: Mode::English, keys: "dkssud ", gap_ms: 25, hold_shift: false, expect: "안녕 ", needs_english: false },
            Case { name: "bug 2: Shift held while typing Korean", mode: Mode::Hangul, keys: "DKSSUDGKSKDY ", gap_ms: 25, hold_shift: true, expect: "안녕하나요 ", needs_english: false },
            Case { name: "bug 2: Shift held (R is a real shifted key)", mode: Mode::Hangul, keys: "GKFRK ", gap_ms: 25, hold_shift: true, expect: "할까 ", needs_english: false },
            Case { name: "bug 1: burst typing across a fix", mode: Mode::English, keys: "dkssud gktpdy dhkt ", gap_ms: 0, hold_shift: false, expect: "안녕 하세요 왓 ", needs_english: false },
            Case { name: "repeat: fix, back to English, fix again x3", mode: Mode::English, keys: "dkssud |dkssud |dkssud ", gap_ms: 25, hold_shift: false, expect: "안녕 안녕 안녕 ", needs_english: false },
            Case { name: "live: 한/영 mid-word undoes it", mode: Mode::English, keys: "dkssu^d ", gap_ms: 60, hold_shift: false, expect: "dkssud ", needs_english: false },
            Case { name: "English kept", mode: Mode::English, keys: "hello world ", gap_ms: 15, hold_shift: false, expect: "hello world ", needs_english: false },
            Case { name: "undo with 한/영", mode: Mode::English, keys: "dkssud ^", gap_ms: 40, hold_shift: false, expect: "dkssud ", needs_english: false },
            Case { name: "HAN mode English -> fixed", mode: Mode::Hangul, keys: "hello ", gap_ms: 25, hold_shift: false, expect: "hello ", needs_english: true },
        ];
        let mut failed = 0;
        for case in &cases {
            if case.needs_english && !commits {
                say(&format!("SKIP  {}", case.name));
                continue;
            }
            if !focused() || LOST_FOCUS.load(Relaxed) {
                say("ABORT: test window lost focus (typing stopped)");
                return 2;
            }
            clear();
            set_ime_mode(case.mode);
            settle();
            type_keys("~", 10, false); // Esc: start from a clean word, like a real user would after a click
            type_keys(case.keys, case.gap_ms, case.hold_shift);
            settle();
            if LOST_FOCUS.load(Relaxed) {
                say("ABORT: test window lost focus (typing stopped)");
                return 2;
            }
            let got = text();
            let ok = got == case.expect;
            failed += !ok as i32;
            say(&format!("{}  {:<40} expected {:?}, got {:?}", if ok { "PASS" } else { "FAIL" }, case.name, case.expect, got));
        }
        let mut config = Config::load();
        config.to_english = commits;
        config.save();
        say(&format!("saved {} (to_english={commits})", Config::path().display()));
        failed
    }

    /// The test window lives in a child process, like any real app: with the window (and the
    /// Korean IME it loads) inside the hook's own process, Windows stopped delivering keys to the
    /// hook at all (measured: 0 hook calls, keys delayed and dropped).
    pub fn target() -> i32 {
        unsafe {
            let class = wide("hanfix.selftest");
            let instance = GetModuleHandleW(null_mut());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                hbrBackground: 6 as _, // COLOR_WINDOW + 1
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            let title = wide("hanfix selftest — 손대지 마세요 (do not touch)");
            let main = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 200, 200, 640, 160, null_mut(), null_mut(), instance, null_mut());
            let edit_class = wide("EDIT");
            let edit = CreateWindowExW(0, edit_class.as_ptr(), null_mut(), WS_CHILD | WS_VISIBLE | WS_BORDER | ES_AUTOHSCROLL as u32, 10, 10, 600, 30, main, null_mut(), instance, null_mut());
            EDIT.store(edit as usize, Relaxed);
            SetForegroundWindow(main);
            SetFocus(edit);
            println!("{} {}", main as usize, edit as usize);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            0
        }
    }

    pub fn run() -> i32 {
        let _ = std::fs::write(data_dir().join("selftest.txt"), "");
        let others = conflicting_programs();
        if !others.is_empty() {
            say(&format!("selftest: {} is running and would convert the test keys itself. Quit it (tray icon > 종료) and run again.", others.join(", ")));
            return 3;
        }
        start(Config { to_english: false, ..Config::load() });
        let exe = std::env::current_exe().expect("own path");
        let mut child = match std::process::Command::new(exe).arg("--selftest-target").stdout(std::process::Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                say(&format!("selftest: could not open the test window: {e}"));
                return 2;
            }
        };
        unsafe { AllowSetForegroundWindow(child.id()) };
        let mut line = String::new();
        let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line);
        let mut parts = line.split_whitespace().filter_map(|x| x.parse::<usize>().ok());
        let (Some(main), Some(edit)) = (parts.next(), parts.next()) else {
            say("selftest: the test window did not start");
            let _ = child.kill();
            return 2;
        };
        MAIN.store(main, Relaxed);
        EDIT.store(edit, Relaxed);
        let code = script();
        unsafe { PostMessageW(main as HWND, WM_CLOSE, 0, 0) };
        let _ = child.wait();
        code
    }
}
