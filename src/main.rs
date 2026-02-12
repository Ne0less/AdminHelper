#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use global_hotkey::{
    GlobalHotKeyManager, 
    hotkey::{HotKey, Code, Modifiers}, 
    GlobalHotKeyEvent, HotKeyState
};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use serde::{Deserialize, Serialize};
use std::fs;
use std::thread;
use std::time::{Duration, Instant};
use chrono::{Local, Timelike, Datelike};
use arboard::Clipboard; 
use rdev::{listen, Event, EventType}; 
use std::collections::HashMap;

#[cfg(target_os = "windows")]
use std::mem::size_of;

// --- ИМПОРТЫ WINDOWS ---
#[cfg(target_os = "windows")]
use winapi::um::winuser::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW, IsIconic,
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId,
    AttachThreadInput, SetFocus, SetActiveWindow, 
    SystemParametersInfoW, SPI_SETFOREGROUNDLOCKTIMEOUT, SPIF_SENDCHANGE,
    INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, SendInput,
    keybd_event, VK_MENU,
    SetWindowPos, HWND_TOPMOST, HWND_NOTOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    PeekMessageW, TranslateMessage, DispatchMessageW, MSG, PM_REMOVE,
    EnumWindows, GetWindowTextW, GetClassNameW
};
#[cfg(target_os = "windows")]
use winapi::um::processthreadsapi::GetCurrentThreadId;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

#[cfg(target_os = "windows")]
extern "system" {
    fn SwitchToThisWindow(hwnd: winapi::shared::windef::HWND, fAltTab: winapi::shared::minwindef::BOOL);
}

mod data;
use data::{Organization, Teleport};

// ================= ЛОГИРОВАНИЕ =================
static GLOBAL_LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn get_logs() -> &'static Mutex<Vec<String>> {
    GLOBAL_LOGS.get_or_init(|| Mutex::new(Vec::new()))
}

fn log(msg: &str) {
    let time = Local::now().format("%H:%M:%S%.3f");
    let full_msg = format!("[{}] {}", time, msg);
    println!("{}", full_msg); 
    if let Ok(mut logs) = get_logs().lock() {
        logs.push(full_msg);
        if logs.len() > 100 { logs.remove(0); } 
    }
}

fn check_admin_rights() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        use winapi::um::securitybaseapi::GetTokenInformation;
        use winapi::um::processthreadsapi::{OpenProcessToken, GetCurrentProcess};
        use winapi::um::winnt::{TOKEN_QUERY, TokenElevation, TOKEN_ELEVATION};
        use winapi::um::handleapi::CloseHandle;
        
        let mut handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) != 0 {
            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
            let ret = GetTokenInformation(
                handle, 
                TokenElevation, 
                &mut elevation as *mut _ as *mut _, 
                size, 
                &mut size
            );
            CloseHandle(handle);
            return ret != 0 && elevation.TokenIsElevated != 0;
        }
    }
    false
}


pub fn restore_application_window(ctx: &egui::Context) {
    #[cfg(target_os = "windows")]
    unsafe {
        let window_name: Vec<u16> = std::ffi::OsStr::new("AdminHelper\0").encode_wide().collect();
        let hwnd = FindWindowW(std::ptr::null(), window_name.as_ptr());

        if !hwnd.is_null() {
            SystemParametersInfoW(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, std::ptr::null_mut(), SPIF_SENDCHANGE);

            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            } else {
                ShowWindow(hwnd, SW_SHOW);
            }

            let foreground = GetForegroundWindow();
            if !foreground.is_null() && foreground != hwnd {
                let mut fg_thread_id: u32 = 0;
                let fg_process_id = GetWindowThreadProcessId(foreground, &mut fg_thread_id);
                let current_thread_id = GetCurrentThreadId();
                if fg_process_id != current_thread_id {
                    AttachThreadInput(fg_process_id, current_thread_id, 1);
                }
            }

            SwitchToThisWindow(hwnd, 1);
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);

            SetForegroundWindow(hwnd);
            BringWindowToTop(hwnd);
            SetFocus(hwnd);
            SetActiveWindow(hwnd);

            keybd_event(VK_MENU as u8, 0, 0, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);

            if !foreground.is_null() && foreground != hwnd {
                let mut fg_thread_id: u32 = 0;
                let fg_process_id = GetWindowThreadProcessId(foreground, &mut fg_thread_id);
                let current_thread_id = GetCurrentThreadId();
                if fg_process_id != current_thread_id {
                    AttachThreadInput(fg_process_id, current_thread_id, 0);
                }
            }
            
            ctx.request_repaint();
            log("System: Window forcefully restored & UI awoken.");
        } else {
            log("System: ERROR - Target window not found!");
        }
    }
}


fn focus_game_window() {
    #[cfg(target_os = "windows")]
    unsafe {
        static mut GAME_HWND: winapi::shared::windef::HWND = std::ptr::null_mut();

        unsafe extern "system" fn enum_window_callback(
            hwnd: winapi::shared::windef::HWND,
            _lparam: isize,
        ) -> i32 {
            let mut class_name: [u16; 512] = [0; 512];
            let mut title: [u16; 512] = [0; 512];
            
            let class_len = GetClassNameW(hwnd, class_name.as_mut_ptr(), 512);
            let title_len = GetWindowTextW(hwnd, title.as_mut_ptr(), 512);
            
            let class_str = if class_len > 0 { String::from_utf16_lossy(&class_name[..class_len as usize]).to_lowercase() } else { String::new() };
            let title_str = if title_len > 0 { String::from_utf16_lossy(&title[..title_len as usize]).to_lowercase() } else { String::new() };
            
            if class_str == "grcwindow" || title_str.contains("rage multiplayer") || title_str.contains("grand theft auto") {
                GAME_HWND = hwnd;
                return 0; 
            }
            1 
        }

        GAME_HWND = std::ptr::null_mut();
        EnumWindows(Some(enum_window_callback), 0);

        if !GAME_HWND.is_null() {
            let hwnd = GAME_HWND;
            if IsIconic(hwnd) != 0 { ShowWindow(hwnd, SW_RESTORE); }
            SwitchToThisWindow(hwnd, 1);
            SetForegroundWindow(hwnd);
            BringWindowToTop(hwnd);
            SetFocus(hwnd);
            log(&format!("System: Game window found and focused! (HWND: {:?})", hwnd));
        } else {
            log("System: Warning - Game window still not found! Anti-Cheat might be hiding it.");
        }
    }
}

// ================= СТРУКТУРЫ =================
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveReplacement {
    pub trigger: String,
    pub label: String,
    pub text: String,
    #[serde(skip)]
    pub is_system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub admin_name: String,
    pub admin_id: String,
    pub saved_triggers: HashMap<String, String>, 
    #[serde(default)] pub custom_replacements: Vec<ActiveReplacement>, 
    #[serde(default)] pub run_on_startup: bool,
    #[serde(default)] pub theme_mode: usize, 
    
    #[serde(default = "default_key_main")] pub key_main: String,    
    #[serde(default = "default_key_punish")] pub key_punish: String, 
    #[serde(default = "default_key_event")] pub key_event: String,    
    #[serde(default = "default_key_mp")] pub key_mp: String,           
    #[serde(default = "default_key_reload")] pub key_reload: String, 
}

fn default_key_main() -> String { "NONE+F6".to_string() }
fn default_key_punish() -> String { "NONE+F7".to_string() }
fn default_key_event() -> String { "NONE+F8".to_string() }
fn default_key_mp() -> String { "NONE+F10".to_string() }
fn default_key_reload() -> String { "CONTROL+R".to_string() }

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            admin_name: "Administrator".to_string(),
            admin_id: String::new(),
            saved_triggers: HashMap::new(),
            custom_replacements: Vec::new(),
            run_on_startup: false,
            theme_mode: 0, 
            key_main: default_key_main(),
            key_punish: default_key_punish(),
            key_event: default_key_event(),
            key_mp: default_key_mp(),
            key_reload: default_key_reload(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)] pub category: String,
    pub article: String,
    pub title: String,
    pub description: String,
    #[serde(default)] pub ban: String,
    #[serde(default)] pub warn: u8,
    #[serde(default)] pub demorgan: String,
    #[serde(default)] pub pacifist: String,
    #[serde(default)] pub mutev: String,
    #[serde(rename = "mutec", default)] pub mute_chat: String,
    #[serde(rename = "muter", default)] pub mute_report: String,
    #[serde(default)] pub ban_market: String,
}

#[derive(Clone, PartialEq)]
struct PunishmentOption {
    label: String,
    cmd_base: String, 
    time_arg: String, 
}

#[derive(Serialize, Deserialize)]
struct TimerState {
    total_seconds: u64,
    last_reset_day: u32, 
}


#[cfg(target_os = "windows")]
fn send_scan_code(scan_code: u16, press: bool) {
    unsafe {
        let mut input = INPUT {
            type_: INPUT_KEYBOARD,
            u: std::mem::zeroed(),
        };
        let mut flags = KEYEVENTF_SCANCODE;
        if !press { flags |= KEYEVENTF_KEYUP; }
        *input.u.ki_mut() = KEYBDINPUT {
            wVk: 0, wScan: scan_code, dwFlags: flags, time: 0, dwExtraInfo: 0,
        };
        SendInput(1, &mut input, size_of::<INPUT>() as i32);
    }
}

#[cfg(not(target_os = "windows"))]
fn send_scan_code(_scan_code: u16, _press: bool) {}

fn type_in_game(ctx: Option<egui::Context>, text: String, open_chat: bool, press_enter: bool, finish_flag: Option<Arc<AtomicBool>>) {
    log(&format!("Action: Typing '{}'", text.replace("\n", " ")));
    
    if let Some(c) = &ctx { 
        c.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    thread::spawn(move || {
        focus_game_window();
        thread::sleep(Duration::from_millis(300)); 

        let lines: Vec<&str> = text.split('\n').collect();

        for (i, line) in lines.iter().enumerate() {
            let clean_line = line.trim();
            if clean_line.is_empty() { continue; }

            if i > 0 { thread::sleep(Duration::from_millis(250)); }
            
            if let Ok(mut clipboard) = Clipboard::new() {
                let _ = clipboard.set_text(clean_line.to_string());
            }

            const SC_T: u16 = 0x14; const SC_V: u16 = 0x2F;       
            const SC_RETURN: u16 = 0x1C; const SC_LCTRL: u16 = 0x1D;  

            if open_chat {
                send_scan_code(SC_T, true); thread::sleep(Duration::from_millis(20));
                send_scan_code(SC_T, false); thread::sleep(Duration::from_millis(250)); 
            }
            
            send_scan_code(SC_LCTRL, true); thread::sleep(Duration::from_millis(20));
            send_scan_code(SC_V, true); thread::sleep(Duration::from_millis(20));
            send_scan_code(SC_V, false); thread::sleep(Duration::from_millis(20));
            send_scan_code(SC_LCTRL, false); thread::sleep(Duration::from_millis(50)); 
            
            if press_enter {
                send_scan_code(SC_RETURN, true); thread::sleep(Duration::from_millis(30));
                send_scan_code(SC_RETURN, false); thread::sleep(Duration::from_millis(70));
                
                send_scan_code(SC_RETURN, true); thread::sleep(Duration::from_millis(30));
                send_scan_code(SC_RETURN, false);
            }
        }
        if let Some(flag) = finish_flag { flag.store(false, Ordering::Relaxed); }
    });
}

fn run_teleport(ctx: &egui::Context, coords: &str) {
    type_in_game(Some(ctx.clone()), format!("/setpos {}", coords), true, true, None);
}


fn restart_app() {
    log("System: Restarting application...");
    #[cfg(target_os = "windows")]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
        std::process::exit(0);
    }
}

fn parse_hotkey(s: &str) -> Option<(Modifiers, Code)> {
    let parts: Vec<&str> = s.split('+').collect();
    if parts.len() < 2 { return None; }
    
    let code_str = parts.last().unwrap();
    let mod_str = &parts[0..parts.len()-1];

    let mut modifiers = Modifiers::empty();
    for m in mod_str {
        match *m {
            "CONTROL" | "CTRL" => modifiers |= Modifiers::CONTROL,
            "SHIFT" => modifiers |= Modifiers::SHIFT,
            "ALT" => modifiers |= Modifiers::ALT,
            "SUPER" | "WIN" => modifiers |= Modifiers::SUPER,
            _ => {}
        }
    }

    let code = match *code_str {
        "F1" => Code::F1, "F2" => Code::F2, "F3" => Code::F3, "F4" => Code::F4,
        "F5" => Code::F5, "F6" => Code::F6, "F7" => Code::F7, "F8" => Code::F8,
        "F9" => Code::F9, "F10" => Code::F10, "F11" => Code::F11, "F12" => Code::F12,
        "A" => Code::KeyA, "B" => Code::KeyB, "C" => Code::KeyC, "D" => Code::KeyD,
        "E" => Code::KeyE, "F" => Code::KeyF, "G" => Code::KeyG, "H" => Code::KeyH,
        "I" => Code::KeyI, "J" => Code::KeyJ, "K" => Code::KeyK, "L" => Code::KeyL,
        "M" => Code::KeyM, "N" => Code::KeyN, "O" => Code::KeyO, "P" => Code::KeyP,
        "Q" => Code::KeyQ, "R" => Code::KeyR, "S" => Code::KeyS, "T" => Code::KeyT,
        "U" => Code::KeyU, "V" => Code::KeyV, "W" => Code::KeyW, "X" => Code::KeyX,
        "Y" => Code::KeyY, "Z" => Code::KeyZ,
        "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2, "3" => Code::Digit3,
        "4" => Code::Digit4, "5" => Code::Digit5, "6" => Code::Digit6, "7" => Code::Digit7,
        "8" => Code::Digit8, "9" => Code::Digit9,
        "SPACE" => Code::Space, "ENTER" => Code::Enter, "ESCAPE" => Code::Escape,
        "UP" => Code::ArrowUp, "DOWN" => Code::ArrowDown, "LEFT" => Code::ArrowLeft, "RIGHT" => Code::ArrowRight,
        "INSERT" => Code::Insert, "DELETE" => Code::Delete, "HOME" => Code::Home, "END" => Code::End,
        "PAGEUP" => Code::PageUp, "PAGEDOWN" => Code::PageDown,
        _ => return None, 
    };

    Some((modifiers, code))
}

fn input_to_bind_string(key: egui::Key, modifiers: egui::Modifiers) -> String {
    let key_str = match key {
        egui::Key::F1 => "F1", egui::Key::F2 => "F2", egui::Key::F3 => "F3", egui::Key::F4 => "F4",
        egui::Key::F5 => "F5", egui::Key::F6 => "F6", egui::Key::F7 => "F7", egui::Key::F8 => "F8",
        egui::Key::F9 => "F9", egui::Key::F10 => "F10", egui::Key::F11 => "F11", egui::Key::F12 => "F12",
        egui::Key::A => "A", egui::Key::B => "B", egui::Key::C => "C", egui::Key::D => "D",
        egui::Key::E => "E", egui::Key::F => "F", egui::Key::G => "G", egui::Key::H => "H",
        egui::Key::I => "I", egui::Key::J => "J", egui::Key::K => "K", egui::Key::L => "L",
        egui::Key::M => "M", egui::Key::N => "N", egui::Key::O => "O", egui::Key::P => "P",
        egui::Key::Q => "Q", egui::Key::R => "R", egui::Key::S => "S", egui::Key::T => "T",
        egui::Key::U => "U", egui::Key::V => "V", egui::Key::W => "W", egui::Key::X => "X",
        egui::Key::Y => "Y", egui::Key::Z => "Z",
        egui::Key::Num0 => "0", egui::Key::Num1 => "1", egui::Key::Num2 => "2", egui::Key::Num3 => "3",
        egui::Key::Num4 => "4", egui::Key::Num5 => "5", egui::Key::Num6 => "6", egui::Key::Num7 => "7",
        egui::Key::Num8 => "8", egui::Key::Num9 => "9",
        egui::Key::Space => "SPACE", egui::Key::Enter => "ENTER", egui::Key::Escape => "ESCAPE",
        egui::Key::ArrowUp => "UP", egui::Key::ArrowDown => "DOWN",
        egui::Key::ArrowLeft => "LEFT", egui::Key::ArrowRight => "RIGHT",
        egui::Key::Insert => "INSERT", egui::Key::Delete => "DELETE",
        egui::Key::Home => "HOME", egui::Key::End => "END",
        egui::Key::PageUp => "PAGEUP", egui::Key::PageDown => "PAGEDOWN",
        _ => return "UNKNOWN".to_string(), 
    };

    let mut parts = Vec::new();
    if modifiers.ctrl { parts.push("CONTROL"); }
    if modifiers.shift { parts.push("SHIFT"); }
    if modifiers.alt { parts.push("ALT"); }
    if parts.is_empty() { parts.push("NONE"); } 

    parts.push(key_str);
    parts.join("+")
}

fn load_rules() -> Vec<Rule> {
    
    let rules_json = include_str!("../rules.json"); 
    serde_json::from_str(rules_json).unwrap_or_default()
}

fn load_config() -> AppConfig {
    match fs::read_to_string("config.json") {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => {
            
            let cfg = AppConfig::default();
            save_config(&cfg); 
            cfg
        }
    }
}

fn save_config(config: &AppConfig) {
    if let Ok(json) = serde_json::to_string(config) {
        let _ = fs::write("config.json", json);
    }
}

fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = fs::read("C:\\Windows\\Fonts\\times.ttf") {
        fonts.font_data.insert("TimesNewRoman".to_owned(), egui::FontData::from_owned(data));
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "TimesNewRoman".to_owned());
        fonts.families.get_mut(&egui::FontFamily::Monospace).unwrap().insert(0, "TimesNewRoman".to_owned());
        ctx.set_fonts(fonts);
    }
}

fn apply_theme(ctx: &egui::Context, theme_index: usize) {
    let mut visuals = if theme_index == 1 { egui::Visuals::light() } else { egui::Visuals::dark() };
    
    let accent_color = match theme_index {
        0 => egui::Color32::from_rgb(100, 200, 255), // Голубой
        1 => egui::Color32::from_rgb(0, 120, 255),   // Светлая (тема)
        2 => egui::Color32::from_rgb(0, 122, 255),   // Синий
        3 => egui::Color32::from_rgb(220, 50, 50),   // Красный
        4 => egui::Color32::from_rgb(140, 50, 230),  // Фиолетовый
        5 => egui::Color32::from_rgb(255, 140, 0),   // Оранжевый
        6 => egui::Color32::from_rgb(0, 200, 100),   // Зеленый
        7 => egui::Color32::from_rgb(255, 20, 147),  // Розовый
        8 => egui::Color32::from_rgb(212, 175, 55),  // Золотой
        _ => egui::Color32::from_rgb(100, 200, 255),
    };

    
    visuals.extreme_bg_color = if theme_index == 1 {
        egui::Color32::from_gray(245) 
    } else {
        egui::Color32::from_gray(30) 
    };

   
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(80)); 
    visuals.widgets.inactive.rounding = egui::Rounding::same(4.0); 

    
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, accent_color);
    visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
    
    visuals.widgets.hovered.weak_bg_fill = if theme_index == 1 { egui::Color32::from_gray(230) } else { egui::Color32::from_gray(50) };

   
    visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0, accent_color);
    visuals.widgets.active.rounding = egui::Rounding::same(4.0);
    visuals.widgets.active.bg_fill = accent_color.linear_multiply(0.2); 

    
    visuals.selection.bg_fill = accent_color;
    visuals.hyperlink_color = accent_color;
    
    
    visuals.widgets.open.bg_fill = accent_color;
    
    if theme_index == 1 { 
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE); 
    } else {
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::BLACK);
    }

    ctx.set_visuals(visuals);
}

fn start_hotstring_listener(shared_replacements: Arc<Mutex<Vec<ActiveReplacement>>>) {
    let (tx, rx) = mpsc::channel::<ActiveReplacement>();

    thread::spawn(move || {
        while let Ok(rep) = rx.recv() {
            log(&format!("Hotstring triggered: {}", rep.label));
            thread::sleep(Duration::from_millis(50));
            let len = rep.trigger.chars().count();
            for _ in 0..len {
                #[cfg(target_os = "windows")]
                {
                    send_scan_code(0x0E, true); 
                    thread::sleep(Duration::from_millis(10));
                    send_scan_code(0x0E, false); 
                    thread::sleep(Duration::from_millis(10));
                }
            }
            type_in_game(None, rep.text, false, false, None);
        }
    });

    thread::spawn(move || {
        let mut buffer = String::new();
        let callback = move |event: Event| {
            if let EventType::KeyPress(key) = event.event_type {
                if key == rdev::Key::Return || key == rdev::Key::Escape {
                    buffer.clear();
                } 
                else if let Some(name) = event.name {
                    if name == "\u{8}" { 
                        buffer.pop();
                    } else {
                        buffer.push_str(&name);
                        if buffer.len() > 30 { buffer.remove(0); }
                    }

                    if let Ok(replacements) = shared_replacements.lock() {
                        for rep in replacements.iter() {
                            if !rep.trigger.is_empty() && buffer.to_lowercase().ends_with(&rep.trigger.to_lowercase()) {
                                buffer.clear();
                                let _ = tx.send(rep.clone());
                                break; 
                            }
                        }
                    }
                }
            }
        };
        if let Err(error) = listen(callback) {
            log(&format!("Error in hotstring listener: {:?}", error));
        }
    });
}

// ================= GUI =================

#[derive(PartialEq)]
enum F6Tab { Description, Commands, AutoReplace, Events, OrgManager, OnlineTimer, BugReport }
#[derive(PartialEq)]
enum MainTab { Setup, InfoF6, PunishF7, TeleportF8, MpF9, Logs }
#[derive(PartialEq)]
enum F9Tab { Commands, Teleports }

#[derive(PartialEq, Clone, Copy)]
enum BindAction { Main, Punish, Event, Mp, Reload }

#[derive(Debug, Clone, Copy)]
enum HotkeyAction {
    MainMenu,
    PunishMenu,
    EventsMenu,
    MpMenu,
    Reload,
}

struct MyApp {
    config: AppConfig,
    rules: Vec<Rule>,
    orgs: Vec<Organization>,
    active_replacements: Arc<Mutex<Vec<ActiveReplacement>>>,
    current_tab: MainTab,
    f6_tab: F6Tab,
    f9_tab: F9Tab,
    org_input_id: String,
    selected_org_index: usize,
    selected_rank_index: usize,
    cmd_search: String,
    replace_search: String,
    new_rep_trigger: String,
    new_rep_label: String,
    new_rep_text: String,
    timer_start: Instant,
    timer_saved_seconds: u64,
    timer_paused: bool,
    last_reset_day: u32,
    search_text: String,
    input_id: String,
    input_report_num: String,
    input_violation_time: String,
    selected_rule: Option<Rule>, 
    selected_punishment_idx: usize,
    generated_punish_cmd: String, 
    teleport_list: Vec<Teleport>,
    teleport_search: String,
    teleport_category: String,
    hotkey_sender: Sender<AppConfig>, 
    action_receiver: std::sync::mpsc::Receiver<HotkeyAction>,
    is_mp_running: Arc<AtomicBool>,
    waiting_for_key: Option<BindAction>,
    is_admin: bool, 
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        log("Application starting...");
        setup_custom_fonts(&cc.egui_ctx);
        
        let is_admin = check_admin_rights();
        if is_admin { log("Startup: ADMIN RIGHTS = YES"); } else { log("Startup: ADMIN RIGHTS = NO"); }

        let config = load_config();
        let (saved_seconds, last_day) = Self::load_timer();
        
        let raw_data = data::get_auto_replacements();
        let mut combined_replacements = Vec::new();
        for (label, text) in raw_data {
            let trigger = config.saved_triggers.get(label).cloned().unwrap_or_default();
            combined_replacements.push(ActiveReplacement {
                trigger, label: label.to_string(), text: text.to_string(), is_system: true,
            });
        }
        for mut custom in config.custom_replacements.clone() {
            custom.is_system = false;
            combined_replacements.push(custom);
        }
        let shared_replacements = Arc::new(Mutex::new(combined_replacements));
        start_hotstring_listener(shared_replacements.clone());

        let (tx_config, rx_config) = mpsc::channel::<AppConfig>();
        let (tx_action, rx_action) = mpsc::channel::<HotkeyAction>();
        let initial_config = config.clone();
        let ctx_clone = cc.egui_ctx.clone();

        thread::spawn(move || {
            log("Hotkey Thread: Started.");
            let mut manager = GlobalHotKeyManager::new().unwrap();
            
            struct KeyMap { key: HotKey, id: u32, action: HotkeyAction }
            let mut key_map: Vec<KeyMap> = Vec::new();

            let update_registrations = |cfg: &AppConfig, mgr: &mut GlobalHotKeyManager, map: &mut Vec<KeyMap>| {
                let old_keys: Vec<HotKey> = map.iter().map(|k| k.key).collect();
                if !old_keys.is_empty() {
                    let _ = mgr.unregister_all(&old_keys);
                }
                map.clear();
                
                let bindings = [
                    (&cfg.key_main, HotkeyAction::MainMenu),
                    (&cfg.key_punish, HotkeyAction::PunishMenu),
                    (&cfg.key_event, HotkeyAction::EventsMenu),
                    (&cfg.key_mp, HotkeyAction::MpMenu),
                    (&cfg.key_reload, HotkeyAction::Reload),
                ];

                for (bind_str, action) in bindings {
                    if let Some((mods, code)) = parse_hotkey(bind_str) {
                        let mods_opt = if mods.is_empty() { None } else { Some(mods) };
                        let key = HotKey::new(mods_opt, code);
                        
                        if mgr.register(key).is_ok() {
                            map.push(KeyMap { key, id: key.id(), action });
                            log(&format!("Hotkey: Registered {:?} on {}", action, bind_str));
                        } else {
                            log(&format!("Hotkey: FAILED to register {:?} on {}", action, bind_str));
                        }
                    }
                }
            };

            update_registrations(&initial_config, &mut manager, &mut key_map);

            loop {
                if let Ok(new_cfg) = rx_config.try_recv() {
                    log("Hotkey Thread: Config updated.");
                    update_registrations(&new_cfg, &mut manager, &mut key_map);
                }

                while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                    if event.state == HotKeyState::Pressed {
                        if let Some(mapping) = key_map.iter().find(|k| k.id == event.id) {
                            log(&format!("Hotkey Thread: Key Pressed -> {:?}", mapping.action));
                            restore_application_window(&ctx_clone);
                            let _ = tx_action.send(mapping.action);
                        }
                    }
                }

                #[cfg(target_os = "windows")]
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                
                thread::sleep(Duration::from_millis(50));
            }
        });

        let start_tab = if config.admin_name.is_empty() { MainTab::Setup } else { MainTab::InfoF6 };

        Self {
            config,
            rules: load_rules(),
            orgs: data::get_organizations(),
            active_replacements: shared_replacements,
            current_tab: start_tab,
            f6_tab: F6Tab::Description,
            f9_tab: F9Tab::Commands,
            org_input_id: String::new(),
            selected_org_index: 0,
            selected_rank_index: 0,
            cmd_search: String::new(),
            replace_search: String::new(),
            new_rep_trigger: String::new(),
            new_rep_label: String::new(),
            new_rep_text: String::new(),
            timer_start: Instant::now(),
            timer_saved_seconds: saved_seconds,
            timer_paused: false,
            last_reset_day: last_day,
            search_text: String::new(),
            input_id: String::new(),
            input_report_num: String::new(),
            input_violation_time: String::new(),
            selected_rule: None,
            selected_punishment_idx: 0,
            generated_punish_cmd: String::new(),
            teleport_list: data::get_teleports(),
            teleport_search: String::new(),
            teleport_category: "Все события".to_string(),
            hotkey_sender: tx_config,
            action_receiver: rx_action,
            is_mp_running: Arc::new(AtomicBool::new(false)),
            waiting_for_key: None,
            is_admin,
        }
    }

    fn update_hotkeys(&mut self) {
        let _ = self.hotkey_sender.send(self.config.clone());
    }

    fn reset_to_defaults(&mut self) {
        self.config = AppConfig::default();
        save_config(&self.config);
        self.update_hotkeys();
    }

    fn load_timer() -> (u64, u32) {
        if let Ok(data) = fs::read_to_string("timer.json") {
            if let Ok(state) = serde_json::from_str::<TimerState>(&data) { return (state.total_seconds, state.last_reset_day); }
        }
        (0, Local::now().day())
    }
    fn save_timer(&self) {
        let current_seconds = self.get_total_seconds();
        let state = TimerState { total_seconds: current_seconds, last_reset_day: self.last_reset_day };
        if let Ok(json) = serde_json::to_string(&state) { let _ = fs::write("timer.json", json); }
    }
    fn save_triggers(&mut self) {
        if let Ok(replacements) = self.active_replacements.lock() {
            self.config.saved_triggers.clear();
            self.config.custom_replacements.clear();
            for rep in replacements.iter() {
                if rep.is_system {
                    if !rep.trigger.is_empty() { self.config.saved_triggers.insert(rep.label.clone(), rep.trigger.clone()); }
                } else { self.config.custom_replacements.push(rep.clone()); }
            }
        }
        save_config(&self.config);
    }
    fn get_total_seconds(&self) -> u64 {
        if self.timer_paused { self.timer_saved_seconds } else {
            let session_seconds = self.timer_start.elapsed().as_secs();
            self.timer_saved_seconds + session_seconds
        }
    }
    fn check_daily_reset(&mut self) {
        let now = Local::now();
        if now.day() != self.last_reset_day {
            if now.hour() >= 3 {
                self.timer_saved_seconds = 0; self.timer_start = Instant::now(); self.last_reset_day = now.day(); self.save_timer();
            }
        }
    }
    fn reset_timer(&mut self) { self.timer_saved_seconds = 0; self.timer_start = Instant::now(); self.save_timer(); }
    fn get_rule_options(rule: &Rule) -> Vec<PunishmentOption> {
        let mut options = Vec::new();
        if !rule.demorgan.is_empty() { for part in rule.demorgan.split('/') { options.push(PunishmentOption { label: format!("Demorgan {}", part), cmd_base: "/ban".to_string(), time_arg: part.trim().to_string() }); } }
        if !rule.ban.is_empty() { for part in rule.ban.split('/') { options.push(PunishmentOption { label: format!("Ban {}", part), cmd_base: "/ban".to_string(), time_arg: part.trim().to_string() }); } }
        if rule.warn > 0 { options.push(PunishmentOption { label: "Warn".to_string(), cmd_base: "/warn".to_string(), time_arg: "".to_string() }); }
        if !rule.mutev.is_empty() { options.push(PunishmentOption { label: format!("Voice Mute {}", rule.mutev), cmd_base: "/mutevoice".to_string(), time_arg: rule.mutev.clone() }); }
        if !rule.mute_chat.is_empty() { options.push(PunishmentOption { label: format!("Chat Mute {}", rule.mute_chat), cmd_base: "/mutechat".to_string(), time_arg: rule.mute_chat.clone() }); }
        if !rule.mute_report.is_empty() { options.push(PunishmentOption { label: format!("Report Mute {}", rule.mute_report), cmd_base: "/mutereport".to_string(), time_arg: rule.mute_report.clone() }); }
        if !rule.pacifist.is_empty() { options.push(PunishmentOption { label: format!("Pacifist {}", rule.pacifist), cmd_base: "/pacifist".to_string(), time_arg: rule.pacifist.clone() }); }
        if !rule.ban_market.is_empty() { options.push(PunishmentOption { label: format!("Ban Market {}", rule.ban_market), cmd_base: "/ban_market_content_creation".to_string(), time_arg: rule.ban_market.clone() }); }
        options
    }
    fn update_punish_command(&mut self) {
        if let Some(rule) = &self.selected_rule {
            let options = Self::get_rule_options(rule);
            if options.is_empty() { self.generated_punish_cmd = "На данный пункт правила не предусмотрены наказание.".to_string(); return; }
            if self.selected_punishment_idx >= options.len() { self.selected_punishment_idx = 0; }
            let action = &options[self.selected_punishment_idx];
            let mut reason = rule.article.clone();
            if !self.input_violation_time.is_empty() { reason = format!("{} (Ранее {})", reason, self.input_violation_time); }
            if !self.input_report_num.is_empty() {
                let report_str = if self.input_report_num.contains("№") { self.input_report_num.clone() } else { format!("№{}", self.input_report_num) };
                reason = format!("{} | {}", reason, report_str);
            }
            let id = if self.input_id.is_empty() { "ID" } else { &self.input_id };
            self.generated_punish_cmd = if action.time_arg.is_empty() { format!("{} {} {}", action.cmd_base, id, reason) } else { format!("{} {} {} {}", action.cmd_base, id, action.time_arg, reason) };
        }
    }
}

impl eframe::App for MyApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_timer();
        self.save_triggers();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_theme(ctx, self.config.theme_mode);
        ctx.request_repaint_after(Duration::from_millis(100));
        self.check_daily_reset();

        while let Ok(action) = self.action_receiver.try_recv() {
            log(&format!("[UI] Switching tab to: {:?}", action));
            match action {
                HotkeyAction::MainMenu => self.current_tab = MainTab::InfoF6,
                HotkeyAction::PunishMenu => self.current_tab = MainTab::PunishF7,
                HotkeyAction::EventsMenu => self.current_tab = MainTab::TeleportF8,
                HotkeyAction::MpMenu => self.current_tab = MainTab::MpF9,
                HotkeyAction::Reload => restart_app(),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        let accent_color = match self.config.theme_mode {
            0 => egui::Color32::from_rgb(100, 200, 255),
            1 => egui::Color32::from_rgb(0, 120, 255),
            2 => egui::Color32::from_rgb(0, 122, 255),
            3 => egui::Color32::from_rgb(220, 50, 50),
            4 => egui::Color32::from_rgb(140, 50, 230),
            5 => egui::Color32::from_rgb(255, 140, 0),
            6 => egui::Color32::from_rgb(0, 200, 100),
            7 => egui::Color32::from_rgb(255, 20, 147),
            8 => egui::Color32::from_rgb(212, 175, 55),
            _ => egui::Color32::from_rgb(100, 200, 255),
        };

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🛡 AdminHelper").strong().color(accent_color).size(16.0));
                ui.separator();
                ui.selectable_value(&mut self.current_tab, MainTab::InfoF6, "Меню");
                ui.selectable_value(&mut self.current_tab, MainTab::PunishF7, "Наказания");
                ui.selectable_value(&mut self.current_tab, MainTab::TeleportF8, "События");
                ui.selectable_value(&mut self.current_tab, MainTab::MpF9, "Мероприятие");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Настройки").clicked() { self.current_tab = MainTab::Setup; }
                });
                ui.selectable_value(&mut self.current_tab, MainTab::Logs, "🐞 Логи");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(action) = self.waiting_for_key {
                let mut captured = None;
                ctx.input(|i| {
                    for event in &i.events {
                        if let egui::Event::Key { key, pressed: true, modifiers, .. } = event {
                            let bind_str = input_to_bind_string(*key, *modifiers);
                            if bind_str != "UNKNOWN" { 
                                captured = Some(bind_str); 
                                break; 
                            }
                        }
                    }
                });
                
                if let Some(s) = captured {
                    match action {
                        BindAction::Main => self.config.key_main = s,
                        BindAction::Punish => self.config.key_punish = s,
                        BindAction::Event => self.config.key_event = s,
                        BindAction::Mp => self.config.key_mp = s,
                        BindAction::Reload => self.config.key_reload = s,
                    }
                    self.waiting_for_key = None;
                    self.update_hotkeys();
                    save_config(&self.config);
                }
            }

            match self.current_tab {
                MainTab::Setup => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(30.0);
                            ui.heading(egui::RichText::new("⚙ Настройки").size(24.0).strong().color(accent_color));
                            if !self.is_admin {
                                ui.label(egui::RichText::new("⚠ ПРАВА АДМИНИСТРАТОРА НЕ ОБНАРУЖЕНЫ ⚠").size(18.0).color(accent_color).strong());
                                ui.label("Горячие клавиши в игре работать НЕ БУДУТ.");
                            } else {
                                ui.label(egui::RichText::new("✔ Запущено с правами администратора").color(egui::Color32::GREEN));
                            }
                            ui.add_space(20.0);
                        });

                        // Блок выбора темы
                        ui.group(|ui| {
                            ui.heading("🎨 Внешний вид");
                            ui.horizontal(|ui| {
                                ui.label("Тема оформления:");
                                egui::ComboBox::from_id_source("theme_selector")
                                    .selected_text(match self.config.theme_mode {
                                        0 => "🔵 Стандартная (Синяя)",
                                        1 => "☀ Светлая",
                                        2 => "🔷 Темно-синяя",
                                        3 => "🔴 Красная",
                                        4 => "🟣 Фиолетовая",
                                        5 => "🟠 Оранжевая",
                                        6 => "🟢 Зеленая",
                                        7 => "🌸 Розовая",
                                        8 => "👑 Золотая",
                                        _ => "Неизвестная"
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.config.theme_mode, 0, "🔵 Стандартная (Синяя)");
                                        ui.selectable_value(&mut self.config.theme_mode, 1, "☀ Светлая");
                                        ui.selectable_value(&mut self.config.theme_mode, 2, "🔷 Темно-синяя");
                                        ui.selectable_value(&mut self.config.theme_mode, 3, "🔴 Красная");
                                        ui.selectable_value(&mut self.config.theme_mode, 4, "🟣 Фиолетовая");
                                        ui.selectable_value(&mut self.config.theme_mode, 5, "🟠 Оранжевая");
                                        ui.selectable_value(&mut self.config.theme_mode, 6, "🟢 Зеленая");
                                        ui.selectable_value(&mut self.config.theme_mode, 7, "🌸 Розовая");
                                        ui.selectable_value(&mut self.config.theme_mode, 8, "👑 Золотая");
                                    });
                            });
                        });

                        ui.add_space(15.0);
                        ui.group(|ui| {
                            ui.heading("⌨ Горячие клавиши");
                            ui.label(egui::RichText::new("⚠ ВАЖНО: Переключите раскладку клавиатуры на английскую перед назначением клавиш!").color(accent_color).strong());
                            ui.label("Кликните на кнопку, затем зажмите комбинацию (например: Alt + R).");
                            ui.add_space(5.0);
                            egui::Grid::new("setup_keys").num_columns(2).spacing([20.0, 10.0]).show(ui, |ui| {
                                let btn_size = [150.0, 25.0];
                                ui.label("Основное меню:");
                                let txt1 = if self.waiting_for_key == Some(BindAction::Main) { "Нажмите клавиши...".to_string() } else { self.config.key_main.replace("NONE+", "") };
                                if ui.add_sized(btn_size, egui::Button::new(txt1)).clicked() { self.waiting_for_key = Some(BindAction::Main); }
                                ui.end_row();
                                ui.label("Меню наказаний:");
                                let txt2 = if self.waiting_for_key == Some(BindAction::Punish) { "Нажмите клавиши...".to_string() } else { self.config.key_punish.replace("NONE+", "") };
                                if ui.add_sized(btn_size, egui::Button::new(txt2)).clicked() { self.waiting_for_key = Some(BindAction::Punish); }
                                ui.end_row();
                                ui.label("Меню событий:");
                                let txt3 = if self.waiting_for_key == Some(BindAction::Event) { "Нажмите клавиши...".to_string() } else { self.config.key_event.replace("NONE+", "") };
                                if ui.add_sized(btn_size, egui::Button::new(txt3)).clicked() { self.waiting_for_key = Some(BindAction::Event); }
                                ui.end_row();
                                ui.label("Меню МП:");
                                let txt4 = if self.waiting_for_key == Some(BindAction::Mp) { "Нажмите клавиши...".to_string() } else { self.config.key_mp.replace("NONE+", "") };
                                if ui.add_sized(btn_size, egui::Button::new(txt4)).clicked() { self.waiting_for_key = Some(BindAction::Mp); }
                                ui.end_row();
                                ui.label("Перезагрузка скрипта:");
                                let txt5 = if self.waiting_for_key == Some(BindAction::Reload) { "Нажмите клавиши...".to_string() } else { self.config.key_reload.replace("NONE+", "") };
                                if ui.add_sized(btn_size, egui::Button::new(txt5)).clicked() { self.waiting_for_key = Some(BindAction::Reload); }
                                ui.end_row();
                            });
                        });
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            
                            if ui.add_sized([200.0, 40.0], egui::Button::new(egui::RichText::new("⚠ Сбросить все настройки").color(accent_color))).clicked() {
                                self.reset_to_defaults();
                            }
                        });
                    });
                },
                MainTab::Logs => {
                     ui.heading("Диагностика и Логи");
                     egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                         if let Ok(logs) = get_logs().lock() {
                             for log in logs.iter() { ui.monospace(log); }
                         }
                     });
                },
                MainTab::InfoF6 => {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.f6_tab, F6Tab::Description, "Описание");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::Commands, "Команды");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::AutoReplace, "Автозамены");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::Events, "Мероприятия");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::OrgManager, "Организация");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::OnlineTimer, "Активность");
                        ui.selectable_value(&mut self.f6_tab, F6Tab::BugReport, "Баг-репорт");
                    });
                    ui.separator();
                    match self.f6_tab {
                        F6Tab::Description => {
                            ui.heading("AdminHelper - Руководство"); ui.add_space(10.0);
                            egui::ScrollArea::vertical().id_source("desc_scroll").show(ui, |ui| {
                                ui.label(egui::RichText::new(format!("{} - Основное меню", self.config.key_main.replace("NONE+", ""))).strong().color(accent_color));
                                ui.label("• Команды: Поиск и быстрая отправка команд в чат.");
                                ui.label("• Автозамены: Готовые фразы (настройте триггеры).");
                                ui.label("• Мероприятие: Памятка по проведению мероприятия.");
                                ui.label("• Организация: Быстрая выдача рангов игрокам.");
                                ui.label("• Онлайн: Счетчик времени администрирования.");
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new(format!("{} - Система наказаний", self.config.key_punish.replace("NONE+", ""))).strong().color(accent_color));
                                ui.label("• Слева: Список всех правил сервера.");
                                ui.label("• Справа: Авто-генерация команды (/ban, /warn) с учетом времени и номера ЖБ.");
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new(format!("{} - Телепорты", self.config.key_event.replace("NONE+", ""))).strong().color(accent_color));
                                ui.label("• Быстрые телепорты по важным точкам (МП, Зоны).");
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new(format!("{} - Менеджер мероприятий", self.config.key_mp.replace("NONE+", ""))).strong().color(accent_color));
                                ui.label("• Сеты команд для автоматического проведения ивентов.");
                                ui.label("• Телепорты в интерьеры для МП.");
                                ui.add_space(10.0);
                            });
                        },
                        F6Tab::Commands => {
                            ui.horizontal(|ui| { ui.label("Поиск:"); ui.text_edit_singleline(&mut self.cmd_search); });
                            
                            egui::ScrollArea::vertical().id_source("f6_cmd_scroll").show(ui, |ui| {
                                for (cmd, desc) in data::get_admin_commands() {
                                    // Фильтр поиска
                                    if self.cmd_search.is_empty() || cmd.to_lowercase().contains(&self.cmd_search.to_lowercase()) {
                                        
                                        // === НОВАЯ ЛОГИКА ===
                                        // Проверяем описание: если есть '[', значит нужны аргументы.
                                        let needs_args = desc.contains('[');
                                        
                                        // Если нужны аргументы -> Enter НЕ жмем (false), иначе жмем (true)
                                        let press_enter = !needs_args;
                                        
                                        // Если нужны аргументы -> добавляем пробел в конце, чтобы сразу писать ID
                                        let text_to_type = if needs_args { format!("{} ", cmd) } else { cmd.to_string() };

                                        // === СТАРЫЙ СТИЛЬ ===
                                        // Обычная кнопка с форматом "Команда - Описание"
                                        if ui.button(format!("{} - {}", cmd, desc)).clicked() { 
                                            type_in_game(Some(ctx.clone()), text_to_type, true, press_enter, None); 
                                        }
                                    }
                                }
                            });
                        },
                        F6Tab::AutoReplace => {
                            
                            ui.horizontal(|ui| {
                                ui.label("🔎 Поиск:");
                               
                                let available_width = ui.available_width() - 140.0; 
                                ui.add(egui::TextEdit::singleline(&mut self.replace_search).desired_width(available_width));
                                
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("💾 Сохранить всё").clicked() { self.save_triggers(); }
                                });
                            });
                            
                            ui.separator();
                            ui.add_space(5.0);

                            
                            ui.columns(2, |columns| {
                                
                                columns[0].vertical(|ui| {
                                    ui.heading(egui::RichText::new("👤 Мои автозамены").color(accent_color));
                                    ui.add_space(5.0);
                                    
                                    
                                    ui.group(|ui| {
                                        ui.label(egui::RichText::new("➕ Создать новую").strong());
                                        
                                        
                                        egui::Grid::new("new_rep_grid").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
                                            ui.label("Бинд:");
                                            ui.add(egui::TextEdit::singleline(&mut self.new_rep_trigger).desired_width(f32::INFINITY).hint_text("п1"));
                                            ui.end_row();

                                            ui.label("Описание:");
                                            ui.add(egui::TextEdit::singleline(&mut self.new_rep_label).desired_width(f32::INFINITY).hint_text("Коротко об бинде"));
                                            ui.end_row();

                                            ui.label("Текст:");
                                            ui.add(egui::TextEdit::multiline(&mut self.new_rep_text).desired_width(f32::INFINITY).desired_rows(3));
                                            ui.end_row();
                                        });

                                        ui.add_space(5.0);
                                        ui.vertical_centered_justified(|ui| {
                                            if ui.button("Добавить").clicked() {
                                                if !self.new_rep_trigger.is_empty() && !self.new_rep_text.is_empty() {
                                                    if let Ok(mut replacements) = self.active_replacements.lock() {
                                                        replacements.push(ActiveReplacement {
                                                            trigger: self.new_rep_trigger.clone(),
                                                            label: self.new_rep_label.clone(),
                                                            text: self.new_rep_text.clone(),
                                                            is_system: false,
                                                        });
                                                        self.new_rep_trigger.clear(); self.new_rep_label.clear(); self.new_rep_text.clear();
                                                    }
                                                    self.save_triggers();
                                                }
                                            }
                                        });
                                    });

                                    ui.add_space(10.0);
                                    ui.separator();
                                    
                                    
                                    egui::ScrollArea::vertical().id_source("custom_rep_scroll").max_height(ui.available_height() - 20.0).show(ui, |ui| {
                                        if let Ok(mut replacements) = self.active_replacements.lock() {
                                            
                                            let mut to_remove = None;
                                            
                                            for (idx, rep) in replacements.iter_mut().enumerate() {
                                                if rep.is_system { continue; }
                                                if !self.replace_search.is_empty() && !rep.label.to_lowercase().contains(&self.replace_search.to_lowercase()) { continue; }
                                                
                                                ui.group(|ui| {
                                                    ui.set_width(ui.available_width()); 
                                                    
                                                    
                                                    ui.horizontal(|ui| {
                                                        ui.label(egui::RichText::new(&rep.label).strong().color(accent_color));
                                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                            if ui.button("🗑").clicked() { to_remove = Some(idx); }
                                                        });
                                                    });
                                                    
                                                    
                                                    ui.horizontal(|ui| { 
                                                        ui.label("Бинд:"); 
                                                        ui.add(egui::TextEdit::singleline(&mut rep.trigger).desired_width(50.0)); 
                                                    });
                                                    
                                                    
                                                    let oneline = rep.text.replace("\n", " ");
                                                    let preview = if oneline.chars().count() > 50 { 
                                                        format!("{}...", oneline.chars().take(50).collect::<String>()) 
                                                    } else { 
                                                        oneline 
                                                    };
                                                    
                                                    let resp = ui.label(egui::RichText::new(preview).weak().size(11.0));
                                                    if resp.hovered() { egui::show_tooltip_text(ui.ctx(), ui.id(), &rep.text); }
                                                });
                                            }

                                            
                                            if let Some(idx) = to_remove {
                                                replacements.remove(idx);
                                            }
                                        }
                                    });
                                });

                                
                                columns[1].vertical(|ui| {
                                    ui.heading(egui::RichText::new("🔧 Стандартные").color(accent_color));
                                    ui.separator();
                                    
                                    egui::ScrollArea::vertical().id_source("system_rep_scroll").show(ui, |ui| {
                                        
                                        let total_width = ui.available_width();
                                        
                                        let width_bind = 50.0;
                                        let width_desc = 180.0;
                                        let width_text = (total_width - width_bind - width_desc - 30.0).max(100.0);

                                        egui::Grid::new("sys_grid")
                                            .striped(true)
                                            .spacing([10.0, 10.0])
                                            .min_col_width(50.0)
                                            .show(ui, |ui| {
                                                // Заголовки таблицы
                                                ui.label(egui::RichText::new("Бинд").strong()); 
                                                ui.label(egui::RichText::new("Описание").strong()); 
                                                ui.label(egui::RichText::new("Текст").strong()); 
                                                ui.end_row();
                                                
                                                if let Ok(mut replacements) = self.active_replacements.lock() {
                                                    for rep in replacements.iter_mut() {
                                                        if !rep.is_system { continue; }
                                                        
                                                        if !self.replace_search.is_empty() && !rep.label.to_lowercase().contains(&self.replace_search.to_lowercase()) {
                                                            continue;
                                                        }

                                                        
                                                        ui.add(egui::TextEdit::singleline(&mut rep.trigger).desired_width(width_bind).hint_text("..."));
                                                        
                                                        
                                                        ui.add_sized([width_desc, 20.0], egui::Label::new(&rep.label).truncate(true));
                                                        
                                                        
                                                        let oneline = rep.text.replace("\n", " ");
                                                        
                                                        let resp = ui.add_sized(
                                                            [width_text, 20.0], 
                                                            egui::Label::new(oneline).truncate(true)
                                                        );
                                                        
                                                        if resp.hovered() { 
                                                            egui::show_tooltip_text(ui.ctx(), ui.id(), &rep.text); 
                                                        }
                                                        
                                                        ui.end_row();
                                                    }
                                                }
                                            });
                                    });
                                });
                            });
                        },
                        F6Tab::Events => {
                            egui::ScrollArea::vertical().id_source("f6_events_scroll").show(ui, |ui| {
                                ui.heading("📋 Порядок проведения мероприятия");
                                ui.separator();
                                ui.collapsing("1. Подготовка и Сбор", |ui| {
                                    ui.label("1. Выберите место проведения (см. F9 Телепорты).");
                                    ui.label("2. Переместитесь в дименшин и уведомить других администраторов (/dim 824151 3).");
                                    ui.label("3. Спросите у других кто хочет участвовать на меропрятие.");
                                    ui.label("4. Подготовьте все необходимое для проведения мероприятия.");
                                });
                                ui.collapsing("2. Начало мероприятия", |ui| {
                                    ui.label("Откройте телепорт выставите кол-во игроков время проведение и название мероприятие (/gomp 30 30 Прятки)");
                                    ui.label("После того как соберутся игроки нужно их выстрить и объяснить суть мероприятия.");
                                    ui.label("Также объяснить что запрещено делать.");
                                });
                                ui.collapsing("3. Выдача экипировки", |ui| {
                                    ui.label("1. Выдать все необходимые предметы для игроков.");
                                    ui.label("2. Выдайте ХП и броню для игроков.");
                                    ui.label("3. Начать мероприятие написав в чат мероприятие 'Начали!'.");
                                });
                                ui.collapsing("4. Финал и Приз", |ui| {
                                    ui.label("1. Проследите за мероприятием и его нарушениеми");
                                    ui.label("2. Закрыть димешшин.");
                                    ui.label("3. После определние победителя - объявите его.");
                                    ui.label("4. Отправить отчёт об проведённом мероприятии.");
                                });
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new("Примечание: Не забудьте что начало и конец мероприятие нужно скринить а также используйте F9 для автоматизации команд.").italics().color(accent_color));
                            });
                        },
                        F6Tab::OrgManager => {
                            ui.vertical_centered(|ui| {
                                ui.add_space(10.0);
                                ui.heading(egui::RichText::new("👔 Управление Фракцией").size(20.0).strong().color(accent_color));
                            });
                            
                            ui.add_space(10.0);
                            
                            ui.group(|ui| {
                                egui::Grid::new("org_grid")
                                    .num_columns(2)
                                    .spacing([15.0, 15.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        
                                        ui.label(egui::RichText::new("👤 ID Игрока:").strong().color(accent_color));
                                        ui.add(egui::TextEdit::singleline(&mut self.org_input_id)
                                            .desired_width(120.0)
                                            .hint_text("12156"));
                                        ui.end_row();

                                        ui.label(egui::RichText::new("🏰 Организация:").strong().color(accent_color));
                                        let current_org_name = self.orgs[self.selected_org_index].name.clone();
                                        egui::ComboBox::from_id_source("org_selector")
                                            .selected_text(current_org_name)
                                            .width(220.0)
                                            .show_ui(ui, |ui| {
                                                for (i, org) in self.orgs.iter().enumerate() { 
                                                    if ui.selectable_value(&mut self.selected_org_index, i, &org.name).changed() { 
                                                        self.selected_rank_index = 0; 
                                                    } 
                                                }
                                            });
                                        ui.end_row();

                                        ui.label(egui::RichText::new("⭐ Ранг:").strong().color(accent_color));
                                        let current_org = &self.orgs[self.selected_org_index];
                                        if !current_org.ranks.is_empty() {
                                            let current_rank_name = current_org.ranks[self.selected_rank_index].name.clone();
                                            egui::ComboBox::from_id_source("rank_selector")
                                                .selected_text(current_rank_name)
                                                .width(220.0)
                                                .show_ui(ui, |ui| {
                                                    for (i, rank) in current_org.ranks.iter().enumerate() { 
                                                        ui.selectable_value(&mut self.selected_rank_index, i, &rank.name); 
                                                    }
                                                });
                                        } else { 
                                            ui.label(egui::RichText::new("Нет рангов").italics()); 
                                        }
                                        ui.end_row();
                                    });
                            });

                            ui.add_space(15.0);
                            
                            
                            ui.vertical_centered(|ui| {
                                
                                let btn_text_color = if self.config.theme_mode == 1 { 
                                    egui::Color32::BLACK 
                                } else { 
                                    egui::Color32::WHITE 
                                };

                                if ui.add_sized(
                                    [180.0, 35.0], 
                                    egui::Button::new(egui::RichText::new("ВЫДАТЬ РАНГ").strong().color(btn_text_color))
                                ).clicked() {
                                    let org = &self.orgs[self.selected_org_index];
                                    if !org.ranks.is_empty() {
                                        let rank = &org.ranks[self.selected_rank_index];
                                        let cmd = format!("/setfactionrank {} {} {}", self.org_input_id, org.key, rank.id);
                                        type_in_game(Some(ctx.clone()), cmd, true, true, None);
                                    }
                                }
                            });
                        },
                        F6Tab::OnlineTimer => {
                            let s = self.get_total_seconds();
                            let hours = s / 3600;
                            let mins = (s % 3600) / 60;
                            let secs = s % 60;
                            let time_str = format!("{:02}:{:02}:{:02}", hours, mins, secs);

                            ui.vertical_centered(|ui| {
                                ui.add_space(15.0);
                                
                               
                                if self.timer_paused {
                                    ui.label(egui::RichText::new("💤 Таймер на паузе").color(egui::Color32::from_rgb(255, 200, 0))); // Желтый
                                } else {
                                    ui.label(egui::RichText::new("🔥 Таймер активен").color(accent_color)); // Цвет темы
                                }

                                ui.add_space(5.0);

                                
                                ui.label(egui::RichText::new(time_str)
                                    .size(45.0) 
                                    .strong()
                                    .monospace()
                                    .color(accent_color));

                                ui.add_space(20.0);

                                ui.horizontal(|ui| {
                                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center), |ui| {
                                        
                                        
                                        let (btn_text, btn_icon) = if self.timer_paused { ("Продолжить", "▶") } else { ("Пауза", "⏸") };
                                        
                                        if ui.add_sized([130.0, 35.0], egui::Button::new(
                                            format!("{} {}", btn_icon, btn_text)
                                        )).clicked() { 
                                            self.timer_paused = !self.timer_paused; 
                                            if self.timer_paused { 
                                                self.timer_saved_seconds += self.timer_start.elapsed().as_secs(); 
                                            } else { 
                                                self.timer_start = Instant::now(); 
                                            } 
                                        }

                                        ui.add_space(10.0);

                                        if ui.add_sized([130.0, 35.0], egui::Button::new("🔄 Сбросить")).clicked() { 
                                            self.reset_timer(); 
                                        }
                                    });
                                });
                                
                                ui.add_space(15.0);
                                ui.separator();
                                ui.label(egui::RichText::new("Автоматический сброс в 03:00 утра.").weak().size(12.0));
                            });
                        },
                        F6Tab::BugReport => {
                            egui::ScrollArea::vertical().id_source("bug_report_scroll").show(ui, |ui| {
                                ui.heading("🐞 Регламент работы с багами");
                                ui.separator();
                                ui.label(egui::RichText::new("При фиксации бага администратор должен запросить у игрока:").strong().color(accent_color));
                                ui.add_space(5.0);
                                ui.label("1. Дату возникновения бага.");
                                ui.label("2. Суть проблемы (текстовое описание).");
                                ui.label("3. Доказательства (предпочтительно видео, в отдельных случаях скриншот).");
                                ui.label("4. Логи (при серьёзных багах: бессмертие, бесконечные патроны, пропавшие текстуры, вылеты, ошибки).");

                                ui.add_space(15.0);
                                ui.heading("📂 Как выгрузить логи?");
                                ui.label("Файлы логов находятся в папке RAGEMP по следующему пути:");
                                ui.add_space(5.0);
                                ui.monospace("RAGEMP\\clientdata\\console.txt");
                                ui.monospace("RAGEMP\\clientdata\\cef_game_logs.txt");
                                ui.monospace("RAGEMP\\clientdata\\cef_launcher_log.txt");
                                ui.monospace("RAGEMP\\clientdata\\main_logs.txt");

                                ui.add_space(15.0);
                                ui.heading("📝 Порядок действий:");
                                ui.label("1. Выдели все файлы (Ctrl + ЛКМ) и нажми правую кнопку мыши.");
                                ui.label("2. Выбери «Добавить в архив» (WinRAR/7-Zip).");
                                ui.label("3. Открой Google Диск и нажми «Создать».");
                                ui.label("4. Выбери «Загрузить файлы» и дождись загрузки.");
                                ui.label("5. Настрой доступ к файлу (доступ по ссылке).");
                                ui.label("6. Отправь ссылку на архив с откатом в Discord, в канал #баг-репорт.");
                            });
                        },
                    }
                },
                MainTab::PunishF7 => {
                    ui.columns(2, |columns| {
                        columns[0].vertical(|ui| {
                            ui.horizontal(|ui| { ui.label("🔎"); ui.text_edit_singleline(&mut self.search_text); if !self.search_text.is_empty() && ui.button("X").clicked() { self.search_text.clear(); } }); ui.separator();
                            egui::ScrollArea::vertical().id_source("f7_list_scroll").show(ui, |ui| {
                                let query = self.search_text.to_lowercase();
                                let mut picked_rule: Option<Rule> = None;
                                for rule in &self.rules {
                                    if query.is_empty() || rule.article.to_lowercase().contains(&query) || rule.title.to_lowercase().contains(&query) {
                                        if ui.add_sized([ui.available_width(), 20.0], egui::Button::new(format!("{} - {}", rule.article, rule.title))).clicked() { picked_rule = Some(rule.clone()); }
                                    }
                                }
                                if let Some(rule) = picked_rule { self.selected_rule = Some(rule); self.selected_punishment_idx = 0; self.update_punish_command(); }
                            });
                        });
                        columns[1].vertical(|ui| {
                            let current_rule = self.selected_rule.clone();
                            if let Some(rule) = current_rule {
                                ui.heading(format!("{} {}", rule.article, rule.title)); ui.separator();
                                egui::ScrollArea::vertical().id_source("f7_desc_scroll").max_height(300.0).show(ui, |ui| { let clean_desc = rule.description.replace("`n", "\n");ui.label(egui::RichText::new(clean_desc).italics()); }); ui.separator();
                                egui::Grid::new("punish_inputs").spacing([10.0, 10.0]).show(ui, |ui| {
                                    ui.label("ID:"); if ui.add(egui::TextEdit::singleline(&mut self.input_id).desired_width(100.0)).changed() { self.update_punish_command(); } ui.end_row();
                                    ui.label("Время:"); if ui.add(egui::TextEdit::singleline(&mut self.input_violation_time).desired_width(100.0)).changed() { self.update_punish_command(); } ui.end_row();
                                    ui.label("ЖБ:"); if ui.add(egui::TextEdit::singleline(&mut self.input_report_num).desired_width(100.0)).changed() { self.update_punish_command(); } ui.end_row();
                                }); ui.separator();
                                let options = Self::get_rule_options(&rule);
                                for (i, opt) in options.iter().enumerate() { if ui.radio_value(&mut self.selected_punishment_idx, i, &opt.label).changed() { self.update_punish_command(); } } ui.separator();
                                ui.add_sized([ui.available_width(), 30.0], egui::TextEdit::multiline(&mut self.generated_punish_cmd));
                                ui.horizontal(|ui| {
                                    if ui.button("📋 Копировать").clicked() { if let Ok(mut clipboard) = Clipboard::new() { let _ = clipboard.set_text(self.generated_punish_cmd.clone()); } }
                                    if ui.button("🚀 Выдать (Enter)").clicked() { type_in_game(Some(ctx.clone()), self.generated_punish_cmd.clone(), true, true, None); }
                                });
                            } else { ui.label("Выберите правило слева"); }
                        });
                    });
                },
                MainTab::TeleportF8 => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label("📂 Категория:");
                            egui::ComboBox::from_id_source("tp_cat").selected_text(&self.teleport_category).width(180.0)
                                .show_ui(ui, |ui| {
                                     ui.selectable_value(&mut self.teleport_category, "Все события".to_string(), "Все события");
                                     ui.selectable_value(&mut self.teleport_category, "Налёты".to_string(), "Налёты");
                                     ui.selectable_value(&mut self.teleport_category, "Захват Районов".to_string(), "Захват Районов");
                                     ui.selectable_value(&mut self.teleport_category, "Захват территорий".to_string(), "Захват территорий");
                                     ui.selectable_value(&mut self.teleport_category, "Поставки, ограбление".to_string(), "Поставки");
                                     ui.selectable_value(&mut self.teleport_category, "ВЗК, ВЗА".to_string(), "ВЗК, ВЗА");
                                });
                            ui.add_space(15.0);
                            ui.label("🔍 Поиск:");
                            ui.add(egui::TextEdit::singleline(&mut self.teleport_search).desired_width(120.0));
                        });
                    });
                    ui.add_space(10.0); ui.separator(); ui.add_space(5.0);
                    ui.scope(|ui| {
                        let style = ui.style_mut();
                        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(70));
                        style.visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
                        style.visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
                        style.visuals.widgets.active.rounding = egui::Rounding::same(6.0);

                        egui::ScrollArea::vertical().id_source("f8_tp_scroll").show(ui, |ui| {
                                let spacing_x = 10.0;
                                let btn_width = (ui.available_width() - spacing_x - 8.0) / 2.0;
                                egui::Grid::new("tp_grid").num_columns(2).spacing([spacing_x, 10.0]).striped(true).show(ui, |ui| {
                                        let query = self.teleport_search.to_lowercase();
                                        let mut c = 0;
                                        for tp in &self.teleport_list {
                                            if (self.teleport_category == "Все события" || tp.category == self.teleport_category)
                                                && (query.is_empty() || tp.name.to_lowercase().contains(&query))
                                            {
                                                let btn_text = egui::RichText::new(&tp.name).size(14.0);
                                                if ui.add_sized([btn_width, 28.0], egui::Button::new(btn_text)).clicked() { run_teleport(ctx, &tp.command); }
                                                c += 1;
                                                if c % 2 == 0 { ui.end_row(); }
                                            }
                                        }
                                });
                        });
                    });
                },
                MainTab::MpF9 => {
                    ui.heading("Менеджер мероприятий"); ui.separator();
                    ui.horizontal(|ui| { ui.selectable_value(&mut self.f9_tab, F9Tab::Commands, "Команды"); ui.selectable_value(&mut self.f9_tab, F9Tab::Teleports, "Телепорты"); }); ui.separator();

                    let is_running = self.is_mp_running.load(Ordering::Relaxed);
                    if is_running {
                        ui.colored_label(egui::Color32::RED, "⏳ Выполняется команда... Подождите");
                    }
                    ui.set_enabled(!is_running);

                    match self.f9_tab {
                        F9Tab::Commands => {
                            egui::ScrollArea::vertical().id_source("f9_cmd_scroll").show(ui, |ui| {
                                egui::Grid::new("mp_c").striped(true).spacing([10.0, 10.0]).show(ui, |ui| {
                                    let presets = data::get_mp_commands(&self.config.admin_id);
                                    for (i, p) in presets.iter().enumerate() {
                                        if ui.add_sized([250.0, 30.0], egui::Button::new(&p.button_name)).clicked() {
                                            let cmds = p.commands.clone();
                                            self.is_mp_running.store(true, Ordering::Relaxed);
                                            let flag = self.is_mp_running.clone();

                                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));

                                            thread::spawn(move || {
                                                 thread::sleep(Duration::from_millis(500));
                                                 log("MP Thread: Starting commands execution...");
                                                 for cmd in cmds {
                                                     type_in_game(None, cmd, true, true, None);
                                                     thread::sleep(Duration::from_millis(1500));
                                                 }
                                                 flag.store(false, Ordering::Relaxed);
                                            });
                                        }
                                        if (i + 1) % 2 == 0 { ui.end_row(); }
                                    }
                                });
                            });
                        },
                        F9Tab::Teleports => {
                            egui::ScrollArea::vertical().id_source("f9_tp_scroll").show(ui, |ui| {
                                egui::Grid::new("mp_tp_grid").striped(true).spacing([10.0, 10.0]).show(ui, |ui| {
                                    let teleports = data::get_mp_teleports();
                                    for (i, (name, coords)) in teleports.iter().enumerate() {
                                        if ui.add_sized([250.0, 30.0], egui::Button::new(*name)).clicked() { run_teleport(ctx, *coords); } if (i + 1) % 2 == 0 { ui.end_row(); }
                                    }
                                });
                            });
                        },
                    }
                },
            }
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AdminHelper")
            .with_inner_size([750.0, 800.0])
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native("AdminHelper", options, Box::new(|cc| Box::new(MyApp::new(cc))))
}