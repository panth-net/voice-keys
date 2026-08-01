#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;
use std::{env, fs, process, thread};

use chrono::{Datelike, Local, NaiveDate, Utc};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use flexi_logger::{DeferredNow, Duplicate, FileSpec, Logger};
use log::{debug, error, info, warn};
#[cfg(not(target_os = "macos"))]
use rdev::EventType;
use rdev::Key;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
#[cfg(target_os = "windows")]
#[allow(unused_imports)]
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Theme as TaoTheme, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::WebViewBuilder;
#[cfg(target_os = "windows")]
use wry::{Theme as WebViewTheme, WebViewBuilderExtWindows};

// ---------------------------------------------------------------------------
// macOS: lightweight CGEventTap key listener that replaces rdev::listen.
// rdev calls TISGetInputSourceProperty from a background thread, which
// newer macOS versions forbid (dispatch_assert_queue → SIGTRAP).  This
// listener skips character-name resolution entirely — we only need key codes.
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod macos_keys {
    use rdev::Key;
    use std::os::raw::c_void;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
    use std::sync::OnceLock;

    type CGEventTapProxy = *const c_void;
    type CGEventRef = *const c_void;
    type CFMachPortRef = *const c_void;
    type CFRunLoopSourceRef = *const c_void;
    type CFRunLoopRef = *const c_void;
    type CFRunLoopMode = *const c_void;

    const K_CG_HEAD_INSERT: u32 = 0;
    const K_CG_LISTEN_ONLY: u32 = 1;
    const K_CG_HID_TAP: u32 = 0;
    const K_CG_SESSION_TAP: u32 = 1;
    const K_CG_KEY_DOWN: u32 = 10;
    const K_CG_KEY_UP: u32 = 11;
    const K_CG_FLAGS_CHANGED: u32 = 12;
    const K_CG_KEYCODE_FIELD: u32 = 9;
    const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
    const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFFFFFF;
    const K_CG_HID_EVENT_TAP_POST: u32 = 0;
    const K_CG_EVENT_FLAG_MASK_CAPS_LOCK: u64 = 1 << 16;
    const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 1 << 17;
    const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 1 << 18;
    const K_CG_EVENT_FLAG_MASK_ALT: u64 = 1 << 19;
    const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;
    const K_CG_EVENT_FLAG_MASK_FUNCTION: u64 = 1 << 23;
    const KEYCODE_V: u16 = 9;
    // Match rdev's full event mask — macOS may not activate the tap with keyboard-only mask
    const EVENT_MASK: u64 = (1 << 1)
        | (1 << 2)
        | (1 << 3)
        | (1 << 4)
        | (1 << 5)
        | (1 << 6)
        | (1 << 7)
        | (1 << K_CG_KEY_DOWN)
        | (1 << K_CG_KEY_UP)
        | (1 << K_CG_FLAGS_CHANGED)
        | (1 << 22); // mouse + keyboard + scroll

    #[derive(Debug, Clone, Copy)]
    pub enum KeyEvent {
        Press(Key),
        Release(Key),
    }

    #[link(name = "Cocoa", kind = "framework")]
    extern "C" {
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            mask: u64,
            cb: unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
            info: *mut c_void,
        ) -> CFMachPortRef;
        fn CFMachPortCreateRunLoopSource(
            a: *const c_void,
            p: CFMachPortRef,
            o: i64,
        ) -> CFRunLoopSourceRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, s: CFRunLoopSourceRef, m: CFRunLoopMode);
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFRunLoopGetMain() -> CFRunLoopRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CFRunLoopRun();
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtualKey: u16,
            keyDown: bool,
        ) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CFRelease(obj: *const c_void);
        static kCFRunLoopCommonModes: CFRunLoopMode;
    }

    // SAFETY: CALLBACK is written once, in `listen()`, before the event tap is
    // enabled, and is read only from `raw_callback` — which CoreFoundation
    // invokes on the same thread that owns the run loop. No other thread ever
    // touches it, so no synchronisation is required.
    #[allow(static_mut_refs)]
    static mut CALLBACK: Option<Box<dyn FnMut(KeyEvent)>> = None;
    static LAST_FLAGS: AtomicU64 = AtomicU64::new(0);
    /// Remaining raw key events to record. Stays at zero unless diagnostics are
    /// explicitly turned up to level 2.
    static RAW_LOG_BUDGET: AtomicUsize = AtomicUsize::new(0);

    /// Diagnostic verbosity, read once from `VOICEKEYS_DEBUG_KEYS`.
    ///
    /// `0` (the default) writes nothing at all and creates no file. `1` records
    /// the listener's own lifecycle plus the hotkeys Voice Keys has bound. `2`
    /// additionally records a bounded burst of raw key events *including keys
    /// that are not hotkeys* — it exists to diagnose a dead event tap, and is
    /// never enabled by default.
    static DEBUG_LEVEL: OnceLock<u8> = OnceLock::new();
    static DEBUG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

    fn debug_level() -> u8 {
        *DEBUG_LEVEL.get_or_init(|| match std::env::var("VOICEKEYS_DEBUG_KEYS").as_deref() {
            Ok("1") => 1,
            Ok("2") => 2,
            _ => 0,
        })
    }

    pub(crate) fn debug_enabled() -> bool {
        debug_level() >= 1
    }

    /// Where diagnostics go, or `None` when they are off.
    ///
    /// Deliberately under `app_data_dir()` rather than the working directory: a
    /// bundled app's CWD is arbitrary, so writing a key log into whatever folder
    /// the user happened to launch from is both surprising and hard to find again.
    fn debug_path() -> Option<&'static PathBuf> {
        DEBUG_PATH
            .get_or_init(|| {
                if debug_level() == 0 {
                    return None;
                }
                let dir = crate::app_data_dir();
                std::fs::create_dir_all(&dir).ok()?;
                Some(dir.join("debug_keys.log"))
            })
            .as_ref()
    }

    fn key_from_code(code: u16) -> Key {
        match code {
            0 => Key::KeyA,
            1 => Key::KeyS,
            2 => Key::KeyD,
            3 => Key::KeyF,
            4 => Key::KeyH,
            5 => Key::KeyG,
            6 => Key::KeyZ,
            7 => Key::KeyX,
            8 => Key::KeyC,
            9 => Key::KeyV,
            11 => Key::KeyB,
            12 => Key::KeyQ,
            13 => Key::KeyW,
            14 => Key::KeyE,
            15 => Key::KeyR,
            16 => Key::KeyY,
            17 => Key::KeyT,
            18 => Key::Num1,
            19 => Key::Num2,
            20 => Key::Num3,
            21 => Key::Num4,
            22 => Key::Num6,
            23 => Key::Num5,
            24 => Key::Equal,
            25 => Key::Num9,
            26 => Key::Num7,
            27 => Key::Minus,
            28 => Key::Num8,
            29 => Key::Num0,
            30 => Key::RightBracket,
            31 => Key::KeyO,
            32 => Key::KeyU,
            33 => Key::LeftBracket,
            34 => Key::KeyI,
            35 => Key::KeyP,
            36 => Key::Return,
            37 => Key::KeyL,
            38 => Key::KeyJ,
            39 => Key::Quote,
            40 => Key::KeyK,
            41 => Key::SemiColon,
            42 => Key::BackSlash,
            43 => Key::Comma,
            44 => Key::Slash,
            45 => Key::KeyN,
            46 => Key::KeyM,
            47 => Key::Dot,
            48 => Key::Tab,
            49 => Key::Space,
            50 => Key::BackQuote,
            51 => Key::Backspace,
            53 => Key::Escape,
            54 => Key::MetaRight,
            55 => Key::MetaLeft,
            56 => Key::ShiftLeft,
            57 => Key::CapsLock,
            58 => Key::Alt,
            59 => Key::ControlLeft,
            60 => Key::ShiftRight,
            61 => Key::AltGr,
            62 => Key::ControlRight,
            63 => Key::Function,
            96 => Key::F5,
            97 => Key::F6,
            98 => Key::F7,
            99 => Key::F3,
            100 => Key::F8,
            101 => Key::F9,
            103 => Key::F11,
            109 => Key::F10,
            111 => Key::F12,
            118 => Key::F4,
            120 => Key::F2,
            122 => Key::F1,
            123 => Key::LeftArrow,
            124 => Key::RightArrow,
            125 => Key::DownArrow,
            126 => Key::UpArrow,
            c => Key::Unknown(c.into()),
        }
    }

    fn modifier_flag_mask(key: Key) -> Option<u64> {
        match key {
            Key::MetaLeft | Key::MetaRight => Some(K_CG_EVENT_FLAG_MASK_COMMAND),
            Key::Alt | Key::AltGr => Some(K_CG_EVENT_FLAG_MASK_ALT),
            Key::ControlLeft | Key::ControlRight => Some(K_CG_EVENT_FLAG_MASK_CONTROL),
            Key::ShiftLeft | Key::ShiftRight => Some(K_CG_EVENT_FLAG_MASK_SHIFT),
            Key::CapsLock => Some(K_CG_EVENT_FLAG_MASK_CAPS_LOCK),
            Key::Function => Some(K_CG_EVENT_FLAG_MASK_FUNCTION),
            _ => None,
        }
    }

    use std::io::Write;

    pub(crate) fn dlog(msg: &str) {
        let Some(path) = debug_path() else {
            return;
        };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
        }
    }

    /// Record one raw key event, budget permitting.
    ///
    /// Gated here rather than inside `dlog` because this sits on the event tap's
    /// hot path: without the early return the `format!` would run for every
    /// keystroke on the system, whether or not anything is listening.
    #[inline]
    fn dlog_raw(kind: &str, code: u16, key: Key, flags: u64) {
        if debug_level() < 2 {
            return;
        }
        if RAW_LOG_BUDGET
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_err()
        {
            return;
        }
        dlog(&format!(
            "raw event: type={kind} code={code} key={key:?} flags=0x{flags:x}"
        ));
    }

    // macOS can disable the tap with timeout/user-input event types; we keep
    // the tap port so we can re-enable immediately.
    static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    pub fn simulate_cmd_v() -> bool {
        unsafe {
            let down = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_V, true);
            if down.is_null() {
                dlog("ERROR: CGEventCreateKeyboardEvent (v down) returned NULL");
                return false;
            }
            CGEventSetFlags(down, K_CG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(K_CG_HID_EVENT_TAP_POST, down);
            CFRelease(down);

            let up = CGEventCreateKeyboardEvent(std::ptr::null(), KEYCODE_V, false);
            if up.is_null() {
                dlog("ERROR: CGEventCreateKeyboardEvent (v up) returned NULL");
                return false;
            }
            CGEventSetFlags(up, K_CG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(K_CG_HID_EVENT_TAP_POST, up);
            CFRelease(up);
        }
        true
    }

    unsafe extern "C" fn raw_callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        cg_event: CGEventRef,
        _info: *mut c_void,
    ) -> CGEventRef {
        // Re-enable the tap if macOS disabled it due to timeout or user input.
        if etype == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
            || etype == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
        {
            dlog(&format!(
                "event tap disabled (etype=0x{:x}); re-enabling",
                etype
            ));
            let port = TAP_PORT.load(Ordering::Relaxed);
            if !port.is_null() {
                CGEventTapEnable(port, true);
            }
            return cg_event;
        }

        let ev = match etype {
            K_CG_KEY_DOWN => {
                let c = CGEventGetIntegerValueField(cg_event, K_CG_KEYCODE_FIELD) as u16;
                let key = key_from_code(c);
                dlog_raw("keydown", c, key, CGEventGetFlags(cg_event));
                Some(KeyEvent::Press(key))
            }
            K_CG_KEY_UP => {
                let c = CGEventGetIntegerValueField(cg_event, K_CG_KEYCODE_FIELD) as u16;
                let key = key_from_code(c);
                dlog_raw("keyup", c, key, CGEventGetFlags(cg_event));
                Some(KeyEvent::Release(key))
            }
            K_CG_FLAGS_CHANGED => {
                let c = CGEventGetIntegerValueField(cg_event, K_CG_KEYCODE_FIELD) as u16;
                let key = key_from_code(c);
                let flags = CGEventGetFlags(cg_event);
                let previous_flags = LAST_FLAGS.swap(flags, Ordering::Relaxed);
                dlog_raw("flags", c, key, flags);

                modifier_flag_mask(key).and_then(|mask| {
                    let was_down = (previous_flags & mask) != 0;
                    let is_down = (flags & mask) != 0;
                    if was_down == is_down {
                        None
                    } else if is_down {
                        Some(KeyEvent::Press(key))
                    } else {
                        Some(KeyEvent::Release(key))
                    }
                })
            }
            _ => None,
        };
        if let Some(e) = ev {
            let cb_ptr = &raw mut CALLBACK;
            if let Some(cb) = &mut *cb_ptr {
                cb(e);
            }
        }
        cg_event
    }

    pub fn listen<F: FnMut(KeyEvent) + 'static>(callback: F) {
        // Start each run with a fresh diagnostic file — but only when the user
        // has actually asked for one.
        if let Some(path) = debug_path() {
            let _ = std::fs::write(path, "");
        }
        dlog("=== macos_keys::listen starting ===");
        if debug_level() >= 2 {
            RAW_LOG_BUDGET.store(200, Ordering::Relaxed);
        }

        unsafe {
            CALLBACK = Some(Box::new(callback));
            let _pool = objc_autoreleasePoolPush();
            dlog("autorelease pool created");

            // Try HID tap first, fall back to Session tap
            dlog("creating CGEventTap (HID, listen-only)...");
            let mut tap = CGEventTapCreate(
                K_CG_HID_TAP,
                K_CG_HEAD_INSERT,
                K_CG_LISTEN_ONLY,
                EVENT_MASK,
                raw_callback,
                std::ptr::null_mut(),
            );
            if tap.is_null() {
                dlog("HID tap returned NULL, trying Session tap...");
                tap = CGEventTapCreate(
                    K_CG_SESSION_TAP,
                    K_CG_HEAD_INSERT,
                    K_CG_LISTEN_ONLY,
                    EVENT_MASK,
                    raw_callback,
                    std::ptr::null_mut(),
                );
            }
            if tap.is_null() {
                // Goes to the normal log too, not just the opt-in diagnostic:
                // this is why someone's hotkeys silently do nothing, and they
                // shouldn't have to know about an environment variable to find out.
                log::error!(
                    "could not watch the keyboard — grant Voice Keys both Accessibility and \
                     Input Monitoring in System Settings > Privacy & Security, then quit and \
                     reopen the app"
                );
                dlog("ERROR: both HID and Session taps failed — check Accessibility & Input Monitoring permissions");
                return;
            }
            log::info!("keyboard listener started");
            dlog(&format!("CGEventTap created OK (ptr={:?})", tap));

            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if src.is_null() {
                dlog("ERROR: CFMachPortCreateRunLoopSource returned NULL");
                return;
            }
            dlog("RunLoopSource created OK");

            // Add source to BOTH the main run loop and the background run loop.
            // The main run loop is already being driven by tao's event loop.
            let main_rl = CFRunLoopGetMain();
            let bg_rl = CFRunLoopGetCurrent();
            dlog(&format!(
                "main_rl={:?} bg_rl={:?} same={}",
                main_rl,
                bg_rl,
                main_rl == bg_rl
            ));

            CFRunLoopAddSource(main_rl, src, kCFRunLoopCommonModes);
            dlog("added source to main run loop");

            if main_rl != bg_rl {
                CFRunLoopAddSource(bg_rl, src, kCFRunLoopCommonModes);
                dlog("added source to background run loop");
            }

            TAP_PORT.store(tap as *mut c_void, Ordering::Relaxed);
            CGEventTapEnable(tap, true);
            dlog("CGEventTap enabled");

            dlog("entering CFRunLoopRun on background thread...");
            CFRunLoopRun();
            dlog("CFRunLoopRun returned (unexpected)");
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct Config {
    pub deepgram: DeepgramConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub hotkeys: HotkeyConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeepgramConfig {
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default = "default_true")]
    pub punctuate: bool,
    #[serde(default = "default_true")]
    pub smart_format: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: f64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AudioConfig {
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_max_recording_minutes")]
    pub max_recording_minutes: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HotkeyConfig {
    #[serde(default = "default_paste_keys")]
    pub paste: Vec<String>,
    #[serde(default = "default_clipboard_keys")]
    pub clipboard: Vec<String>,
}

/// Deepgram's multilingual language code, and the only model family that supports it.
const MULTILINGUAL_LANGUAGE: &str = "multi";
const MULTILINGUAL_MODEL: &str = "nova-3";
/// Models offered in the settings dropdown. Keep in sync with ui/index.html.
const SELECTABLE_MODELS: [&str; 2] = ["nova-3", "nova-2"];

fn default_model() -> String {
    "nova-3".into()
}
fn default_lang() -> String {
    "en".into()
}
fn default_true() -> bool {
    true
}
fn default_timeout() -> f64 {
    60.0
}
fn default_sample_rate() -> u32 {
    16000
}
fn default_max_recording_minutes() -> u32 {
    20
}
fn default_modifier_key() -> String {
    "alt".into()
}
fn default_paste_keys() -> Vec<String> {
    vec![default_modifier_key(), "dot".into()]
}
fn default_clipboard_keys() -> Vec<String> {
    vec![default_modifier_key(), "slash".into()]
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_model(),
            language: default_lang(),
            punctuate: default_true(),
            smart_format: default_true(),
            timeout_secs: default_timeout(),
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            max_recording_minutes: default_max_recording_minutes(),
        }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            paste: default_paste_keys(),
            clipboard: default_clipboard_keys(),
        }
    }
}

fn map_key(name: &str) -> Option<Key> {
    match name.to_lowercase().as_str() {
        "a" => Some(Key::KeyA),
        "b" => Some(Key::KeyB),
        "c" => Some(Key::KeyC),
        "d" => Some(Key::KeyD),
        "e" => Some(Key::KeyE),
        "f" => Some(Key::KeyF),
        "g" => Some(Key::KeyG),
        "h" => Some(Key::KeyH),
        "i" => Some(Key::KeyI),
        "j" => Some(Key::KeyJ),
        "k" => Some(Key::KeyK),
        "l" => Some(Key::KeyL),
        "m" => Some(Key::KeyM),
        "n" => Some(Key::KeyN),
        "o" => Some(Key::KeyO),
        "p" => Some(Key::KeyP),
        "q" => Some(Key::KeyQ),
        "r" => Some(Key::KeyR),
        "s" => Some(Key::KeyS),
        "t" => Some(Key::KeyT),
        "u" => Some(Key::KeyU),
        "v" => Some(Key::KeyV),
        "w" => Some(Key::KeyW),
        "x" => Some(Key::KeyX),
        "y" => Some(Key::KeyY),
        "z" => Some(Key::KeyZ),
        "0" | "num0" => Some(Key::Num0),
        "1" | "num1" => Some(Key::Num1),
        "2" | "num2" => Some(Key::Num2),
        "3" | "num3" => Some(Key::Num3),
        "4" | "num4" => Some(Key::Num4),
        "5" | "num5" => Some(Key::Num5),
        "6" | "num6" => Some(Key::Num6),
        "7" | "num7" => Some(Key::Num7),
        "8" | "num8" => Some(Key::Num8),
        "9" | "num9" => Some(Key::Num9),
        "minus" | "-" => Some(Key::Minus),
        "equal" | "=" | "plus" | "+" => Some(Key::Equal),
        "space" => Some(Key::Space),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "enter" | "return" => Some(Key::Return),
        "escape" | "esc" => Some(Key::Escape),
        "backquote" | "backtick" | "tilde" | "`" | "~" => Some(Key::BackQuote),
        "leftbracket" | "[" => Some(Key::LeftBracket),
        "rightbracket" | "]" => Some(Key::RightBracket),
        "semicolon" | ";" => Some(Key::SemiColon),
        "quote" | "'" => Some(Key::Quote),
        "backslash" | "\\" => Some(Key::BackSlash),
        "comma" | "," => Some(Key::Comma),
        "dot" | "." | "period" => Some(Key::Dot),
        "slash" | "/" | "forwardslash" => Some(Key::Slash),
        "shift" | "lshift" => Some(Key::ShiftLeft),
        "rshift" => Some(Key::ShiftRight),
        "ctrl" | "control" | "lctrl" => Some(Key::ControlLeft),
        "rctrl" => Some(Key::ControlRight),
        "alt" | "lalt" | "option" => Some(Key::Alt),
        "ralt" => Some(Key::AltGr),
        "cmd" | "meta" | "super" | "win" => Some(Key::MetaLeft),
        "rcmd" | "rmeta" => Some(Key::MetaRight),
        "capslock" => Some(Key::CapsLock),
        "f1" => Some(Key::F1),
        "f2" => Some(Key::F2),
        "f3" => Some(Key::F3),
        "f4" => Some(Key::F4),
        "f5" => Some(Key::F5),
        "f6" => Some(Key::F6),
        "f7" => Some(Key::F7),
        "f8" => Some(Key::F8),
        "f9" => Some(Key::F9),
        "f10" => Some(Key::F10),
        "f11" => Some(Key::F11),
        "f12" => Some(Key::F12),
        _ => None,
    }
}

fn normalize(key: Key) -> Key {
    match key {
        Key::ShiftRight => Key::ShiftLeft,
        Key::ControlRight => Key::ControlLeft,
        Key::MetaRight => Key::MetaLeft,
        Key::AltGr => Key::Alt,
        _ => key,
    }
}

fn parse_combo(names: &[String]) -> HashSet<Key> {
    names.iter().filter_map(|n| map_key(n)).collect()
}

fn combo_label(names: &[String]) -> String {
    names.join(" + ")
}

fn normalize_hotkey_part(input: &str) -> String {
    input.trim().to_lowercase()
}

fn combo_fields_for_ui(keys: &[String]) -> (String, String) {
    match keys {
        [] => (String::new(), String::new()),
        [single] => (String::new(), single.clone()),
        [modifier, trigger, ..] => (modifier.clone(), trigger.clone()),
    }
}

fn combo_from_parts(modifier: &str, trigger: &str) -> Vec<String> {
    let modifier = normalize_hotkey_part(modifier);
    let trigger = normalize_hotkey_part(trigger);
    let mut combo = Vec::new();
    if !modifier.is_empty() {
        combo.push(modifier);
    }
    if !trigger.is_empty() {
        combo.push(trigger);
    }
    combo
}

/// Borrow at most `max_chars` characters from the front of `s`.
///
/// `&s[..n]` slices by *byte* offset and panics outright when `n` lands inside a
/// multi-byte character. Transcripts are routinely non-ASCII — a CJK one is three
/// bytes per character — so every truncation of user text has to go through this.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Longest audio source name the header line will show, the trailing `..`
/// included.
///
/// A character cap rather than a pixel one: the device name is a passing detail
/// beside the title, and capping it in characters makes it render to the same
/// length whatever the window is doing. `MacBook Air Microphone Input Con` is
/// exactly this long and still shows in full.
const MAX_AUDIO_SOURCE_CHARS: usize = 32;

/// Marks a name that had to be cut short. Two dots rather than three: it is a
/// cut mark, not an ellipsis, and it buys back a character of the name.
const TRUNCATION_MARK: &str = "..";

/// Cut `text` to at most `max_chars` *including* the trailing `..`.
///
/// A name that already fits comes back untouched — the mark only ever appears
/// where something was actually removed.
fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    // Counting in chars, not bytes: device names carry emoji and accents, and
    // byte-slicing one of those mid-character panics. The trim keeps the cut
    // from reading as "Input .." when it lands on a space.
    let kept = truncate_chars(text, max_chars.saturating_sub(TRUNCATION_MARK.len())).trim_end();
    format!("{}{}", kept, TRUNCATION_MARK)
}

fn notify(title: &str, body: &str) {
    let t = title.to_owned();
    let b = body.to_owned();
    thread::spawn(move || {
        let result = notify_inner(&t, &b);
        if let Err(e) = result {
            warn!("notification failed: {}", e);
        }
    });
}

fn notify_inner(title: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        // AppleScript string literals take a bare `"` delimiter. Escape the
        // backslashes first, or the ones we add for `"` get escaped in turn and
        // the quote breaks out of the literal.
        fn applescript_literal(s: &str) -> String {
            s.replace('\\', "\\\\").replace('"', "\\\"")
        }
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            applescript_literal(body),
            applescript_literal(title),
        );
        process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .spawn()?;
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let ps = format!(
            r#"[void][System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms');
[void][System.Reflection.Assembly]::LoadWithPartialName('System.Drawing');
$n=New-Object System.Windows.Forms.NotifyIcon;
$n.Icon=[System.Drawing.SystemIcons]::Information;
$n.Visible=$true;
$n.ShowBalloonTip(3000,'{}','{}','Info');
Start-Sleep -Seconds 4;
$n.Dispose();"#,
            title.replace('\'', "''"),
            body.replace('\'', "''"),
        );
        process::Command::new("powershell")
            .args(["-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .spawn()?;
    }

    #[cfg(target_os = "linux")]
    {
        process::Command::new("notify-send")
            .args([title, body])
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .spawn()?;
    }

    Ok(())
}

enum TranscribeError {
    MissingApiKey,
    RequestFailed,
}

fn encode_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .expect("WAV writer creation should not fail on in-memory buffer");
        for &s in samples {
            writer.write_sample(s).expect("WAV sample write failed");
        }
        writer.finalize().expect("WAV finalize failed");
    }
    cursor.into_inner()
}

/// Resolve the model to use for a language, falling back for unknown values.
///
/// Deepgram's `multi` is only multilingual on nova-3: asking nova-2 for it does
/// *not* fail, it quietly resolves to nova-2's monolingual model (which covers
/// only Spanish + English), so other languages come back as garbage. The UI locks
/// the dropdown to nova-3 for `multi`; this enforces the same rule for
/// hand-edited configs and is the single source of truth for both.
fn normalize_model(model: &str, language: &str) -> String {
    if language.eq_ignore_ascii_case(MULTILINGUAL_LANGUAGE) {
        return MULTILINGUAL_MODEL.to_string();
    }
    let model = model.trim();
    if SELECTABLE_MODELS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(model))
    {
        return model.to_ascii_lowercase();
    }
    // Unrecognized value (typo, or a specialty model set by hand) — keep it
    // rather than silently overriding a deliberate choice, unless it is empty.
    if model.is_empty() {
        default_model()
    } else {
        model.to_string()
    }
}

/// The model to actually request for a given config.
fn effective_model(cfg: &DeepgramConfig) -> String {
    let resolved = normalize_model(&cfg.model, &cfg.language);
    if resolved != cfg.model {
        warn!(
            "language '{}' with model '{}' -> using '{}' for this request",
            cfg.language, cfg.model, resolved
        );
    }
    resolved
}

fn transcribe(
    wav: &[u8],
    cfg: &DeepgramConfig,
    audio_duration_secs: f64,
) -> Result<Option<String>, TranscribeError> {
    if cfg.api_key.trim().is_empty() {
        warn!("Deepgram API key is empty; update it from the Voice Keys tray UI");
        return Err(TranscribeError::MissingApiKey);
    }

    let url = "https://api.deepgram.com/v1/listen";
    let client = reqwest::blocking::Client::new();
    let base = cfg.timeout_secs.max(60.0);
    let computed = base + audio_duration_secs;
    info!(
        "Deepgram timeout: {:.0}s (base {:.0} + audio {:.1})",
        computed, base, audio_duration_secs
    );
    let timeout = Duration::from_secs_f64(computed);

    let model = effective_model(cfg);
    let resp = client
        .post(url)
        .query(&[
            ("model", model.as_str()),
            ("language", cfg.language.as_str()),
            ("punctuate", if cfg.punctuate { "true" } else { "false" }),
            (
                "smart_format",
                if cfg.smart_format { "true" } else { "false" },
            ),
        ])
        .header("Authorization", format!("Token {}", cfg.api_key))
        .header("Content-Type", "audio/wav")
        .body(wav.to_vec())
        .timeout(timeout)
        .send();

    match resp {
        Ok(r) => {
            if !r.status().is_success() {
                let status = r.status();
                let body = r.text().unwrap_or_default();
                error!("Deepgram HTTP {}: {}", status, truncate_chars(&body, 200));
                return Err(TranscribeError::RequestFailed);
            }
            let json: serde_json::Value = r.json().map_err(|_| TranscribeError::RequestFailed)?;
            let transcript = json["results"]["channels"][0]["alternatives"][0]["transcript"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            if transcript.is_empty() {
                Ok(None)
            } else {
                Ok(Some(transcript))
            }
        }
        Err(e) => {
            error!("Deepgram request failed: {}", e);
            Err(TranscribeError::RequestFailed)
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeepgramProjectsResponse {
    #[serde(default)]
    projects: Vec<DeepgramProject>,
}

#[derive(Debug, Deserialize)]
struct DeepgramProject {
    #[serde(default)]
    project_id: String,
}

#[derive(Debug, Deserialize)]
struct DeepgramUsageBreakdownResponse {
    #[serde(default)]
    results: Vec<DeepgramUsageBreakdownResult>,
}

#[derive(Debug, Deserialize)]
struct DeepgramUsageBreakdownResult {
    #[serde(default)]
    hours: f64,
    #[serde(default)]
    total_hours: f64,
    #[serde(default)]
    requests: u64,
    #[serde(default)]
    grouping: Option<DeepgramGrouping>,
}

/// Each breakdown result carries the day it covers under `grouping.start`, which
/// is what lets a single wide-range request be re-bucketed into months.
#[derive(Debug, Deserialize)]
struct DeepgramGrouping {
    #[serde(default)]
    start: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeepgramBillingBreakdownResponse {
    #[serde(default)]
    results: Vec<DeepgramBillingBreakdownResult>,
}

#[derive(Debug, Deserialize)]
struct DeepgramBillingBreakdownResult {
    #[serde(default)]
    dollars: f64,
    #[serde(default)]
    grouping: Option<DeepgramGrouping>,
}

#[derive(Debug, Deserialize)]
struct DeepgramBalancesResponse {
    #[serde(default)]
    balances: Vec<DeepgramBalanceEntry>,
}

#[derive(Debug, Deserialize)]
struct DeepgramBalanceEntry {
    #[serde(default)]
    amount: f64,
    #[serde(default)]
    units: String,
}

#[derive(Debug, Clone, Serialize)]
struct DeepgramUsageUiPayload {
    period: String,
    minutes_used: String,
    requests: String,
    spend: String,
    balance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DeepgramUsageMonth {
    /// Tooltip heading, e.g. "Mar 2026".
    label: String,
    /// Compact x-axis tick, e.g. "Mar".
    axis_label: String,
    /// Raw minutes, used to scale bar heights.
    minutes: f64,
    minutes_label: String,
    requests_label: String,
    spend_label: String,
}

#[derive(Debug, Clone, Serialize)]
struct DeepgramUsageHistoryPayload {
    range_label: String,
    total_minutes: String,
    total_requests: String,
    total_spend: String,
    months: Vec<DeepgramUsageMonth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct MonthBucket {
    minutes: f64,
    requests: u64,
    dollars: f64,
}

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month_axis_label(month: u32) -> &'static str {
    month
        .checked_sub(1)
        .and_then(|idx| MONTH_NAMES.get(idx as usize))
        .copied()
        .unwrap_or("?")
}

/// Pull the `YYYY-MM` bucket key out of a breakdown result's grouping start date.
fn month_key_from_grouping(grouping: &Option<DeepgramGrouping>) -> Option<(i32, u32)> {
    let start = grouping.as_ref()?.start.as_ref()?;
    let date = NaiveDate::parse_from_str(start.trim(), "%Y-%m-%d").ok()?;
    Some((date.year(), date.month()))
}

/// All-time usage, aggregated per calendar month.
///
/// `/usage/breakdown` and `/billing/breakdown` return one result per *day* with
/// activity (verified against the live API), and reject `resolution=month`, so a
/// single wide request per endpoint is fetched and re-bucketed here. Days with no
/// activity are simply absent from the response, so gap months are synthesized as
/// zeros to keep the x-axis continuous.
fn fetch_deepgram_usage_history(api_key: &str) -> Result<DeepgramUsageHistoryPayload, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("Set your Deepgram API key first, then refresh usage.".to_string());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {}", e))?;

    let projects: DeepgramProjectsResponse = deepgram_get_json(
        &client,
        api_key,
        "/v1/projects",
        "https://api.deepgram.com/v1/projects",
        None,
    )
    .map_err(add_admin_usage_hint)?;

    let project = projects
        .projects
        .first()
        .ok_or_else(|| {
            "No Deepgram projects are available for this API key. Usage statistics require a Deepgram Admin-level API key.".to_string()
        })?;
    if project.project_id.trim().is_empty() {
        return Err("Deepgram project response did not include a project_id.".to_string());
    }

    // The projects endpoint exposes no creation date, so "all time" uses a fixed
    // five-year floor and the chart starts at the first month with real activity.
    let today = Utc::now().date_naive();
    let floor = NaiveDate::from_ymd_opt(today.year() - 5, 1, 1).unwrap_or(today);
    let start = floor.format("%Y-%m-%d").to_string();
    let end = today.format("%Y-%m-%d").to_string();
    let query = [("start", start.clone()), ("end", end.clone())];

    let usage_url = format!(
        "https://api.deepgram.com/v1/projects/{}/usage/breakdown",
        project.project_id
    );
    let usage: DeepgramUsageBreakdownResponse = deepgram_get_json(
        &client,
        api_key,
        "/usage/breakdown",
        &usage_url,
        Some(&query),
    )
    .map_err(add_admin_usage_hint)?;

    let mut buckets: BTreeMap<(i32, u32), MonthBucket> = BTreeMap::new();
    let mut undated_days = 0_usize;
    for result in &usage.results {
        let key = match month_key_from_grouping(&result.grouping) {
            Some(key) => key,
            None => {
                undated_days += 1;
                continue;
            }
        };
        let entry = buckets.entry(key).or_default();
        // Same convention as the month-to-date card.
        entry.minutes += result.total_hours.max(result.hours) * 60.0;
        entry.requests += result.requests;
    }

    let mut notes: Vec<String> = Vec::new();
    if projects.projects.len() > 1 {
        notes.push(format!(
            "API key can access {} projects; showing the first one.",
            projects.projects.len()
        ));
    }
    if undated_days > 0 {
        notes.push(format!(
            "{} usage entries had no date and are excluded from the monthly chart.",
            undated_days
        ));
    }

    let billing_url = format!(
        "https://api.deepgram.com/v1/projects/{}/billing/breakdown",
        project.project_id
    );
    let mut spend_available = true;
    match deepgram_get_json::<DeepgramBillingBreakdownResponse>(
        &client,
        api_key,
        "/billing/breakdown",
        &billing_url,
        Some(&query),
    ) {
        Ok(billing) => {
            for result in &billing.results {
                if let Some(key) = month_key_from_grouping(&result.grouping) {
                    buckets.entry(key).or_default().dollars += result.dollars;
                }
            }
        }
        Err(e) => {
            spend_available = false;
            notes.push(format!(
                "Spend unavailable: {}",
                short_text(&add_admin_usage_hint(e), 120)
            ));
        }
    }

    let total_minutes: f64 = buckets.values().map(|b| b.minutes).sum();
    let total_requests: u64 = buckets.values().map(|b| b.requests).sum();
    let total_dollars: f64 = buckets.values().map(|b| b.dollars).sum();

    // Walk from the first active month to the current one so gaps render as zero bars.
    let mut months: Vec<DeepgramUsageMonth> = Vec::new();
    if let Some((&(first_year, first_month), _)) = buckets.iter().next() {
        let mut year = first_year;
        let mut month = first_month;
        let (last_year, last_month) = (today.year(), today.month());
        loop {
            let bucket = buckets.get(&(year, month)).copied().unwrap_or_default();
            months.push(DeepgramUsageMonth {
                label: format!("{} {}", month_axis_label(month), year),
                axis_label: month_axis_label(month).to_string(),
                minutes: bucket.minutes,
                minutes_label: format!("{:.1} billable min", bucket.minutes),
                requests_label: format!("{} requests", format_with_commas(bucket.requests)),
                spend_label: if spend_available {
                    format_usd(bucket.dollars)
                } else {
                    "Unavailable".to_string()
                },
            });

            if year == last_year && month == last_month {
                break;
            }
            if year > last_year || (year == last_year && month > last_month) {
                // Guard against a clock skew where data is newer than "today".
                break;
            }
            if month == 12 {
                year += 1;
                month = 1;
            } else {
                month += 1;
            }
        }
    }

    let range_label = match (months.first(), months.last()) {
        (Some(first), Some(last)) if months.len() > 1 => {
            format!("All time · {} – {}", first.label, last.label)
        }
        (Some(first), _) => format!("All time · {}", first.label),
        _ => format!("All time · no usage recorded since {}", start),
    };

    Ok(DeepgramUsageHistoryPayload {
        range_label,
        total_minutes: format!("{:.1} billable min", total_minutes),
        total_requests: format!("{} requests", format_with_commas(total_requests)),
        total_spend: if spend_available {
            format_usd(total_dollars)
        } else {
            "Unavailable".to_string()
        },
        months,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join(" "))
        },
    })
}

fn short_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut result = String::new();
    for c in input.chars().take(max_chars.saturating_sub(1)) {
        result.push(c);
    }
    result.push('…');
    result
}

fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut reversed = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(ch);
    }
    reversed.chars().rev().collect()
}

fn format_usd(value: f64) -> String {
    format!("${:.2}", value)
}

fn add_admin_usage_hint(message: String) -> String {
    if message.contains("HTTP 401") || message.contains("HTTP 403") {
        return format!(
            "{} Usage statistics require a Deepgram Admin-level API key.",
            message
        );
    }
    message
}

fn format_balances(entries: &[DeepgramBalanceEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut totals_by_units: BTreeMap<String, f64> = BTreeMap::new();
    for entry in entries {
        let units = if entry.units.trim().is_empty() {
            "units".to_string()
        } else {
            entry.units.trim().to_string()
        };
        *totals_by_units.entry(units).or_insert(0.0) += entry.amount;
    }

    if totals_by_units.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    for (units, amount) in totals_by_units {
        if units.eq_ignore_ascii_case("usd") {
            parts.push(format_usd(amount));
        } else {
            parts.push(format!("{:.2} {}", amount, units));
        }
    }

    Some(parts.join(" + "))
}

fn parse_deepgram_json_response<T: DeserializeOwned>(
    endpoint_label: &str,
    response: reqwest::blocking::Response,
) -> Result<T, String> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default().replace('\n', " ");
        let body = short_text(body.trim(), 180);
        if body.is_empty() {
            return Err(format!(
                "{} request failed with HTTP {}",
                endpoint_label, status
            ));
        }
        return Err(format!(
            "{} request failed with HTTP {}: {}",
            endpoint_label, status, body
        ));
    }

    response
        .json::<T>()
        .map_err(|e| format!("{} response parse failed: {}", endpoint_label, e))
}

fn deepgram_get_json<T: DeserializeOwned>(
    client: &reqwest::blocking::Client,
    api_key: &str,
    endpoint_label: &str,
    url: &str,
    query_params: Option<&[(&str, String)]>,
) -> Result<T, String> {
    let mut request = client
        .get(url)
        .header("Authorization", format!("Token {}", api_key))
        .header("Accept", "application/json");
    if let Some(query) = query_params {
        request = request.query(query);
    }
    let response = request
        .send()
        .map_err(|e| format!("{} request failed: {}", endpoint_label, e))?;
    parse_deepgram_json_response(endpoint_label, response)
}

fn fetch_deepgram_usage_payload(api_key: &str) -> Result<DeepgramUsageUiPayload, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("Set your Deepgram API key first, then refresh usage.".to_string());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {}", e))?;

    let projects: DeepgramProjectsResponse = deepgram_get_json(
        &client,
        api_key,
        "/v1/projects",
        "https://api.deepgram.com/v1/projects",
        None,
    )
    .map_err(add_admin_usage_hint)?;

    let project = projects
        .projects
        .first()
        .ok_or_else(|| {
            "No Deepgram projects are available for this API key. Usage statistics require a Deepgram Admin-level API key.".to_string()
        })?;
    if project.project_id.trim().is_empty() {
        return Err("Deepgram project response did not include a project_id.".to_string());
    }

    let today = Utc::now().date_naive();
    let month_start = today.with_day(1).unwrap_or(today);
    let start = month_start.format("%Y-%m-%d").to_string();
    let end = today.format("%Y-%m-%d").to_string();
    let query = [("start", start.clone()), ("end", end.clone())];

    let usage_url = format!(
        "https://api.deepgram.com/v1/projects/{}/usage/breakdown",
        project.project_id
    );
    let usage: DeepgramUsageBreakdownResponse = deepgram_get_json(
        &client,
        api_key,
        "/usage/breakdown",
        &usage_url,
        Some(&query),
    )
    .map_err(add_admin_usage_hint)?;

    let usage_hours: f64 = usage.results.iter().map(|result| result.hours).sum();
    let usage_total_hours: f64 = usage.results.iter().map(|result| result.total_hours).sum();
    let minutes_used = usage_total_hours.max(usage_hours) * 60.0;
    let requests_used: u64 = usage.results.iter().map(|result| result.requests).sum();

    let mut notes: Vec<String> = Vec::new();
    if projects.projects.len() > 1 {
        notes.push(format!(
            "API key can access {} projects; showing the first one.",
            projects.projects.len()
        ));
    }

    let billing_url = format!(
        "https://api.deepgram.com/v1/projects/{}/billing/breakdown",
        project.project_id
    );
    let spend = match deepgram_get_json::<DeepgramBillingBreakdownResponse>(
        &client,
        api_key,
        "/billing/breakdown",
        &billing_url,
        Some(&query),
    ) {
        Ok(billing) => format_usd(
            billing
                .results
                .iter()
                .map(|entry| entry.dollars)
                .sum::<f64>(),
        ),
        Err(e) => {
            notes.push(format!(
                "Spend unavailable: {}",
                short_text(&add_admin_usage_hint(e), 120)
            ));
            "Unavailable".to_string()
        }
    };

    let balances_url = format!(
        "https://api.deepgram.com/v1/projects/{}/balances",
        project.project_id
    );
    let balance = match deepgram_get_json::<DeepgramBalancesResponse>(
        &client,
        api_key,
        "/balances",
        &balances_url,
        None,
    ) {
        Ok(balances) => format_balances(&balances.balances).unwrap_or_else(|| "None".to_string()),
        Err(e) => {
            notes.push(format!(
                "Balance unavailable: {}",
                short_text(&add_admin_usage_hint(e), 120)
            ));
            "Unavailable".to_string()
        }
    };

    Ok(DeepgramUsageUiPayload {
        period: format!("Period: {} to {} (month-to-date, UTC)", start, end),
        minutes_used: format!("{:.1} billable min", minutes_used),
        requests: format!("{} requests", format_with_commas(requests_used)),
        spend,
        balance,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join(" "))
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Paste,
    Clipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualState {
    Idle,
    Recording,
    Processing,
    Finished,
}

#[cfg(not(target_os = "macos"))]
fn simulate_event(ev: &EventType) -> bool {
    match rdev::simulate(ev) {
        Ok(_) => true,
        Err(e) => {
            warn!("simulate {:?} failed: {:?}", ev, e);
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn simulate_paste_once(modifier: Key, delay: Duration) -> bool {
    let modifier_press = EventType::KeyPress(modifier);
    if !simulate_event(&modifier_press) {
        // Avoid typing plain "v" when the modifier does not go down.
        return false;
    }
    thread::sleep(delay);

    let v_press = EventType::KeyPress(Key::KeyV);
    if !simulate_event(&v_press) {
        let _ = simulate_event(&EventType::KeyRelease(modifier));
        return false;
    }
    thread::sleep(delay);

    let v_release = EventType::KeyRelease(Key::KeyV);
    if !simulate_event(&v_release) {
        let _ = simulate_event(&EventType::KeyRelease(modifier));
        return false;
    }
    thread::sleep(delay);

    let modifier_release = EventType::KeyRelease(modifier);
    simulate_event(&modifier_release)
}

fn simulate_paste() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_keys::simulate_cmd_v()
    }

    #[cfg(not(target_os = "macos"))]
    {
        let modifier = Key::ControlLeft;
        if simulate_paste_once(modifier, Duration::from_millis(20)) {
            return true;
        }

        warn!("paste simulation retrying with longer delay");
        thread::sleep(Duration::from_millis(40));
        simulate_paste_once(modifier, Duration::from_millis(45))
    }
}

fn send_visual_state(proxy: &EventLoopProxy<UserEvent>, state: VisualState) {
    let _ = proxy.send_event(UserEvent::VisualState(state));
}

fn send_banner(proxy: &EventLoopProxy<UserEvent>, kind: &str, message: &str) {
    let _ = proxy.send_event(UserEvent::StatusBanner {
        kind: kind.to_string(),
        message: message.to_string(),
    });
}

fn complete_visual_cycle(proxy: &EventLoopProxy<UserEvent>) {
    send_visual_state(proxy, VisualState::Finished);
    let proxy = proxy.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1200));
        let _ = proxy.send_event(UserEvent::VisualState(VisualState::Idle));
    });
}

/// How many times the user may re-send one stashed recording before Voice Keys
/// stops offering.
///
/// Every retry is an explicit click *and* a paid Deepgram request, so the offer
/// is capped rather than open-ended: audio that comes back empty three times
/// running is a muted microphone, not a flaky API, and further requests only
/// spend credit to learn the same thing again.
const MAX_USER_RETRIES: u32 = 3;

/// Where a failed recording's audio is parked so it can be re-sent.
///
/// One fixed path, deliberately not a unique name per failure: a stash that
/// accumulated files would quietly fill the config directory with hundreds of
/// megabytes of WAV nobody asked for. The next failure overwrites this one, so
/// at most a single stray file can ever exist.
static RETRY_AUDIO_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Serialises "write the WAV, then announce it" so the file on disk always
/// matches the newest offer the event loop has seen. Two recordings really can
/// be in flight at once — you can start a new one while the previous is still
/// transcribing — and without this their writes and events could interleave.
static RETRY_STASH_LOCK: Mutex<()> = Mutex::new(());

static NEXT_RETRY_OFFER_ID: AtomicU64 = AtomicU64::new(1);

fn retry_audio_path() -> Option<&'static Path> {
    RETRY_AUDIO_PATH.get().map(PathBuf::as_path)
}

/// Delete the stashed audio, if any. Safe to call when nothing is stashed.
fn clear_retry_stash(why: &str) {
    let Some(path) = retry_audio_path() else {
        return;
    };
    let _guard = RETRY_STASH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    match fs::remove_file(path) {
        Ok(()) => info!("deleted saved retry audio ({})", why),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("could not delete {}: {}", path.display(), e),
    }
}

/// Why a transcription produced no text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RetryReason {
    /// Deepgram accepted the audio, answered 200, and returned an empty string.
    EmptyTranscript,
    RequestFailed,
    MissingApiKey,
}

impl RetryReason {
    /// Shown on the sticky banner alongside the Retry button.
    fn offer_message(self, duration_secs: f64) -> String {
        let length = format_clock_duration(duration_secs);
        match self {
            RetryReason::EmptyTranscript => format!(
                "Deepgram sent back an empty transcript for your {} recording. The audio is saved — send it again?",
                length
            ),
            RetryReason::RequestFailed => format!(
                "Couldn't reach Deepgram, so your {} recording wasn't transcribed. The audio is saved — send it again?",
                length
            ),
            RetryReason::MissingApiKey => format!(
                "No API key set, so your {} recording was never sent. The audio is saved — add a key above, then send it.",
                length
            ),
        }
    }

    /// Shown when no retry can be offered. These are the messages Voice Keys
    /// has always used, so behaviour is unchanged wherever the stash is
    /// unavailable.
    fn plain_message(self) -> &'static str {
        match self {
            RetryReason::EmptyTranscript => "No speech detected.",
            RetryReason::RequestFailed => "Transcription failed. Check log for details.",
            RetryReason::MissingApiKey => "Missing API key. Set it in settings above.",
        }
    }

    fn banner_kind(self) -> &'static str {
        match self {
            RetryReason::EmptyTranscript => "info",
            RetryReason::RequestFailed | RetryReason::MissingApiKey => "error",
        }
    }
}

/// `8m 05s`, or `47s` for anything under a minute.
fn format_clock_duration(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs.round() as u64
    } else {
        0
    };
    if total >= 60 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{}s", total)
    }
}

/// Where this transcription attempt sits in the retry story.
#[derive(Debug, Clone, Copy)]
struct RetryContext {
    /// The offer being re-sent, or `None` for a freshly recorded clip.
    offer_id: Option<u64>,
    /// Retries the user has already spent on this audio.
    retries_used: u32,
}

/// The event loop's view of the stashed recording: enough to re-send it, and
/// nothing else. The audio itself stays on disk.
#[derive(Debug, Clone, Copy)]
struct PendingRetry {
    offer_id: u64,
    mode: Mode,
    duration_secs: f64,
    retries_used: u32,
}

impl RetryContext {
    fn fresh() -> Self {
        Self {
            offer_id: None,
            retries_used: 0,
        }
    }
}

/// Park `wav` on disk and ask the event loop to offer the user a retry.
///
/// Returns false when no offer could be made, which leaves the caller to fall
/// back to a plain error banner. Nothing here is fatal: the transcription has
/// already failed, and a second banner about a disk problem helps nobody.
fn offer_retry(
    wav: &[u8],
    duration_secs: f64,
    mode: Mode,
    reason: RetryReason,
    retry: RetryContext,
    ui_proxy: &EventLoopProxy<UserEvent>,
) -> bool {
    let Some(path) = retry_audio_path() else {
        return false;
    };

    let guard = RETRY_STASH_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // A re-send that failed again is already on disk byte-for-byte; rewriting
    // 20-odd megabytes to say the same thing would be pure waste.
    let offer_id = match retry.offer_id {
        Some(id) => id,
        None => {
            if let Err(e) = fs::write(path, wav) {
                error!(
                    "could not save audio for retry at {}: {}",
                    path.display(),
                    e
                );
                return false;
            }
            info!(
                "audio saved for retry: {} ({:.1} KB, {:.1}s)",
                path.display(),
                wav.len() as f64 / 1024.0,
                duration_secs
            );
            NEXT_RETRY_OFFER_ID.fetch_add(1, Ordering::SeqCst)
        }
    };

    let sent = ui_proxy
        .send_event(UserEvent::RetryOffer {
            offer_id,
            message: reason.offer_message(duration_secs),
            mode,
            duration_secs,
            retries_used: retry.retries_used,
        })
        .is_ok();
    drop(guard);
    sent
}

/// Common landing point for every way a transcription can come back without
/// text. Offers a retry when one is available and useful, and otherwise says
/// plainly what went wrong.
fn handle_transcription_failure(
    wav: &[u8],
    duration_secs: f64,
    mode: Mode,
    reason: RetryReason,
    retry: RetryContext,
    ui_proxy: &EventLoopProxy<UserEvent>,
) {
    let exhausted = retry.retries_used >= MAX_USER_RETRIES;

    if !exhausted && offer_retry(wav, duration_secs, mode, reason, retry, ui_proxy) {
        notify("Voice Keys", reason.plain_message());
        complete_visual_cycle(ui_proxy);
        return;
    }

    // Either the stash is unavailable, or the user has spent every retry on
    // this clip. Say so instead of showing a button that would just buy another
    // request against the same silence.
    let message = if exhausted {
        let kept = retry_audio_path()
            .map(|p| format!(" The audio is still at {}.", p.display()))
            .unwrap_or_default();
        warn!(
            "giving up on this recording after {} retries; not offering another",
            retry.retries_used
        );
        format!(
            "Still nothing after {} retries — check that the right microphone is selected.{}",
            retry.retries_used, kept
        )
    } else {
        reason.plain_message().to_string()
    };

    send_banner(ui_proxy, reason.banner_kind(), &message);
    notify("Voice Keys", &message);

    // Tell the event loop the re-send is over so the Retry button unsticks,
    // but keep the file: it is the only copy of what they said.
    if let Some(offer_id) = retry.offer_id {
        let _ = ui_proxy.send_event(UserEvent::RetryFinished {
            offer_id,
            keep_audio: true,
        });
    }
    complete_visual_cycle(ui_proxy);
}

fn process(
    samples: Vec<i16>,
    sample_rate: u32,
    mode: Mode,
    dg_cfg: DeepgramConfig,
    ui_proxy: EventLoopProxy<UserEvent>,
) {
    let duration_secs = samples.len() as f64 / sample_rate as f64;
    if duration_secs < 0.3 {
        info!("recording too short ({:.1}s), skipping", duration_secs);
        complete_visual_cycle(&ui_proxy);
        return;
    }

    let wav = encode_wav(&samples, sample_rate);
    info!(
        "recorded {:.1}s, sending {:.1} KB to Deepgram",
        duration_secs,
        wav.len() as f64 / 1024.0
    );

    transcribe_and_deliver(
        wav,
        duration_secs,
        mode,
        dg_cfg,
        ui_proxy,
        RetryContext::fresh(),
    );
}

/// Send `wav` to Deepgram and act on whatever comes back.
///
/// Reached both by a freshly stopped recording and by the user re-sending
/// stashed audio; `retry` is what tells the two apart.
fn transcribe_and_deliver(
    wav: Vec<u8>,
    duration_secs: f64,
    mode: Mode,
    dg_cfg: DeepgramConfig,
    ui_proxy: EventLoopProxy<UserEvent>,
    retry: RetryContext,
) {
    send_banner(&ui_proxy, "processing", "Processing transcription...");

    let max_attempts = 2;
    let text = 'attempts: {
        for attempt in 1..=max_attempts {
            match transcribe(&wav, &dg_cfg, duration_secs) {
                Ok(Some(t)) => break 'attempts t,
                Ok(None) => {
                    // Deepgram took the audio, answered 200, and had nothing to
                    // say about it. Logged rather than dropped in silence: this
                    // outcome and a dead worker thread used to leave an
                    // identical trace in voicekeys.log, which is to say none.
                    warn!(
                        "Deepgram returned an empty transcript for {:.1}s of audio (no speech detected)",
                        duration_secs
                    );
                    handle_transcription_failure(
                        &wav,
                        duration_secs,
                        mode,
                        RetryReason::EmptyTranscript,
                        retry,
                        &ui_proxy,
                    );
                    return;
                }
                Err(TranscribeError::MissingApiKey) => {
                    handle_transcription_failure(
                        &wav,
                        duration_secs,
                        mode,
                        RetryReason::MissingApiKey,
                        retry,
                        &ui_proxy,
                    );
                    return;
                }
                Err(TranscribeError::RequestFailed) => {
                    if attempt < max_attempts {
                        warn!(
                            "Transcription attempt {} failed, retrying in 2s...",
                            attempt
                        );
                        send_banner(&ui_proxy, "retry", "Request failed. Retrying...");
                        thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    handle_transcription_failure(
                        &wav,
                        duration_secs,
                        mode,
                        RetryReason::RequestFailed,
                        retry,
                        &ui_proxy,
                    );
                    return;
                }
            }
        }
        unreachable!()
    };

    // Only the length at the default level: voicekeys.log is what users attach to
    // bug reports and what the "Copy last 500 log lines" button puts on the
    // clipboard, and neither should carry what they dictated.
    info!("transcript received: {} chars", text.len());

    // The stashed copy has served its purpose.
    if let Some(offer_id) = retry.offer_id {
        info!("retry succeeded on attempt {}", retry.retries_used);
        let _ = ui_proxy.send_event(UserEvent::RetryFinished {
            offer_id,
            keep_audio: false,
        });
    }
    debug!("transcript: {}", truncate_chars(&text, 120));
    send_banner(&ui_proxy, "success", "Transcription complete.");
    let _ = ui_proxy.send_event(UserEvent::TranscriptCompleted(text.clone()));

    match mode {
        Mode::Paste => {
            if let Ok(mut clip) = arboard::Clipboard::new() {
                if let Err(e) = clip.set_text(&text) {
                    error!("clipboard set failed: {}", e);
                    complete_visual_cycle(&ui_proxy);
                    return;
                }
            } else {
                error!("could not open clipboard");
                complete_visual_cycle(&ui_proxy);
                return;
            }
            thread::sleep(Duration::from_millis(60));
            if simulate_paste() {
                notify(
                    "Voice Keys",
                    "Transcription complete. Pasted to active app.",
                );
            } else {
                let paste_shortcut = if cfg!(target_os = "macos") {
                    "Cmd+V"
                } else {
                    "Ctrl+V"
                };
                warn!("paste keystroke failed; clipboard still contains transcript");
                notify(
                    "Voice Keys",
                    &format!(
                        "Transcription copied. Paste with {} in your active app.",
                        paste_shortcut
                    ),
                );
            }
        }
        Mode::Clipboard => {
            if let Ok(mut clip) = arboard::Clipboard::new() {
                if let Err(e) = clip.set_text(&text) {
                    error!("clipboard set failed: {}", e);
                    complete_visual_cycle(&ui_proxy);
                    return;
                }
            }
            let preview = if text.chars().count() <= 80 {
                text.clone()
            } else {
                format!("{}...", truncate_chars(&text, 77))
            };
            notify("Voice Keys", &format!("Copied: {}", preview));
        }
    }

    complete_visual_cycle(&ui_proxy);
}

/// Name of the OS default input device, without opening a stream on it.
///
/// Used to fill in the header line before the first recording, where
/// `setup_audio` has not run yet and so nothing else knows the device.
fn default_input_device_name() -> String {
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_else(|| "no input device".to_string())
}

fn setup_audio(
    target_sample_rate: u32,
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<i16>>>,
) -> (cpal::Stream, u32, String) {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("no audio input device found");

    let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
    info!("audio device: {}", device_name);

    let supported = device
        .supported_input_configs()
        .expect("failed to query audio configs");

    let mut chosen_config = None;
    for cfg in supported {
        if cfg.channels() == 1
            && cfg.sample_format() == cpal::SampleFormat::I16
            && cfg.min_sample_rate().0 <= target_sample_rate
            && cfg.max_sample_rate().0 >= target_sample_rate
        {
            chosen_config = Some(cfg.with_sample_rate(cpal::SampleRate(target_sample_rate)));
            break;
        }
    }

    let config = match chosen_config {
        Some(c) => c,
        None => {
            let default = device
                .default_input_config()
                .expect("no default input config");
            info!(
                "target {}Hz mono i16 not supported, using device default: {}Hz {}ch {:?}",
                target_sample_rate,
                default.sample_rate().0,
                default.channels(),
                default.sample_format(),
            );
            default
        }
    };

    let actual_rate = config.sample_rate().0;
    let channels = config.channels();
    let format = config.sample_format();

    info!(
        "audio config: {}Hz, {}ch, {:?}",
        actual_rate, channels, format
    );

    let stream = match format {
        cpal::SampleFormat::I16 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            device
                .build_input_stream(
                    &config.into(),
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if rec.load(Ordering::Relaxed) {
                            let mut b = buf.lock().unwrap();
                            if channels == 1 {
                                b.extend_from_slice(data);
                            } else {
                                for chunk in data.chunks(channels as usize) {
                                    b.push(chunk[0]);
                                }
                            }
                        }
                    },
                    |e| error!("audio stream error: {}", e),
                    None,
                )
                .expect("failed to build i16 input stream")
        }
        cpal::SampleFormat::F32 => {
            let rec = recording.clone();
            let buf = buffer.clone();
            device
                .build_input_stream(
                    &config.into(),
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if rec.load(Ordering::Relaxed) {
                            let mut b = buf.lock().unwrap();
                            if channels == 1 {
                                for &sample in data {
                                    b.push((sample * 32767.0).clamp(-32768.0, 32767.0) as i16);
                                }
                            } else {
                                for chunk in data.chunks(channels as usize) {
                                    let s = (chunk[0] * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                    b.push(s);
                                }
                            }
                        }
                    },
                    |e| error!("audio stream error: {}", e),
                    None,
                )
                .expect("failed to build f32 input stream")
        }
        other => panic!("unsupported audio sample format: {:?}", other),
    };

    stream.play().expect("failed to start audio stream");
    (stream, actual_rate, device_name)
}

fn default_config_yaml() -> String {
    let paste = default_paste_keys();
    let clipboard = default_clipboard_keys();
    let paste_modifier = paste.first().cloned().unwrap_or_else(|| "alt".into());
    let paste_trigger = paste.get(1).cloned().unwrap_or_else(|| "dot".into());
    let clipboard_modifier = clipboard.first().cloned().unwrap_or_else(|| "alt".into());
    let clipboard_trigger = clipboard.get(1).cloned().unwrap_or_else(|| "slash".into());

    format!(
        r#"# Voice Keys configuration

deepgram:
  api_key: ""
  model: "nova-3"
  language: "en"
  punctuate: true
  smart_format: true
  timeout_secs: 10.0

audio:
  sample_rate: 16000
  max_recording_minutes: 20

hotkeys:
  paste: ["{paste_modifier}", "{paste_trigger}"]
  clipboard: ["{clipboard_modifier}", "{clipboard_trigger}"]
"#
    )
}

fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = env::var("APPDATA") {
            return PathBuf::from(appdata).join("VoiceKeys");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(".config").join("voicekeys");
        }
    }
    // Fallback: next to the exe
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_config_path() -> PathBuf {
    // 1. Check next to the exe (portable mode)
    if let Ok(exe) = env::current_exe() {
        let beside_exe = exe.parent().unwrap().join("config.yaml");
        if beside_exe.exists() {
            return beside_exe;
        }
    }

    // 2. Check cwd
    let cwd = PathBuf::from("config.yaml");
    if cwd.exists() {
        return cwd;
    }

    // 3. Check app data dir
    let data_dir = app_data_dir();
    let in_data_dir = data_dir.join("config.yaml");
    if in_data_dir.exists() {
        return in_data_dir;
    }

    // 4. Create new config — try next to exe first, fall back to app data dir
    let target = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.yaml")))
        .unwrap_or_else(|| in_data_dir.clone());

    match fs::write(&target, default_config_yaml()) {
        Ok(()) => {
            eprintln!(
                "Created default config at {}. You can edit API key from the Voice Keys tray UI.",
                target.display()
            );
            return target;
        }
        Err(_) if target != in_data_dir => {
            // Exe directory not writable (e.g. Program Files), use app data dir
            let _ = fs::create_dir_all(&data_dir);
            if let Err(e) = fs::write(&in_data_dir, default_config_yaml()) {
                eprintln!(
                    "failed to write default config at {}: {}",
                    in_data_dir.display(),
                    e
                );
            } else {
                eprintln!(
                    "Created default config at {}. You can edit API key from the Voice Keys tray UI.",
                    in_data_dir.display()
                );
            }
            return in_data_dir;
        }
        Err(e) => {
            eprintln!(
                "failed to write default config at {}: {}",
                target.display(),
                e
            );
        }
    }

    target
}

fn load_config(path: &Path) -> Config {
    match fs::read_to_string(path) {
        Ok(text) => match serde_yaml::from_str::<Config>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                error!("invalid config.yaml at {}: {}", path.display(), e);
                Config::default()
            }
        },
        Err(e) => {
            error!("failed to read config {}: {}", path.display(), e);
            Config::default()
        }
    }
}

fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    let text = serde_yaml::to_string(cfg).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

fn log_path_for_config(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join("voicekeys.log")
}

fn transcript_history_path_for_config(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join("transcripts.txt")
}

fn retry_audio_path_for_config(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join("voicekeys-retry.wav")
}

/// Append one transcript to the running history file, creating it if needed.
///
/// Deliberately separate from `voicekeys.log`: this is a document the user opens
/// on purpose, whereas the log is what they paste into bug reports. Keeping the
/// two apart is what lets the log stay free of dictated text.
fn append_transcript_history(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let stamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    // Block form rather than one line per entry, so multi-line transcripts stay
    // readable in whatever the OS opens .txt with.
    writeln!(file, "[{}]\n{}\n", stamp, text.trim())
}

/// Create the history file if it doesn't exist yet, without writing an entry.
fn append_transcript_history_touch(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
}

/// Hand a path to the OS so it opens in the user's default application.
fn open_path_in_os(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` is a cmd builtin, not an executable; the empty "" is the window
        // title argument, which start would otherwise take from a quoted path.
        let mut c = process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut c = process::Command::new("xdg-open");
        c.arg(path);
        c
    };

    command.spawn().map(|_| ())
}

fn init_logging(log_path: &Path) {
    let log_dir = log_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = log_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("voicekeys");
    let _ = fs::create_dir_all(log_dir);

    Logger::try_with_env_or_str("info")
        .expect("logger init failed")
        .format(log_line_format)
        .log_to_file(
            FileSpec::default()
                .directory(log_dir)
                .basename(base)
                .suppress_timestamp(),
        )
        .duplicate_to_stdout(Duplicate::Info)
        .append()
        .start()
        .expect("failed to start logger");
}

/// `2026-07-31 00:40:12 INFO  [voicekeys] message`
///
/// Local time, second precision, no sub-second noise: the log is read by humans
/// pasting it into bug reports, and the questions it has to answer ("when did
/// this recording start", "how long was Deepgram silent for") are all at
/// human timescales. The level is padded so the `[module]` column lines up.
fn log_line_format(
    w: &mut dyn Write,
    now: &mut DeferredNow,
    record: &log::Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "{} {:<5} [{}] {}",
        now.format("%Y-%m-%d %H:%M:%S"),
        record.level(),
        record.module_path().unwrap_or("<unnamed>"),
        record.args()
    )
}

fn read_last_log_lines(log_path: &Path, max_lines: usize) -> Result<String, String> {
    if max_lines == 0 {
        return Ok(String::new());
    }

    let file = fs::File::open(log_path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let mut lines: VecDeque<String> = VecDeque::with_capacity(max_lines);

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| e.to_string())?;
        if lines.len() == max_lines {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    Ok(lines.into_iter().collect::<Vec<String>>().join("\n"))
}

#[derive(Debug)]
enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    Ipc(IpcCommand),
    VisualState(VisualState),
    StatusBanner {
        kind: String,
        message: String,
    },
    TranscriptCompleted(String),
    RecordingMinuteTick(u64),
    ClearTrayTemporaryMessage(u64),
    DeepgramUsageLoaded(Result<DeepgramUsageUiPayload, String>),
    DeepgramUsageHistoryLoaded(Result<DeepgramUsageHistoryPayload, String>),
    /// A transcription came back empty or failed, and its audio is stashed.
    RetryOffer {
        offer_id: u64,
        message: String,
        mode: Mode,
        duration_secs: f64,
        retries_used: u32,
    },
    /// A re-send finished, one way or another. `keep_audio` is false only once
    /// the stashed copy is genuinely no longer needed.
    RetryFinished {
        offer_id: u64,
        keep_audio: bool,
    },
    /// The input device Voice Keys is about to record from.
    AudioSourceChanged(String),
}

#[derive(Debug)]
enum IpcCommand {
    SaveDeepgramSettings {
        api_key: String,
        language: String,
        model: String,
        auto_stop_minutes: String,
    },
    RefreshDeepgramUsage,
    RefreshDeepgramUsageHistory,
    SaveHotkeys {
        paste_modifier: String,
        paste_trigger: String,
        clipboard_modifier: String,
        clipboard_trigger: String,
    },
    OpenLog,
    OpenTranscriptHistory,
    CopyTranscripts,
    CopyLastMessage,
    /// Re-send the stashed audio. Only ever arrives from a click on the retry
    /// banner — nothing in Voice Keys issues this on its own.
    RetryTranscription,
    DiscardRetry,
    OpenMacSetting(String),
    CopyLink(String),
    WindowDrag,
    WindowMinimize,
    WindowMaximizeToggle,
    WindowClose,
    Quit,
}

#[derive(Debug, Deserialize)]
struct IpcPayload {
    cmd: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    auto_stop_minutes: String,
    #[serde(default)]
    paste_modifier: String,
    #[serde(default)]
    paste_trigger: String,
    #[serde(default)]
    clipboard_modifier: String,
    #[serde(default)]
    clipboard_trigger: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    setting: String,
}

fn parse_ipc_command(raw: &str) -> Result<IpcCommand, String> {
    let payload: IpcPayload = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    match payload.cmd.as_str() {
        "save_api_key" => Ok(IpcCommand::SaveDeepgramSettings {
            api_key: payload.api_key.trim().to_string(),
            language: normalize_language_code(&payload.language),
            model: payload.model.trim().to_string(),
            auto_stop_minutes: payload.auto_stop_minutes.trim().to_string(),
        }),
        "refresh_usage" => Ok(IpcCommand::RefreshDeepgramUsage),
        "refresh_usage_history" => Ok(IpcCommand::RefreshDeepgramUsageHistory),
        "save_hotkeys" => Ok(IpcCommand::SaveHotkeys {
            paste_modifier: payload.paste_modifier.trim().to_string(),
            paste_trigger: payload.paste_trigger.trim().to_string(),
            clipboard_modifier: payload.clipboard_modifier.trim().to_string(),
            clipboard_trigger: payload.clipboard_trigger.trim().to_string(),
        }),
        "open_log" => Ok(IpcCommand::OpenLog),
        "open_transcript_history" => Ok(IpcCommand::OpenTranscriptHistory),
        "copy_transcripts" => Ok(IpcCommand::CopyTranscripts),
        "copy_last_message" => Ok(IpcCommand::CopyLastMessage),
        "retry_transcription" => Ok(IpcCommand::RetryTranscription),
        "discard_retry" => Ok(IpcCommand::DiscardRetry),
        "open_mac_setting" => Ok(IpcCommand::OpenMacSetting(
            payload.setting.trim().to_string(),
        )),
        "copy_link" => Ok(IpcCommand::CopyLink(payload.url.trim().to_string())),
        "window_drag" => Ok(IpcCommand::WindowDrag),
        "window_minimize" => Ok(IpcCommand::WindowMinimize),
        "window_maximize_toggle" => Ok(IpcCommand::WindowMaximizeToggle),
        "window_close" => Ok(IpcCommand::WindowClose),
        "quit" => Ok(IpcCommand::Quit),
        _ => Err(format!("unknown IPC command: {}", payload.cmd)),
    }
}

/// Clean up a hand-typed language code.
///
/// People paste `'en'`, `"en"`, or `en` interchangeably, so surrounding quotes are
/// stripped, then each hyphen-separated subtag is put into its conventional
/// BCP-47 casing: `'en'` -> `en`, `"EN"` -> `en`, `fr-ca` -> `fr-CA`,
/// `PT-br` -> `pt-BR`, `zh-hans` -> `zh-Hans`, `'multi'` -> `multi`.
fn normalize_language_code(raw: &str) -> String {
    let trimmed = raw.trim();

    // Strip one matching quote pair, so a legitimate `pt-BR` is left alone.
    let quote_pairs: [(char, char); 6] = [
        ('\'', '\''),
        ('"', '"'),
        ('`', '`'),
        ('\u{2018}', '\u{2019}'), // ‘ ’
        ('\u{201C}', '\u{201D}'), // “ ”
        ('\u{00AB}', '\u{00BB}'), // « »
    ];
    let mut unquoted = trimmed;
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2 {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        for (open, close) in quote_pairs {
            if first == open && last == close {
                let start = open.len_utf8();
                let end = trimmed.len() - close.len_utf8();
                unquoted = trimmed[start..end].trim();
                break;
            }
        }
    }

    unquoted
        .split('-')
        .enumerate()
        .map(|(idx, part)| {
            if idx == 0 {
                return part.to_ascii_lowercase();
            }
            let is_alpha = !part.is_empty() && part.chars().all(|c| c.is_ascii_alphabetic());
            match part.len() {
                // Region subtag: en-US, pt-BR
                2 if is_alpha => part.to_ascii_uppercase(),
                // Script subtag: zh-Hans
                4 if is_alpha => {
                    let mut out = String::with_capacity(4);
                    for (i, c) in part.chars().enumerate() {
                        if i == 0 {
                            out.extend(c.to_uppercase());
                        } else {
                            out.extend(c.to_lowercase());
                        }
                    }
                    out
                }
                // Numeric or extended subtags (es-419) pass through untouched.
                _ => part.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Loose sanity check so obvious junk is refused before it breaks every future
/// transcription. Accepts `multi`, `en`, `pt-BR`, `es-419`, `zh-Hans`.
fn looks_like_language_code(code: &str) -> bool {
    // Deepgram's multilingual model is not a BCP-47 tag, so allow it by name.
    if code == "multi" {
        return true;
    }

    let mut parts = code.split('-');
    let primary = match parts.next() {
        Some(p) => p,
        None => return false,
    };
    if !(2..=3).contains(&primary.len()) || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|part| {
        (2..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const UI_HTML_TEMPLATE: &str = include_str!("../ui/index.html");
const UI_CSS: &str = include_str!("../ui/styles.css");

fn build_gui_html(
    api_key: &str,
    language: &str,
    model: &str,
    max_recording_minutes: u32,
    paste_hotkey: &[String],
    clipboard_hotkey: &[String],
    audio_source: &str,
) -> String {
    // Full name on hover, cut-down name on screen.
    let audio_source_label = html_escape(&ellipsize(audio_source, MAX_AUDIO_SOURCE_CHARS));
    let audio_source = html_escape(audio_source);
    let api_key = html_escape(api_key);
    let language = html_escape(language);
    // Show the model that will actually be used, so a stale nova-2 in config
    // alongside language=multi doesn't render a dropdown that disagrees with reality.
    let model = html_escape(&normalize_model(model, language.as_str()));
    let max_recording_minutes = max_recording_minutes.to_string();
    let (paste_modifier, paste_trigger) = combo_fields_for_ui(paste_hotkey);
    let (clipboard_modifier, clipboard_trigger) = combo_fields_for_ui(clipboard_hotkey);
    let paste_modifier = html_escape(&paste_modifier);
    let paste_trigger = html_escape(&paste_trigger);
    let clipboard_modifier = html_escape(&clipboard_modifier);
    let clipboard_trigger = html_escape(&clipboard_trigger);

    UI_HTML_TEMPLATE
        .replace("{{CSS}}", UI_CSS)
        .replace("{{API_KEY}}", &api_key)
        .replace("{{LANGUAGE}}", &language)
        .replace("{{MODEL}}", &model)
        .replace("{{MAX_RECORDING_MINUTES}}", &max_recording_minutes)
        .replace("{{PASTE_MODIFIER}}", &paste_modifier)
        .replace("{{PASTE_TRIGGER}}", &paste_trigger)
        .replace("{{CLIPBOARD_MODIFIER}}", &clipboard_modifier)
        .replace("{{CLIPBOARD_TRIGGER}}", &clipboard_trigger)
        .replace("{{AUDIO_SOURCE_LABEL}}", &audio_source_label)
        .replace("{{AUDIO_SOURCE}}", &audio_source)
        // The bug report form asks people for their version and tells them to
        // read it off the app's window, so the window has to actually show it.
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
}

fn send_webview_event(webview: &wry::WebView, event_name: &str, detail: serde_json::Value) {
    let script = format!(
        "window.dispatchEvent(new CustomEvent('{}', {{ detail: {} }}));",
        event_name, detail
    );
    if let Err(e) = webview.evaluate_script(&script) {
        warn!("failed to send webview event {}: {}", event_name, e);
    }
}

fn send_webview_status(webview: &wry::WebView, kind: &str, message: &str) {
    let payload = serde_json::json!({ "kind": kind, "message": message });
    send_webview_event(webview, "voicekeys-status", payload);
}

fn send_webview_usage(webview: &wry::WebView, result: Result<DeepgramUsageUiPayload, String>) {
    match result {
        Ok(payload) => {
            let detail = serde_json::json!({
                "state": "ok",
                "period": payload.period,
                "minutes_used": payload.minutes_used,
                "requests": payload.requests,
                "spend": payload.spend,
                "balance": payload.balance,
                "note": payload.note
            });
            send_webview_event(webview, "voicekeys-usage", detail);
        }
        Err(message) => {
            send_webview_event(
                webview,
                "voicekeys-usage",
                serde_json::json!({
                    "state": "error",
                    "message": message
                }),
            );
        }
    }
}

fn send_webview_usage_history(
    webview: &wry::WebView,
    result: Result<DeepgramUsageHistoryPayload, String>,
) {
    let detail = match result {
        Ok(payload) => serde_json::json!({
            "state": "ok",
            "range_label": payload.range_label,
            "total_minutes": payload.total_minutes,
            "total_requests": payload.total_requests,
            "total_spend": payload.total_spend,
            "months": payload.months,
            "note": payload.note
        }),
        Err(message) => serde_json::json!({
            "state": "error",
            "message": message
        }),
    };
    send_webview_event(webview, "voicekeys-usage-history", detail);
}

fn request_deepgram_usage_history_refresh(
    shared_cfg: Arc<RwLock<Config>>,
    usage_proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let api_key = match shared_cfg.read() {
            Ok(cfg) => cfg.deepgram.api_key.clone(),
            Err(_) => {
                let _ = usage_proxy.send_event(UserEvent::DeepgramUsageHistoryLoaded(Err(
                    "Unable to read app config while loading all-time usage.".to_string(),
                )));
                return;
            }
        };
        let result = fetch_deepgram_usage_history(&api_key);
        let _ = usage_proxy.send_event(UserEvent::DeepgramUsageHistoryLoaded(result));
    });
}

fn request_deepgram_usage_refresh(
    shared_cfg: Arc<RwLock<Config>>,
    usage_proxy: EventLoopProxy<UserEvent>,
) {
    thread::spawn(move || {
        let api_key = match shared_cfg.read() {
            Ok(cfg) => cfg.deepgram.api_key.clone(),
            Err(_) => {
                let _ = usage_proxy.send_event(UserEvent::DeepgramUsageLoaded(Err(
                    "Unable to read app config while refreshing Deepgram usage.".to_string(),
                )));
                return;
            }
        };
        let result = fetch_deepgram_usage_payload(&api_key);
        let _ = usage_proxy.send_event(UserEvent::DeepgramUsageLoaded(result));
    });
}

/// The foreground used for every glyph drawn onto the tray icon.
const ICON_FG: [u8; 4] = [245, 245, 245, 255];

fn set_icon_pixel(rgba: &mut [u8], width: u32, height: u32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let idx = ((y as u32 * width + x as u32) * 4) as usize;
    rgba[idx..idx + 4].copy_from_slice(&color);
}

fn make_tray_icon(state: VisualState) -> Icon {
    let width: u32 = 32;
    let height: u32 = 32;
    let mut rgba = vec![0_u8; (width * height * 4) as usize];

    let (bg_r, bg_g, bg_b) = match state {
        VisualState::Idle => (10, 120, 82),
        VisualState::Recording => (186, 46, 46),
        VisualState::Processing => (183, 132, 32),
        VisualState::Finished => (27, 132, 90),
    };

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let in_round_rect = x > 2 && x < width - 3 && y > 2 && y < height - 3;
            if in_round_rect {
                rgba[idx] = bg_r;
                rgba[idx + 1] = bg_g;
                rgba[idx + 2] = bg_b;
                rgba[idx + 3] = 255;
            }
        }
    }

    match state {
        VisualState::Idle => {
            for y in 0..height {
                for x in 0..width {
                    let idx = ((y * width + x) * 4) as usize;
                    if (x > 8 && x < 12 && y > 10 && y < 22)
                        || (x > 14 && x < 18 && y > 8 && y < 24)
                        || (x > 20 && x < 24 && y > 12 && y < 20)
                    {
                        rgba[idx] = 236;
                        rgba[idx + 1] = 252;
                        rgba[idx + 2] = 242;
                        rgba[idx + 3] = 255;
                    }
                }
            }
        }
        VisualState::Recording => {
            let cx = 16_i32;
            let cy = 16_i32;
            let radius = 7_i32;
            for y in (cy - radius)..=(cy + radius) {
                for x in (cx - radius)..=(cx + radius) {
                    let dx = x - cx;
                    let dy = y - cy;
                    if dx * dx + dy * dy <= radius * radius {
                        set_icon_pixel(&mut rgba, width, height, x, y, ICON_FG);
                    }
                }
            }
        }
        VisualState::Processing => {
            for center_x in [10_i32, 16_i32, 22_i32] {
                for y in 12_i32..=20_i32 {
                    for x in (center_x - 3)..=(center_x + 3) {
                        let dx = x - center_x;
                        let dy = y - 16_i32;
                        if dx * dx + dy * dy <= 8 {
                            set_icon_pixel(&mut rgba, width, height, x, y, ICON_FG);
                        }
                    }
                }
            }
        }
        VisualState::Finished => {
            for i in 0_i32..6_i32 {
                set_icon_pixel(&mut rgba, width, height, 8 + i, 15 + i, ICON_FG);
                set_icon_pixel(&mut rgba, width, height, 9 + i, 15 + i, ICON_FG);
            }
            for i in 0_i32..10_i32 {
                set_icon_pixel(&mut rgba, width, height, 13 + i, 20 - i, ICON_FG);
                set_icon_pixel(&mut rgba, width, height, 14 + i, 20 - i, ICON_FG);
            }
        }
    }

    Icon::from_rgba(rgba, width, height).expect("failed to create tray icon")
}

const TRAY_TOOLTIP_BASE: &str = "Voice Keys";

// Only the macOS menu-bar title and the Windows tray tooltip surface elapsed
// recording time; the Linux tray has nowhere to put it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn minute_label(minutes: u64) -> String {
    if minutes == 1 {
        "1 min".into()
    } else {
        format!("{} min", minutes)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn recording_elapsed_label(elapsed_minutes: Option<u64>) -> String {
    match elapsed_minutes {
        Some(minutes) => minute_label(minutes),
        None => String::new(),
    }
}

fn update_tray_elapsed_display(
    icon: &TrayIcon,
    is_recording: bool,
    elapsed_minutes: Option<u64>,
    temporary_message: Option<&str>,
) {
    #[cfg(target_os = "macos")]
    {
        if is_recording {
            let title = temporary_message
                .map(str::to_string)
                .unwrap_or_else(|| recording_elapsed_label(elapsed_minutes));
            icon.set_title(Some(title));
        } else {
            // tray-icon 0.21 does not clear macOS title on None, so use empty string.
            icon.set_title(Some(String::new()));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let tooltip = if is_recording {
            let text = temporary_message
                .map(str::to_string)
                .unwrap_or_else(|| recording_elapsed_label(elapsed_minutes));
            format!("{} ({})", TRAY_TOOLTIP_BASE, text)
        } else {
            TRAY_TOOLTIP_BASE.to_string()
        };
        if let Err(e) = icon.set_tooltip(Some(tooltip.as_str())) {
            error!("failed to update tray tooltip: {}", e);
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (icon, is_recording, elapsed_minutes, temporary_message);
    }
}

fn show_window(window: &tao::window::Window) {
    window.set_minimized(false);
    window.set_visible(true);
    window.set_focus();
}

#[cfg(target_os = "windows")]
fn hide_window(window: &tao::window::Window) {
    // On Windows keep the app in the taskbar so elapsed text is visible there.
    window.set_visible(true);
    window.set_minimized(true);
}

#[cfg(not(target_os = "windows"))]
fn hide_window(window: &tao::window::Window) {
    window.set_visible(false);
}

fn update_windows_taskbar_display(
    window: &tao::window::Window,
    is_recording: bool,
    elapsed_minutes: Option<u64>,
    temporary_message: Option<&str>,
) {
    #[cfg(target_os = "windows")]
    {
        let title = if is_recording {
            let text = temporary_message
                .map(str::to_string)
                .unwrap_or_else(|| recording_elapsed_label(elapsed_minutes));
            format!("Voice Keys - {}", text)
        } else {
            "Voice Keys".to_string()
        };
        window.set_title(&title);
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, is_recording, elapsed_minutes, temporary_message);
    }
}

/// Shared handles to the live recording state.
///
/// These are created in `main()` so both the key-listener daemon thread and the
/// tao event loop (which owns the tray menu) can act on the current recording.
#[derive(Clone)]
struct RecordingHandles {
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<i16>>>,
    active_mode: Arc<Mutex<Option<Mode>>>,
}

impl RecordingHandles {
    fn new() -> Self {
        Self {
            recording: Arc::new(AtomicBool::new(false)),
            buffer: Arc::new(Mutex::new(Vec::new())),
            active_mode: Arc::new(Mutex::new(None)),
        }
    }
}

/// Throw the in-progress recording away without transcribing it.
///
/// Deliberately stops short of `process()` so nothing is sent to Deepgram — the
/// user cancelled because they intend to start over. Returns false when no
/// recording was in progress.
fn cancel_recording(handles: &RecordingHandles, ui_proxy: &EventLoopProxy<UserEvent>) -> bool {
    if !handles.recording.swap(false, Ordering::SeqCst) {
        return false;
    }

    // The minute-tick timer breaks on its own once `recording` is false, and the
    // auto-stop-cap thread bails out of stop_recording_and_transcribe for the
    // same reason, so neither needs to be signalled separately.
    let discarded = {
        let mut active = handles.active_mode.lock().unwrap();
        active.take();
        let mut b = handles.buffer.lock().unwrap();
        let len = b.len();
        b.clear();
        len
    };

    info!(
        "recording cancelled (tray menu), discarded {} samples",
        discarded
    );
    notify("Voice Keys", "Recording cancelled");
    send_banner(ui_proxy, "info", "Recording cancelled.");
    // Straight to Idle rather than complete_visual_cycle(): the green "Finished"
    // flash would read as a successful transcription.
    send_visual_state(ui_proxy, VisualState::Idle);
    true
}

fn stop_recording_and_transcribe(
    recording: &Arc<AtomicBool>,
    active_mode: &Arc<Mutex<Option<Mode>>>,
    buffer: &Arc<Mutex<Vec<i16>>>,
    sample_rate: u32,
    shared_cfg: &Arc<RwLock<Config>>,
    ui_proxy: &EventLoopProxy<UserEvent>,
    reason: &str,
) -> bool {
    if !recording.swap(false, Ordering::SeqCst) {
        return false;
    }

    let mode = {
        let mut active = active_mode.lock().unwrap();
        active.take()
    };

    let mode = match mode {
        Some(mode) => mode,
        None => {
            warn!("recording stopped ({}) but active mode was missing", reason);
            complete_visual_cycle(ui_proxy);
            return false;
        }
    };

    let samples: Vec<i16> = {
        let mut b = buffer.lock().unwrap();
        std::mem::take(&mut *b)
    };

    info!("recording stopped ({}), {} samples", reason, samples.len());
    notify("Voice Keys", "Recording OFF");
    notify("Voice Keys", "Processing transcription...");
    send_visual_state(ui_proxy, VisualState::Processing);

    let dg_cfg = match shared_cfg.read() {
        Ok(c) => c.deepgram.clone(),
        Err(_) => {
            error!("failed to read config after recording stop");
            complete_visual_cycle(ui_proxy);
            return false;
        }
    };

    let process_proxy = ui_proxy.clone();
    thread::spawn(move || {
        process(samples, sample_rate, mode, dg_cfg, process_proxy);
    });
    true
}

fn run_voice_daemon(
    config: Arc<RwLock<Config>>,
    ui_proxy: EventLoopProxy<UserEvent>,
    handles: RecordingHandles,
) {
    let startup_cfg = config.read().unwrap().clone();

    info!(
        "paste hotkey:     {}",
        combo_label(&startup_cfg.hotkeys.paste)
    );
    info!(
        "clipboard hotkey: {}",
        combo_label(&startup_cfg.hotkeys.clipboard)
    );

    let recording = Arc::clone(&handles.recording);
    let buffer: Arc<Mutex<Vec<i16>>> = Arc::clone(&handles.buffer);

    // Audio stream is (re)built on each recording start so that changes to
    // the OS default input device are picked up without restarting the app.
    info!("Voice Keys daemon running in background.");
    notify(
        "Voice Keys",
        "Running in background. Click tray/menu icon for settings.",
    );

    let active_mode: Arc<Mutex<Option<Mode>>> = Arc::clone(&handles.active_mode);
    let mut chord_latched: bool = false; // prevent retrigger while keys are held
    let mut hotkey_keys_down: HashSet<Key> = HashSet::new(); // current state for order-agnostic chord detection
    let recording_generation = Arc::new(AtomicU64::new(0));

    let rec = recording.clone();
    let buf = buffer.clone();
    let target_rate = startup_cfg.audio.sample_rate;
    let mut current_stream: Option<cpal::Stream> = None;
    let mut rate: u32 = target_rate;
    let active_mode_for_keys = Arc::clone(&active_mode);
    let shared_cfg = Arc::clone(&config);
    let generation = Arc::clone(&recording_generation);

    let mut handle_key = move |is_press: bool, raw_key: Key| {
        let key = normalize(raw_key);

        let cfg = match shared_cfg.read() {
            Ok(c) => c,
            Err(_) => return,
        };
        let max_recording_minutes = cfg.audio.max_recording_minutes;
        let paste_mod = cfg
            .hotkeys
            .paste
            .first()
            .and_then(|s| map_key(s))
            .map(normalize);
        let paste_trig = cfg
            .hotkeys
            .paste
            .get(1)
            .and_then(|s| map_key(s))
            .map(normalize);
        let clip_mod = cfg
            .hotkeys
            .clipboard
            .first()
            .and_then(|s| map_key(s))
            .map(normalize);
        let clip_trig = cfg
            .hotkeys
            .clipboard
            .get(1)
            .and_then(|s| map_key(s))
            .map(normalize);
        drop(cfg);

        let is_hotkey_key = Some(key) == paste_mod
            || Some(key) == paste_trig
            || Some(key) == clip_mod
            || Some(key) == clip_trig;

        // The mutex and the `format!` are both skipped unless diagnostics are on;
        // this runs on every hotkey keypress.
        #[cfg(target_os = "macos")]
        if is_hotkey_key && macos_keys::debug_enabled() {
            let active_mode_snapshot = {
                let active = active_mode_for_keys.lock().unwrap();
                *active
            };
            macos_keys::dlog(&format!(
                "hotkey event: {} key={:?} chord_latched={} active_mode={:?}",
                if is_press { "press" } else { "release" },
                key,
                chord_latched,
                active_mode_snapshot
            ));
        }

        if is_press {
            if is_hotkey_key {
                hotkey_keys_down.insert(key);
            }

            // Chord mode fast path: trigger when Key1 + Key2 are both down (either order).
            if !chord_latched {
                let paste_chord_down = match (paste_mod, paste_trig) {
                    (Some(modifier), Some(trigger)) => {
                        hotkey_keys_down.contains(&modifier) && hotkey_keys_down.contains(&trigger)
                    }
                    _ => false,
                };
                let clip_chord_down = match (clip_mod, clip_trig) {
                    (Some(modifier), Some(trigger)) => {
                        hotkey_keys_down.contains(&modifier) && hotkey_keys_down.contains(&trigger)
                    }
                    _ => false,
                };

                let chord_mode = if paste_chord_down {
                    Some(Mode::Paste)
                } else if clip_chord_down {
                    Some(Mode::Clipboard)
                } else {
                    None
                };

                if let Some(mode) = chord_mode {
                    chord_latched = true;

                    if !rec.load(Ordering::SeqCst) {
                        buf.lock().unwrap().clear();
                        // Drop any previous stream and build a fresh one so
                        // the current OS default input device is used.
                        drop(current_stream.take());
                        let (new_stream, new_rate, device_name) =
                            setup_audio(target_rate, rec.clone(), buf.clone());
                        rate = new_rate;
                        current_stream = Some(new_stream);
                        // The header line was filled in at launch; the default
                        // device may well have changed since.
                        let _ = ui_proxy.send_event(UserEvent::AudioSourceChanged(device_name));
                        // Touch to ensure the closure captures it by move.
                        let _ = current_stream.is_some();
                        rec.store(true, Ordering::SeqCst);
                        {
                            let mut active = active_mode_for_keys.lock().unwrap();
                            *active = Some(mode);
                        }
                        let session_id = generation.fetch_add(1, Ordering::SeqCst) + 1;
                        match mode {
                            Mode::Paste => info!("recording started (paste mode)"),
                            Mode::Clipboard => info!("recording started (clipboard mode)"),
                        }
                        notify("Voice Keys", "Recording ON");
                        send_visual_state(&ui_proxy, VisualState::Recording);
                        send_banner(&ui_proxy, "recording", "Recording...");

                        let timer_proxy = ui_proxy.clone();
                        let timer_rec = rec.clone();
                        let timer_generation = generation.clone();
                        thread::spawn(move || {
                            let mut minutes = 1_u64;
                            loop {
                                thread::sleep(Duration::from_secs(60));
                                if !timer_rec.load(Ordering::SeqCst) {
                                    break;
                                }
                                if timer_generation.load(Ordering::SeqCst) != session_id {
                                    break;
                                }
                                if timer_proxy
                                    .send_event(UserEvent::RecordingMinuteTick(minutes))
                                    .is_err()
                                {
                                    break;
                                }
                                minutes += 1;
                            }
                        });

                        if max_recording_minutes > 0 {
                            let cap_rec = rec.clone();
                            let cap_buf = buf.clone();
                            let cap_cfg = shared_cfg.clone();
                            let cap_generation = generation.clone();
                            let cap_active_mode = active_mode_for_keys.clone();
                            let cap_proxy = ui_proxy.clone();
                            thread::spawn(move || {
                                thread::sleep(Duration::from_secs(
                                    (max_recording_minutes as u64) * 60,
                                ));
                                if cap_generation.load(Ordering::SeqCst) != session_id {
                                    return;
                                }
                                if stop_recording_and_transcribe(
                                    &cap_rec,
                                    &cap_active_mode,
                                    &cap_buf,
                                    rate,
                                    &cap_cfg,
                                    &cap_proxy,
                                    "auto-stop cap reached",
                                ) {
                                    notify(
                                        "Voice Keys",
                                        &format!(
                                            "Reached {} minute cap. Stopped and started transcription.",
                                            max_recording_minutes
                                        ),
                                    );
                                }
                            });
                        }
                    } else {
                        let should_stop = {
                            let active = active_mode_for_keys.lock().unwrap();
                            *active == Some(mode)
                        };
                        if should_stop {
                            let _ = stop_recording_and_transcribe(
                                &rec,
                                &active_mode_for_keys,
                                &buf,
                                rate,
                                &shared_cfg,
                                &ui_proxy,
                                "manual hotkey",
                            );
                            // Release the input device so a device change
                            // (or the same device being reopened) works on
                            // the next recording.
                            drop(current_stream.take());
                        }
                    }
                }
            }
        } else {
            if is_hotkey_key {
                hotkey_keys_down.remove(&key);
                chord_latched = false;
            }
        }
    };

    // On macOS, use our own CGEventTap listener instead of rdev::listen.
    // rdev calls TISGetInputSourceProperty from a background thread, which
    // newer macOS versions forbid → dispatch_assert_queue → SIGTRAP crash.
    #[cfg(target_os = "macos")]
    {
        macos_keys::listen(move |ev| match ev {
            macos_keys::KeyEvent::Press(k) => handle_key(true, k),
            macos_keys::KeyEvent::Release(k) => handle_key(false, k),
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        let callback = move |event: rdev::Event| match event.event_type {
            EventType::KeyPress(k) => handle_key(true, k),
            EventType::KeyRelease(k) => handle_key(false, k),
            _ => {}
        };
        if let Err(e) = rdev::listen(callback) {
            error!("key listener failed: {:?}", e);
        }
    }
}

/// On macOS, accessory apps lack a default Edit menu.  Without one,
/// standard shortcuts (Cmd+A / C / V / X / Z) have no responder and
/// the process receives SIGTRAP.  Creating a minimal Edit menu fixes this.
#[cfg(target_os = "macos")]
fn setup_macos_edit_menu() {
    use objc2::sel;
    use objc2::MainThreadOnly;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;

    let mtm = objc2::MainThreadMarker::new().expect("must be called on main thread");

    unsafe {
        let app = NSApplication::sharedApplication(mtm);

        let menu_bar = NSMenu::new(mtm);

        // App menu (required as first item)
        let app_menu_item = NSMenuItem::new(mtm);
        let app_menu = NSMenu::new(mtm);
        app_menu_item.setSubmenu(Some(&app_menu));
        menu_bar.addItem(&app_menu_item);

        // Edit menu
        let edit_menu_item = NSMenuItem::new(mtm);
        let edit_title = NSString::from_str("Edit");
        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &edit_title);

        let items: &[(&str, objc2::runtime::Sel, &str)] = &[
            ("Undo", sel!(undo:), "z"),
            ("Cut", sel!(cut:), "x"),
            ("Copy", sel!(copy:), "c"),
            ("Paste", sel!(paste:), "v"),
            ("Select All", sel!(selectAll:), "a"),
        ];

        for &(title, action, key) in items {
            let ns_title = NSString::from_str(title);
            let ns_key = NSString::from_str(key);
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns_title,
                Some(action),
                &ns_key,
            );
            edit_menu.addItem(&item);
        }

        edit_menu_item.setSubmenu(Some(&edit_menu));
        menu_bar.addItem(&edit_menu_item);

        app.setMainMenu(Some(&menu_bar));
    }
}

#[cfg(target_os = "macos")]
fn configure_macos_app_identity() {
    use objc2_foundation::{NSProcessInfo, NSString};

    let process_name = NSString::from_str("Voice Keys");
    let process_info = NSProcessInfo::processInfo();
    process_info.setProcessName(&process_name);
}

fn main() {
    #[cfg(target_os = "macos")]
    configure_macos_app_identity();

    let config_path = resolve_config_path();
    let log_path = log_path_for_config(&config_path);
    let transcript_history_path = transcript_history_path_for_config(&config_path);
    init_logging(&log_path);

    info!("loading config from {}", config_path.display());
    info!("writing logs to {}", log_path.display());
    info!(
        "transcript history at {}",
        transcript_history_path.display()
    );

    let retry_audio_file = retry_audio_path_for_config(&config_path);
    // Left over from a previous run that failed and was never resolved. It is
    // not deleted here — it may be the only copy of something the user said —
    // but it is called out so it never sits there unnoticed. There can only
    // ever be one, and the next failure overwrites it.
    if let Ok(meta) = fs::metadata(&retry_audio_file) {
        warn!(
            "a previous recording is still saved at {} ({:.1} KB); it will be replaced by the next failed transcription",
            retry_audio_file.display(),
            meta.len() as f64 / 1024.0
        );
    }
    let _ = RETRY_AUDIO_PATH.set(retry_audio_file);

    let initial_cfg = load_config(&config_path);
    if initial_cfg.deepgram.api_key.trim().is_empty() {
        warn!("Deepgram API key is currently empty; set it from tray UI.");
    }

    let shared_cfg = Arc::new(RwLock::new(initial_cfg));

    #[cfg(target_os = "macos")]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "windows")]
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    #[cfg(target_os = "macos")]
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    #[cfg(target_os = "windows")]
    event_loop.set_theme(Some(TaoTheme::Light));

    // Owned here (not inside the daemon) so the tray menu can cancel a recording.
    let recording_handles = RecordingHandles::new();

    let daemon_proxy = event_loop.create_proxy();
    let daemon_cfg = Arc::clone(&shared_cfg);
    let daemon_handles = recording_handles.clone();
    thread::spawn(move || {
        run_voice_daemon(daemon_cfg, daemon_proxy, daemon_handles);
    });

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Tray(event));
    }));

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));
    let tray_reset_proxy = event_loop.create_proxy();
    let usage_refresh_proxy = event_loop.create_proxy();
    let usage_history_proxy = event_loop.create_proxy();
    let cancel_ui_proxy = event_loop.create_proxy();

    let window_builder = WindowBuilder::new()
        .with_title("Voice Keys")
        .with_decorations(false)
        .with_visible(false)
        .with_theme(Some(TaoTheme::Light))
        .with_inner_size(LogicalSize::new(560.0, 800.0))
        .with_resizable(true);

    // macOS only: a transparent window is what lets the rounded corners actually
    // show. Without it the opaque square window backing paints straight through
    // the clipped corners. Left off elsewhere to avoid regressing those builds —
    // there the body's radius is white-on-white, i.e. visually unchanged.
    #[cfg(target_os = "macos")]
    let window_builder = window_builder.with_transparent(true);

    #[cfg(target_os = "windows")]
    let window_builder = window_builder.with_skip_taskbar(true);

    let window = window_builder
        .build(&event_loop)
        .expect("failed to create app window");
    window.set_theme(Some(TaoTheme::Light));

    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowExtWindows;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
        };
        let hwnd = windows::Win32::Foundation::HWND(window.hwnd() as *mut _);
        let preference = 2u32; // DWMWCP_ROUND
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    let initial_cfg_for_ui = shared_cfg.read().unwrap().clone();
    let html = build_gui_html(
        &initial_cfg_for_ui.deepgram.api_key,
        &initial_cfg_for_ui.deepgram.language,
        &initial_cfg_for_ui.deepgram.model,
        initial_cfg_for_ui.audio.max_recording_minutes,
        &initial_cfg_for_ui.hotkeys.paste,
        &initial_cfg_for_ui.hotkeys.clipboard,
        &default_input_device_name(),
    );

    let ipc_proxy = event_loop.create_proxy();
    let webview_builder = WebViewBuilder::new()
        .with_html(&html)
        .with_navigation_handler(|url| {
            let normalized = url.trim().to_ascii_lowercase();
            !(normalized.starts_with("http://") || normalized.starts_with("https://"))
        })
        .with_ipc_handler(move |request| {
            if let Ok(cmd) = parse_ipc_command(request.body()) {
                let _ = ipc_proxy.send_event(UserEvent::Ipc(cmd));
            }
        });

    // Pairs with the transparent window above so the CSS corner radius is visible.
    #[cfg(target_os = "macos")]
    let webview_builder = webview_builder.with_transparent(true);

    #[cfg(target_os = "windows")]
    let webview_builder = webview_builder.with_theme(WebViewTheme::Light);

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = webview_builder
        .build(&window)
        .expect("failed to create webview");

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().expect("missing gtk vbox");
        webview_builder
            .build_gtk(vbox)
            .expect("failed to create webview")
    };

    // Native rounded corners, applied AFTER the webview exists: wry can swap the
    // window's content view while building, which would discard settings made on
    // whichever view was there before.
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWindow as NSWindowClass;
        use tao::platform::macos::WindowExtMacOS;
        unsafe {
            let ns_window_ptr = window.ns_window() as *mut NSWindowClass;
            let ns_window: &NSWindowClass = &*ns_window_ptr;
            if let Some(content_view) = ns_window.contentView() {
                content_view.setWantsLayer(true);
                if let Some(layer) = content_view.layer() {
                    // Keep in sync with --pg-app-radius in ui/styles.css.
                    layer.setCornerRadius(20.0);
                    layer.setMasksToBounds(true);
                }
            }
        }
    }

    let mut tray_icon = None;
    let mut open_menu_id: Option<MenuId> = None;
    let mut quit_menu_id: Option<MenuId> = None;
    let mut cancel_menu_id: Option<MenuId> = None;
    // Kept alive so "Cancel recording" can be inserted/removed as recording starts and stops.
    let mut tray_menu_handle: Option<Menu> = None;
    let mut cancel_item: Option<MenuItem> = None;
    let mut cancel_item_in_menu = false;
    let mut tray_is_recording = false;
    let mut tray_elapsed_minutes: Option<u64> = None;
    let mut tray_temporary_message: Option<String> = None;
    let mut tray_temp_message_generation: u64 = 0;
    let mut last_nudge_index: Option<usize> = None;
    let mut recent_transcripts: std::collections::VecDeque<String> =
        std::collections::VecDeque::new();
    // The one recording currently eligible for a re-send, if any. A single slot
    // by design: it mirrors the single stashed WAV on disk.
    let mut pending_retry: Option<PendingRetry> = None;
    // True from the moment a re-send is dispatched until its outcome lands, so
    // a double-click cannot buy two Deepgram requests. Only the event loop
    // touches it, so a plain bool is enough.
    let mut retry_in_flight = false;
    let retry_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                #[cfg(target_os = "macos")]
                setup_macos_edit_menu();
                #[cfg(target_os = "windows")]
                {
                    // Start minimized so there's always a taskbar entry for timer visibility.
                    window.set_visible(true);
                    window.set_minimized(true);
                }

                let tray_menu = Menu::new();
                let open_item = MenuItem::new("Open Voice Keys", true, None);
                let quit_item = MenuItem::new("Quit", true, None);
                // Not appended yet: the app starts idle and this item only shows
                // while a recording is in progress.
                let cancel_recording_item = MenuItem::new("Cancel recording", true, None);

                if let Err(e) = tray_menu.append(&open_item) {
                    error!("failed to add tray menu item: {}", e);
                }
                if let Err(e) = tray_menu.append(&quit_item) {
                    error!("failed to add tray menu item: {}", e);
                }

                open_menu_id = Some(open_item.id().clone());
                quit_menu_id = Some(quit_item.id().clone());
                cancel_menu_id = Some(cancel_recording_item.id().clone());
                cancel_item = Some(cancel_recording_item);
                cancel_item_in_menu = false;
                tray_menu_handle = Some(tray_menu.clone());

                match TrayIconBuilder::new()
                    .with_menu(Box::new(tray_menu))
                    .with_menu_on_left_click(false)
                    .with_icon(make_tray_icon(VisualState::Idle))
                    .with_tooltip(TRAY_TOOLTIP_BASE)
                    .build()
                {
                    Ok(icon) => {
                        update_tray_elapsed_display(
                            &icon,
                            tray_is_recording,
                            tray_elapsed_minutes,
                            tray_temporary_message.as_deref(),
                        );
                        tray_icon = Some(icon);
                        info!("tray icon initialized");
                    }
                    Err(e) => error!("failed to create tray icon: {}", e),
                }

                update_windows_taskbar_display(
                    &window,
                    tray_is_recording,
                    tray_elapsed_minutes,
                    tray_temporary_message.as_deref(),
                );
            }

            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                hide_window(&window);
            }

            Event::UserEvent(UserEvent::VisualState(state)) => {
                tray_is_recording = state == VisualState::Recording;
                if !tray_is_recording {
                    tray_elapsed_minutes = None;
                    tray_temporary_message = None;
                }

                // "Cancel recording" only appears while recording.
                if let (Some(menu), Some(item)) = (&tray_menu_handle, &cancel_item) {
                    if tray_is_recording && !cancel_item_in_menu {
                        // Position 1: between "Open Voice Keys" and "Quit".
                        match menu.insert(item, 1) {
                            Ok(_) => cancel_item_in_menu = true,
                            Err(e) => error!("failed to show cancel menu item: {}", e),
                        }
                    } else if !tray_is_recording && cancel_item_in_menu {
                        match menu.remove(item) {
                            Ok(_) => cancel_item_in_menu = false,
                            Err(e) => error!("failed to hide cancel menu item: {}", e),
                        }
                    }
                }
                if let Some(icon) = tray_icon.as_mut() {
                    if let Err(e) = icon.set_icon(Some(make_tray_icon(state))) {
                        error!("failed to update tray icon state: {}", e);
                    }
                    update_tray_elapsed_display(
                        icon,
                        tray_is_recording,
                        tray_elapsed_minutes,
                        tray_temporary_message.as_deref(),
                    );
                }
                update_windows_taskbar_display(
                    &window,
                    tray_is_recording,
                    tray_elapsed_minutes,
                    tray_temporary_message.as_deref(),
                );
            }

            Event::UserEvent(UserEvent::StatusBanner { kind, message }) => {
                let escaped_msg = message.replace('\\', "\\\\").replace('\'', "\\'");
                let escaped_kind = kind.replace('\\', "\\\\").replace('\'', "\\'");
                let script = format!(
                    "if(typeof updateBanner==='function')updateBanner('{}','{}');",
                    escaped_kind, escaped_msg
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    warn!("failed to send banner update: {}", e);
                }
            }

            Event::UserEvent(UserEvent::AudioSourceChanged(name)) => {
                send_webview_event(
                    &webview,
                    "voicekeys-audio-source",
                    serde_json::json!({
                        "label": ellipsize(&name, MAX_AUDIO_SOURCE_CHARS),
                        "name": name,
                    }),
                );
            }

            Event::UserEvent(UserEvent::RetryOffer {
                offer_id,
                message,
                mode,
                duration_secs,
                retries_used,
            }) => {
                retry_in_flight = false;
                pending_retry = Some(PendingRetry {
                    offer_id,
                    mode,
                    duration_secs,
                    retries_used,
                });
                send_webview_event(
                    &webview,
                    "voicekeys-retry-offer",
                    serde_json::json!({
                        "message": message,
                        "retries_left": MAX_USER_RETRIES.saturating_sub(retries_used),
                    }),
                );
            }

            Event::UserEvent(UserEvent::RetryFinished {
                offer_id,
                keep_audio,
            }) => {
                retry_in_flight = false;
                // Guard against a stale outcome clearing a newer offer: another
                // recording may have failed while this re-send was in flight.
                if pending_retry.map(|p| p.offer_id) == Some(offer_id) {
                    pending_retry = None;
                    send_webview_event(
                        &webview,
                        "voicekeys-retry-offer",
                        serde_json::json!({ "clear": true }),
                    );
                }
                if !keep_audio {
                    clear_retry_stash("no longer needed");
                }
            }

            Event::UserEvent(UserEvent::TranscriptCompleted(text)) => {
                if let Err(e) = append_transcript_history(&transcript_history_path, &text) {
                    // Never surface this: a full disk or a read-only config dir
                    // shouldn't interrupt a transcription that already succeeded.
                    warn!("failed to append to transcript history: {}", e);
                }
                if recent_transcripts.len() >= 50 {
                    recent_transcripts.pop_front();
                }
                recent_transcripts.push_back(text);
            }

            Event::UserEvent(UserEvent::RecordingMinuteTick(minutes)) => {
                if tray_is_recording {
                    tray_elapsed_minutes = Some(minutes);
                    if minutes % 4 == 0 {
                        let nudges: &[fn(u64) -> String] = &[
                            |m| format!("Hello you're at {} minutes!", m),
                            |m| format!("Yoyo you're at {} min!", m),
                            |m| format!("Fyi {} min.", m),
                            |m| format!("You got this! {} min.", m),
                            |m| format!("Rock on! {} min.", m),
                            |m| format!("Noice! {} min.", m),
                        ];
                        // Pick a random index, but never the same as last time
                        let seed = (minutes as u128)
                            .wrapping_mul(std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos());
                        let mut idx = (seed % nudges.len() as u128) as usize;
                        if Some(idx) == last_nudge_index {
                            idx = (idx + 1) % nudges.len();
                        }
                        last_nudge_index = Some(idx);
                        let message = nudges[idx](minutes);
                        tray_temporary_message = Some(message.clone());
                        tray_temp_message_generation = tray_temp_message_generation.wrapping_add(1);
                        let generation = tray_temp_message_generation;
                        let clear_proxy = tray_reset_proxy.clone();
                        thread::spawn(move || {
                            thread::sleep(Duration::from_secs(6));
                            let _ = clear_proxy.send_event(UserEvent::ClearTrayTemporaryMessage(
                                generation,
                            ));
                        });
                        notify(
                            "Voice Keys",
                            &message,
                        );
                    }
                    if let Some(icon) = tray_icon.as_mut() {
                        update_tray_elapsed_display(
                            icon,
                            tray_is_recording,
                            tray_elapsed_minutes,
                            tray_temporary_message.as_deref(),
                        );
                    }
                    update_windows_taskbar_display(
                        &window,
                        tray_is_recording,
                        tray_elapsed_minutes,
                        tray_temporary_message.as_deref(),
                    );
                }
            }

            Event::UserEvent(UserEvent::ClearTrayTemporaryMessage(generation)) => {
                if generation == tray_temp_message_generation {
                    tray_temporary_message = None;
                    if let Some(icon) = tray_icon.as_mut() {
                        update_tray_elapsed_display(
                            icon,
                            tray_is_recording,
                            tray_elapsed_minutes,
                            tray_temporary_message.as_deref(),
                        );
                    }
                    update_windows_taskbar_display(
                        &window,
                        tray_is_recording,
                        tray_elapsed_minutes,
                        tray_temporary_message.as_deref(),
                    );
                }
            }

            Event::UserEvent(UserEvent::DeepgramUsageLoaded(result)) => {
                send_webview_usage(&webview, result);
            }

            Event::UserEvent(UserEvent::DeepgramUsageHistoryLoaded(result)) => {
                send_webview_usage_history(&webview, result);
            }

            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            })) => {
                show_window(&window);
            }

            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                if let Some(id) = &open_menu_id {
                    if menu_event.id() == id {
                        show_window(&window);
                    }
                }

                if let Some(id) = &cancel_menu_id {
                    if menu_event.id() == id
                        && !cancel_recording(&recording_handles, &cancel_ui_proxy)
                    {
                        // Recording already ended between opening the menu and clicking.
                        send_webview_status(&webview, "ok", "No recording in progress.");
                    }
                }

                if let Some(id) = &quit_menu_id {
                    if menu_event.id() == id {
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }

            Event::UserEvent(UserEvent::Ipc(command)) => match command {
                IpcCommand::SaveDeepgramSettings {
                    api_key,
                    language,
                    model,
                    auto_stop_minutes,
                } => {
                    if language.trim().is_empty() {
                        send_webview_status(&webview, "error", "Language code is required (for example: en).");
                    } else if !looks_like_language_code(&language) {
                        send_webview_status(
                            &webview,
                            "error",
                            "Language code looks invalid. Use something like en, pt-BR, or multi.",
                        );
                    } else if auto_stop_minutes.is_empty() {
                        send_webview_status(
                            &webview,
                            "error",
                            "Auto-stop minutes is required and must be a whole number.",
                        );
                    } else {
                        match auto_stop_minutes.parse::<u32>() {
                            Ok(max_recording_minutes) if max_recording_minutes > 0 => {
                                let mut cfg = shared_cfg.write().unwrap();
                                cfg.deepgram.api_key = api_key;
                                cfg.deepgram.language = language;
                                cfg.audio.max_recording_minutes = max_recording_minutes;
                                // The dropdown is the source of truth, but re-resolve
                                // so language=multi always lands on nova-3 and an empty
                                // or bogus value can't be persisted.
                                let requested = if model.is_empty() {
                                    cfg.deepgram.model.clone()
                                } else {
                                    model
                                };
                                let resolved = normalize_model(&requested, &cfg.deepgram.language);
                                let locked_to_multilingual =
                                    resolved != requested && resolved == MULTILINGUAL_MODEL;
                                let model_changed = resolved != cfg.deepgram.model;
                                if model_changed {
                                    info!(
                                        "model changed from '{}' to '{}' (language '{}')",
                                        cfg.deepgram.model, resolved, cfg.deepgram.language
                                    );
                                    cfg.deepgram.model = resolved;
                                }
                                match save_config(&config_path, &cfg) {
                                    Ok(_) => {
                                        info!(
                                            "Deepgram settings updated from GUI (API key + language + auto-stop={} min)",
                                            max_recording_minutes
                                        );
                                        // Echo the canonical language back so the field
                                        // shows exactly what was stored (quotes stripped,
                                        // casing fixed) without duplicating that logic in JS.
                                        send_webview_event(
                                            &webview,
                                            "voicekeys-language-normalized",
                                            serde_json::json!({ "language": cfg.deepgram.language }),
                                        );
                                        send_webview_status(
                                            &webview,
                                            "ok",
                                            if locked_to_multilingual {
                                                "Saved. Switched to the nova-3 model, which is the one that supports multilingual."
                                            } else {
                                                "Saved API key, language, and auto-stop cap. New recordings use these settings immediately."
                                            },
                                        );
                                        request_deepgram_usage_refresh(
                                            Arc::clone(&shared_cfg),
                                            usage_refresh_proxy.clone(),
                                        );
                                    }
                                    Err(e) => {
                                        error!("failed to save config: {}", e);
                                        send_webview_status(
                                            &webview,
                                            "error",
                                            "Failed to save Deepgram settings.",
                                        );
                                    }
                                }
                            }
                            _ => {
                                send_webview_status(
                                    &webview,
                                    "error",
                                    "Auto-stop minutes must be a whole number greater than 0.",
                                );
                            }
                        }
                    }
                }
                IpcCommand::RefreshDeepgramUsage => {
                    request_deepgram_usage_refresh(
                        Arc::clone(&shared_cfg),
                        usage_refresh_proxy.clone(),
                    );
                }
                IpcCommand::RefreshDeepgramUsageHistory => {
                    request_deepgram_usage_history_refresh(
                        Arc::clone(&shared_cfg),
                        usage_history_proxy.clone(),
                    );
                }
                IpcCommand::SaveHotkeys {
                    paste_modifier,
                    paste_trigger,
                    clipboard_modifier,
                    clipboard_trigger,
                } => {
                    let paste = combo_from_parts(&paste_modifier, &paste_trigger);
                    let clipboard = combo_from_parts(&clipboard_modifier, &clipboard_trigger);

                    if paste.len() < 2 || clipboard.len() < 2 {
                        send_webview_status(
                            &webview,
                            "error",
                            "Enter both keys for each mode: modifier key + main key.",
                        );
                    } else if parse_combo(&paste).is_empty() || parse_combo(&clipboard).is_empty() {
                        send_webview_status(
                            &webview,
                            "error",
                            "One or more hotkey names are invalid. Use key names like shift, minus, equal, f1.",
                        );
                    } else {
                        let mut cfg = shared_cfg.write().unwrap();
                        cfg.hotkeys.paste = paste.clone();
                        cfg.hotkeys.clipboard = clipboard.clone();
                        match save_config(&config_path, &cfg) {
                            Ok(_) => {
                                info!(
                                    "hotkeys updated from GUI: paste=[{}], clipboard=[{}]",
                                    combo_label(&paste),
                                    combo_label(&clipboard)
                                );
                                send_webview_status(
                                    &webview,
                                    "ok",
                                    "Saved hotkeys. Press once to start recording and press again to stop.",
                                );
                            }
                            Err(e) => {
                                error!("failed to save hotkeys: {}", e);
                                send_webview_status(&webview, "error", "Failed to save hotkeys.");
                            }
                        }
                    }
                }
                IpcCommand::OpenMacSetting(setting) => {
                    let url = match setting.as_str() {
                        "accessibility" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                        "input_monitoring" => "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
                        "microphone" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
                        _ => "",
                    };
                    if !url.is_empty() {
                        let _ = process::Command::new("open").arg(url).spawn();
                    }
                }
                IpcCommand::OpenTranscriptHistory => {
                    // Touch it first so the very first click opens an empty file
                    // rather than failing on a path that doesn't exist yet.
                    if let Err(e) = append_transcript_history_touch(&transcript_history_path) {
                        error!("failed to create transcript history file: {}", e);
                        send_webview_status(
                            &webview,
                            "error",
                            "Could not create the message history file.",
                        );
                        return;
                    }
                    match open_path_in_os(&transcript_history_path) {
                        Ok(_) => send_webview_status(&webview, "ok", "Opening message history..."),
                        Err(e) => {
                            error!("failed to open transcript history: {}", e);
                            send_webview_status(
                                &webview,
                                "error",
                                "Could not open the message history file.",
                            );
                        }
                    }
                }
                IpcCommand::OpenLog => {
                    if let Err(e) = fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                        error!("failed to create/open log file: {}", e);
                        send_webview_status(&webview, "error", "Could not access the log file.");
                        return;
                    }

                    let log_excerpt = match read_last_log_lines(&log_path, 500) {
                        Ok(lines) => {
                            if lines.trim().is_empty() {
                                "(Log file is currently empty.)".to_string()
                            } else {
                                lines
                            }
                        }
                        Err(e) => {
                            error!("failed to read log file: {}", e);
                            send_webview_status(&webview, "error", "Could not read the log file.");
                            return;
                        }
                    };

                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => match clipboard.set_text(log_excerpt) {
                            Ok(_) => {
                                notify("Voice Keys", "Copied last 500 log lines to clipboard.");
                                send_webview_status(
                                    &webview,
                                    "ok",
                                    "Copied last 500 log lines to clipboard.",
                                );
                            }
                            Err(e) => {
                                error!("failed to copy log output to clipboard: {}", e);
                                send_webview_status(
                                    &webview,
                                    "error",
                                    "Could not copy log output to clipboard.",
                                );
                            }
                        },
                        Err(e) => {
                            error!("failed to open clipboard for log copy: {}", e);
                            send_webview_status(
                                &webview,
                                "error",
                                "Could not copy log output to clipboard.",
                            );
                        }
                    }
                }
                IpcCommand::CopyTranscripts => {
                    if recent_transcripts.is_empty() {
                        send_webview_status(&webview, "ok", "No transcripts yet.");
                        return;
                    }
                    let combined: String = recent_transcripts
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("{}. {}", i + 1, t))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => match clipboard.set_text(combined) {
                            Ok(_) => {
                                let count = recent_transcripts.len();
                                let msg = format!("Copied {} transcript{} to clipboard.", count, if count == 1 { "" } else { "s" });
                                send_webview_status(&webview, "ok", &msg);
                            }
                            Err(e) => {
                                error!("failed to copy transcripts: {}", e);
                                send_webview_status(&webview, "error", "Could not copy transcripts.");
                            }
                        },
                        Err(e) => {
                            error!("failed to open clipboard for transcripts: {}", e);
                            send_webview_status(&webview, "error", "Could not copy transcripts.");
                        }
                    }
                }
                IpcCommand::CopyLastMessage => {
                    // Newest transcript is pushed to the back of the deque.
                    let last = match recent_transcripts.back() {
                        Some(text) => text.clone(),
                        None => {
                            send_webview_status(&webview, "ok", "No transcripts yet.");
                            return;
                        }
                    };
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => match clipboard.set_text(last) {
                            Ok(_) => {
                                send_webview_status(
                                    &webview,
                                    "ok",
                                    "Copied the last message to clipboard.",
                                );
                            }
                            Err(e) => {
                                error!("failed to copy last message: {}", e);
                                send_webview_status(
                                    &webview,
                                    "error",
                                    "Could not copy the last message.",
                                );
                            }
                        },
                        Err(e) => {
                            error!("failed to open clipboard for last message: {}", e);
                            send_webview_status(
                                &webview,
                                "error",
                                "Could not copy the last message.",
                            );
                        }
                    }
                }
                IpcCommand::RetryTranscription => {
                    // Three independent brakes on spending credit here: the
                    // request only exists because someone clicked, a re-send
                    // already in flight blocks another, and the per-recording
                    // cap is enforced again rather than trusted from the UI.
                    if retry_in_flight {
                        debug!("re-send already running; ignoring duplicate request");
                        return;
                    }
                    let Some(pending) = pending_retry else {
                        send_webview_status(&webview, "ok", "Nothing to re-send.");
                        return;
                    };
                    if pending.retries_used >= MAX_USER_RETRIES {
                        send_webview_status(
                            &webview,
                            "error",
                            "Out of retries for that recording.",
                        );
                        return;
                    }
                    let Some(path) = retry_audio_path().map(Path::to_path_buf) else {
                        send_webview_status(&webview, "error", "No saved audio to re-send.");
                        return;
                    };
                    let dg_cfg = match shared_cfg.read() {
                        Ok(c) => c.deepgram.clone(),
                        Err(_) => {
                            error!("failed to read config for re-send");
                            send_webview_status(&webview, "error", "Could not read settings.");
                            return;
                        }
                    };

                    retry_in_flight = true;
                    let attempt = pending.retries_used + 1;
                    info!(
                        "re-sending saved recording at user request (retry {} of {})",
                        attempt, MAX_USER_RETRIES
                    );
                    let proxy = retry_proxy.clone();
                    // Read on the worker: the file runs to tens of megabytes and
                    // the event loop is also the UI thread.
                    thread::spawn(move || match fs::read(&path) {
                        Ok(wav) => transcribe_and_deliver(
                            wav,
                            pending.duration_secs,
                            pending.mode,
                            dg_cfg,
                            proxy,
                            RetryContext {
                                offer_id: Some(pending.offer_id),
                                retries_used: attempt,
                            },
                        ),
                        Err(e) => {
                            error!("could not read saved audio {}: {}", path.display(), e);
                            // Unreadable audio is not worth keeping around.
                            let _ = proxy.send_event(UserEvent::RetryFinished {
                                offer_id: pending.offer_id,
                                keep_audio: false,
                            });
                        }
                    });
                }
                IpcCommand::DiscardRetry => {
                    if let Some(pending) = pending_retry.take() {
                        info!("saved recording discarded by user (offer {})", pending.offer_id);
                        clear_retry_stash("discarded by user");
                    }
                    send_webview_event(
                        &webview,
                        "voicekeys-retry-offer",
                        serde_json::json!({ "clear": true }),
                    );
                }
                IpcCommand::CopyLink(url) => {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => match clipboard.set_text(&url) {
                            Ok(_) => {
                                notify("Voice Keys", "Link copied to clipboard. Paste into your web browser!");
                                send_webview_status(
                                    &webview,
                                    "ok",
                                    "Link copied to clipboard. Paste into your web browser!",
                                );
                            }
                            Err(e) => {
                                error!("failed to copy URL to clipboard: {}", e);
                                send_webview_status(&webview, "error", "Could not copy link to clipboard.");
                            }
                        },
                        Err(e) => {
                            error!("failed to open clipboard: {}", e);
                            send_webview_status(&webview, "error", "Could not copy link to clipboard.");
                        }
                    }
                }
                IpcCommand::WindowDrag => {
                    if let Err(e) = window.drag_window() {
                        warn!("failed to drag window: {}", e);
                    }
                }
                IpcCommand::WindowMinimize => {
                    window.set_minimized(true);
                }
                IpcCommand::WindowMaximizeToggle => {
                    window.set_maximized(!window.is_maximized());
                }
                IpcCommand::WindowClose => {
                    hide_window(&window);
                }
                IpcCommand::Quit => {
                    *control_flow = ControlFlow::Exit;
                }
            },

            _ => {}
        }

        let _ = &tray_icon;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_code_strips_quotes_and_fixes_casing() {
        // Quoted forms people actually paste.
        assert_eq!(normalize_language_code("en"), "en");
        assert_eq!(normalize_language_code("'en'"), "en");
        assert_eq!(normalize_language_code("\"en\""), "en");
        assert_eq!(normalize_language_code("  'en'  "), "en");
        assert_eq!(normalize_language_code("`en`"), "en");
        assert_eq!(normalize_language_code("\u{2018}en\u{2019}"), "en");
        assert_eq!(normalize_language_code("\u{201C}en\u{201D}"), "en");
        assert_eq!(normalize_language_code("EN"), "en");

        // Regional codes keep their conventional casing.
        assert_eq!(normalize_language_code("pt-BR"), "pt-BR");
        assert_eq!(normalize_language_code("fr-CA"), "fr-CA");
        assert_eq!(normalize_language_code("fr-ca"), "fr-CA");
        assert_eq!(normalize_language_code("'fr-ca'"), "fr-CA");
        assert_eq!(normalize_language_code("PT-br"), "pt-BR");
        assert_eq!(normalize_language_code("zh-hans"), "zh-Hans");
        assert_eq!(normalize_language_code("es-419"), "es-419");

        // multi is a first-class value now, not an error.
        assert_eq!(normalize_language_code("multi"), "multi");
        assert_eq!(normalize_language_code("'multi'"), "multi");
        assert_eq!(normalize_language_code("MULTI"), "multi");

        // An unmatched quote is not a pair, so it is left for the shape guard to reject.
        assert_eq!(normalize_language_code("'en"), "'en");
    }

    #[test]
    fn language_code_shape_guard() {
        for good in ["en", "es", "multi", "pt-BR", "fr-CA", "zh-Hans", "es-419"] {
            assert!(
                looks_like_language_code(good),
                "{} should be accepted",
                good
            );
        }
        for bad in [
            "",
            "e",
            "zz!!",
            "english-language-code",
            "'en",
            "en-",
            "-en",
            "1234",
        ] {
            assert!(!looks_like_language_code(bad), "{} should be rejected", bad);
        }
    }

    #[test]
    fn month_key_parses_grouping_start() {
        let grouping = Some(DeepgramGrouping {
            start: Some("2026-03-18".to_string()),
        });
        assert_eq!(month_key_from_grouping(&grouping), Some((2026, 3)));

        assert_eq!(month_key_from_grouping(&None), None);
        assert_eq!(
            month_key_from_grouping(&Some(DeepgramGrouping { start: None })),
            None
        );
        assert_eq!(
            month_key_from_grouping(&Some(DeepgramGrouping {
                start: Some("not-a-date".to_string())
            })),
            None
        );
    }

    #[test]
    fn breakdown_result_deserializes_live_api_shape() {
        // Verbatim from a real /usage/breakdown response.
        let raw = r#"{"results":[{"hours":0.0029969,"total_hours":0.0029972,"agent_hours":0.0,
            "tokens_in":0,"tokens_out":0,"tts_characters":0,"requests":2,
            "grouping":{"start":"2026-03-18","end":"2026-03-18","accessor":null,"endpoint":null,
            "feature_set":null,"models":null,"method":null,"tags":null,"deployment":null}}]}"#;
        let parsed: DeepgramUsageBreakdownResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].requests, 2);
        assert_eq!(
            month_key_from_grouping(&parsed.results[0].grouping),
            Some((2026, 3))
        );
    }

    /// Hits the live Deepgram API, so it is opt-in:
    ///   VOICEKEYS_TEST_DG_KEY=<admin key> cargo test -- --ignored --nocapture
    #[test]
    #[ignore = "requires network and a Deepgram admin API key"]
    fn usage_history_aggregates_live_api() {
        let key = std::env::var("VOICEKEYS_TEST_DG_KEY")
            .expect("set VOICEKEYS_TEST_DG_KEY to run this test");
        let payload = fetch_deepgram_usage_history(&key).expect("history fetch failed");

        println!("range: {}", payload.range_label);
        println!(
            "all-time: {} | {} | {}",
            payload.total_minutes, payload.total_requests, payload.total_spend
        );
        for m in &payload.months {
            println!(
                "  {:<10} {:<24} {:<18} {}",
                m.label, m.minutes_label, m.requests_label, m.spend_label
            );
        }

        assert!(!payload.months.is_empty(), "expected at least one month");
        // Months must be contiguous and end at the current month.
        let today = Utc::now().date_naive();
        let last = payload.months.last().unwrap();
        assert_eq!(
            last.label,
            format!("{} {}", month_axis_label(today.month()), today.year()),
            "chart must run through the current month"
        );
        // Totals must equal the sum of the per-month buckets.
        let summed: f64 = payload.months.iter().map(|m| m.minutes).sum();
        let reported: f64 = payload
            .total_minutes
            .trim_end_matches(" billable min")
            .parse()
            .expect("total_minutes should parse");
        assert!(
            (summed - reported).abs() < 0.15,
            "per-month minutes {} should sum to the all-time total {}",
            summed,
            reported
        );
    }

    #[test]
    fn multi_forces_the_nova3_model() {
        // multi is only multilingual on nova-3, so it wins over any stored model.
        assert_eq!(normalize_model("nova-2", "multi"), "nova-3");
        assert_eq!(normalize_model("nova-3", "multi"), "nova-3");
        assert_eq!(normalize_model("", "multi"), "nova-3");
        assert_eq!(normalize_model("nova-2", "MULTI"), "nova-3");

        // Monolingual codes keep whichever model was picked.
        assert_eq!(normalize_model("nova-2", "en"), "nova-2");
        assert_eq!(normalize_model("nova-3", "en"), "nova-3");
        assert_eq!(normalize_model("nova-2", "pt-BR"), "nova-2");
        assert_eq!(normalize_model("NOVA-2", "en"), "nova-2");

        // Empty falls back to the default; a deliberate specialty model is kept.
        assert_eq!(normalize_model("", "en"), default_model());
        assert_eq!(normalize_model("nova-2-meeting", "en"), "nova-2-meeting");
    }

    #[test]
    fn effective_model_matches_normalize() {
        let cfg = DeepgramConfig {
            api_key: "k".into(),
            model: "nova-2".into(),
            language: "multi".into(),
            ..Default::default()
        };
        assert_eq!(effective_model(&cfg), "nova-3");

        let mono = DeepgramConfig {
            language: "es".into(),
            ..cfg.clone()
        };
        assert_eq!(effective_model(&mono), "nova-2");
    }

    #[test]
    fn month_axis_labels() {
        assert_eq!(month_axis_label(1), "Jan");
        assert_eq!(month_axis_label(12), "Dec");
        assert_eq!(month_axis_label(0), "?");
        assert_eq!(month_axis_label(13), "?");
    }

    #[test]
    fn transcript_history_appends_rather_than_truncating() {
        let dir =
            std::env::temp_dir().join(format!("voicekeys-history-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("transcripts.txt");

        // The first write has to create the directory tree, not just the file.
        append_transcript_history(&path, "first message").unwrap();
        append_transcript_history(&path, "  second message  ").unwrap();

        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("first message"), "{body}");
        // Earlier entries must survive: this is a permanent record, not a log.
        assert!(body.contains("second message"), "{body}");
        assert_eq!(body.matches('[').count(), 2, "one timestamp per entry");
        // Surrounding whitespace is trimmed so entries line up.
        assert!(body.contains("\nsecond message\n"), "{body}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncate_chars_cuts_on_character_boundaries() {
        // The regression this exists for: byte-slicing a CJK transcript panicked,
        // because no multiple of 3 lands on 77.
        let cjk = "语音键盘转录测试".repeat(5);
        assert_eq!(cjk.len(), 120, "40 characters at 3 bytes each");
        assert_eq!(
            truncate_chars(&cjk, 77).chars().count(),
            40,
            "shorter than 77"
        );

        let long_cjk = "语".repeat(200);
        assert_eq!(truncate_chars(&long_cjk, 77), "语".repeat(77));

        // Accented Latin: 'é' is two bytes, so byte and character counts diverge.
        assert_eq!(truncate_chars("héllo", 3), "hél");
        assert_eq!(truncate_chars("héllo", 99), "héllo");
        assert_eq!(truncate_chars("", 10), "");
        assert_eq!(truncate_chars("abc", 0), "");
    }

    #[test]
    fn transcript_history_sits_beside_the_config() {
        let path = transcript_history_path_for_config(Path::new("/tmp/vk/config.yaml"));
        assert_eq!(path, PathBuf::from("/tmp/vk/transcripts.txt"));

        // A bare filename means the config is in the working directory.
        let relative = transcript_history_path_for_config(Path::new("config.yaml"));
        assert_eq!(relative, PathBuf::from("./transcripts.txt"));
    }

    #[test]
    fn audio_source_label_stops_at_the_header_width() {
        // The name that set the limit: exactly at the cap, so it survives whole.
        let longest_kept = "MacBook Air Microphone Input Con";
        assert_eq!(longest_kept.chars().count(), MAX_AUDIO_SOURCE_CHARS);
        assert_eq!(
            ellipsize(longest_kept, MAX_AUDIO_SOURCE_CHARS),
            longest_kept
        );

        // One character more and it gets cut, and the `..` counts against the
        // budget rather than pushing past it.
        let cut = ellipsize("MacBook Air Microphone Input Cont", MAX_AUDIO_SOURCE_CHARS);
        assert_eq!(cut, "MacBook Air Microphone Input C..");
        assert!(cut.chars().count() <= MAX_AUDIO_SOURCE_CHARS);

        // Names that fit carry no mark at all.
        assert_eq!(ellipsize("Shure MV7", MAX_AUDIO_SOURCE_CHARS), "Shure MV7");
        assert!(!ellipsize(longest_kept, MAX_AUDIO_SOURCE_CHARS).ends_with(TRUNCATION_MARK));

        // Two dots, never three.
        assert!(cut.ends_with(".."));
        assert!(!cut.ends_with("..."));

        // Device names are user-set and routinely carry emoji; cutting one of
        // those on a byte boundary would panic.
        let emoji = ellipsize("cilantro ˙ᵕ˙ 🎙️🎙️🎙️🎙️🎙️🎙️🎙️🎙️🎙️🎙️", MAX_AUDIO_SOURCE_CHARS);
        assert!(emoji.ends_with(".."));
        assert!(emoji.chars().count() <= MAX_AUDIO_SOURCE_CHARS);
    }

    #[test]
    fn retry_audio_sits_beside_the_config() {
        let path = retry_audio_path_for_config(Path::new("/tmp/vk/config.yaml"));
        assert_eq!(path, PathBuf::from("/tmp/vk/voicekeys-retry.wav"));

        let relative = retry_audio_path_for_config(Path::new("config.yaml"));
        assert_eq!(relative, PathBuf::from("./voicekeys-retry.wav"));
    }

    #[test]
    fn retry_audio_path_is_one_fixed_slot() {
        // The whole "no runaway WAV files" guarantee rests on this: whatever
        // failed and however often, every failure names the same file.
        let a = retry_audio_path_for_config(Path::new("/tmp/vk/config.yaml"));
        let b = retry_audio_path_for_config(Path::new("/tmp/vk/config.yaml"));
        assert_eq!(a, b);
    }

    #[test]
    fn clock_duration_reads_like_a_stopwatch() {
        assert_eq!(format_clock_duration(0.0), "0s");
        assert_eq!(format_clock_duration(47.4), "47s");
        assert_eq!(format_clock_duration(59.6), "1m 00s");
        assert_eq!(format_clock_duration(479.2), "7m 59s");
        assert_eq!(format_clock_duration(605.0), "10m 05s");

        // Duration is derived from a sample count divided by a sample rate, so
        // a zero rate would hand this a NaN rather than a number.
        assert_eq!(format_clock_duration(f64::NAN), "0s");
        assert_eq!(format_clock_duration(-1.0), "0s");
    }

    #[test]
    fn every_failure_reason_has_both_voices() {
        for reason in [
            RetryReason::EmptyTranscript,
            RetryReason::RequestFailed,
            RetryReason::MissingApiKey,
        ] {
            // The offer names the length so the user can tell which recording
            // is on the line; the fallback is the message Voice Keys has always
            // shown when no retry can be offered.
            assert!(reason.offer_message(479.2).contains("7m 59s"));
            assert!(!reason.plain_message().is_empty());
        }
        assert_eq!(
            RetryReason::EmptyTranscript.plain_message(),
            "No speech detected."
        );
    }
}
