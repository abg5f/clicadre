//! Raccourcis globaux : Impr. écran (Windows/Linux) + un raccourci de secours.
//!
//! Deux pièges Windows, gérés ici :
//! - Impr. écran avec une touche de modification encore enfoncée (Alt après un Alt+Tab,
//!   Ctrl, Maj) n'est pas « Impr. écran » pour le système : sans réservation, Windows
//!   copie alors l'écran ou la fenêtre active en silence. On réserve donc aussi ces variantes.
//! - Une réservation peut être perdue en cours de session (veille, reprise, autre logiciel
//!   de capture). Une surveillance vérifie régulièrement que la touche est toujours à nous
//!   et la reprend sinon, sans qu'il faille relancer l'app.

use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::AppState;

#[cfg(target_os = "macos")]
pub const FALLBACK_LABEL: &str = "Cmd+Shift+2";
#[cfg(not(target_os = "macos"))]
pub const FALLBACK_LABEL: &str = "Ctrl+Shift+X";

fn fallback() -> Shortcut {
    #[cfg(target_os = "macos")]
    return Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Digit2);
    #[cfg(not(target_os = "macos"))]
    return Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyX);
}

/// Impr. écran seule et avec Alt, Ctrl, Maj (et leurs combinaisons).
/// Win+Impr. écran reste au système (capture enregistrée dans Images › Captures d'écran).
fn printscreen_variants() -> Vec<Shortcut> {
    let a = Modifiers::ALT;
    let c = Modifiers::CONTROL;
    let s = Modifiers::SHIFT;
    [None, Some(a), Some(c), Some(s), Some(a | s), Some(c | s), Some(c | a), Some(c | a | s)]
        .into_iter()
        .map(|m| Shortcut::new(m, Code::PrintScreen))
        .collect()
}

pub fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                crate::start_capture(app);
            }
        })
        .build()
}

/// (Ré)enregistre les raccourcis. Un échec sur Impr. écran n'est pas bloquant :
/// le raccourci de secours reste disponible et l'éditeur explique comment libérer la touche.
pub fn register_all(app: &AppHandle) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if let Err(err) = gs.register(fallback()) {
        eprintln!("[glossyshots] raccourci de secours indisponible : {err}");
    }
    let mut ok = false;
    if !cfg!(target_os = "macos") {
        for (i, shortcut) in printscreen_variants().into_iter().enumerate() {
            let registered = gs.register(shortcut).is_ok();
            // Seule la touche nue compte pour l'état affiché dans les Réglages.
            if i == 0 {
                ok = registered;
            }
        }
    }
    app.state::<AppState>()
        .printscreen_registered
        .store(ok, Ordering::SeqCst);
}

/// Lance la surveillance de la touche Impr. écran (Windows).
pub fn start_watchdog(app: &AppHandle) {
    #[cfg(windows)]
    {
        let app = app.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            if printscreen_is_free() {
                eprintln!("[glossyshots] Impr. écran n'était plus réservée : nouvelle réservation");
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || register_all(&handle));
            }
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Vrai si personne ne réserve Impr. écran. Test sans effet de bord : on tente une
/// réservation éphémère (refusée si la touche est déjà prise), relâchée aussitôt.
#[cfg(windows)]
fn printscreen_is_free() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, VK_SNAPSHOT};
    const PROBE_ID: i32 = 0x6C55;
    // SAFETY : appels Win32 simples, sans fenêtre (hwnd nul) ni pointeur fourni.
    unsafe {
        if RegisterHotKey(std::ptr::null_mut(), PROBE_ID, 0, u32::from(VK_SNAPSHOT)) != 0 {
            UnregisterHotKey(std::ptr::null_mut(), PROBE_ID);
            true
        } else {
            false
        }
    }
}
