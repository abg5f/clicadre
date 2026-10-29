//! Glossyshots : cœur natif.
//!
//! Flux : raccourci global → capture de l'écran sous le curseur (xcap) →
//! fenêtre `overlay` (sélection de zone sur l'image figée) → découpe →
//! fenêtre `editor` (flou, annotations, fond) → presse-papiers / PNG.
//!
//! Les pixels transitent en binaire brut (IPC `Response` / corps `Raw`),
//! jamais en base64 : c'est ce qui garde l'ouverture de l'overlay instantanée.

mod settings;
mod shortcut;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WindowEvent};

pub use settings::Settings;

/// Image RGBA 8 bits, non prémultipliée.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Shot {
    /// Sérialise au format attendu par le front : largeur u32 LE, hauteur u32 LE, pixels.
    fn to_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.rgba.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.rgba);
        out
    }

    fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Shot {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let w = w.clamp(1, self.width - x);
        let h = h.clamp(1, self.height - y);
        let stride = self.width as usize * 4;
        let row = w as usize * 4;
        let mut rgba = Vec::with_capacity(row * h as usize);
        for r in y..y + h {
            let start = r as usize * stride + x as usize * 4;
            rgba.extend_from_slice(&self.rgba[start..start + row]);
        }
        Shot { width: w, height: h, rgba }
    }
}

#[cfg(test)]
mod tests {
    use super::Shot;

    /// Image 4×3 dont chaque pixel encode sa position : (x, y, 0, 255).
    fn grid() -> Shot {
        let mut rgba = Vec::new();
        for y in 0..3u8 {
            for x in 0..4u8 {
                rgba.extend_from_slice(&[x, y, 0, 255]);
            }
        }
        Shot { width: 4, height: 3, rgba }
    }

    #[test]
    fn crop_keeps_the_right_pixels() {
        let c = grid().crop(1, 1, 2, 2);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.rgba, vec![1, 1, 0, 255, 2, 1, 0, 255, 1, 2, 0, 255, 2, 2, 0, 255]);
    }

    #[test]
    fn crop_is_clamped_to_the_image() {
        let c = grid().crop(3, 2, 50, 50);
        assert_eq!((c.width, c.height), (1, 1));
        assert_eq!(c.rgba, vec![3, 2, 0, 255]);
    }

    /// Écrit réellement dans le presse-papiers : à lancer à la main
    /// (`cargo test -- --ignored`), idéalement pendant qu'un autre processus l'occupe.
    #[test]
    #[ignore]
    fn clipboard_write_survives_a_busy_clipboard() {
        let started = std::time::Instant::now();
        super::write_clipboard_image(2, 2, vec![255; 16]).expect("écriture du presse-papiers");
        eprintln!("presse-papiers écrit en {:?}", started.elapsed());
    }

    /// Stress : à lancer pendant qu'un autre processus ouvre/ferme le presse-papiers en boucle.
    /// Compare un essai unique (ancien comportement) aux essais répétés.
    #[test]
    #[ignore]
    fn clipboard_stress_single_vs_retry() {
        let n = 150;
        let single = (0..n)
            .filter(|_| {
                arboard::Clipboard::new()
                    .and_then(|mut c| {
                        c.set_image(arboard::ImageData {
                            width: 2,
                            height: 2,
                            bytes: std::borrow::Cow::Owned(vec![255; 16]),
                        })
                    })
                    .is_err()
            })
            .count();
        let retry = (0..n)
            .filter(|_| super::write_clipboard_image(2, 2, vec![255; 16]).is_err())
            .count();
        eprintln!("échecs sur {n} copies : essai unique = {single}, avec nouvelles tentatives = {retry}");
    }

    #[test]
    fn wire_format_has_size_header() {
        let w = grid().to_wire();
        assert_eq!(&w[0..4], &4u32.to_le_bytes());
        assert_eq!(&w[4..8], &3u32.to_le_bytes());
        assert_eq!(w.len(), 8 + 4 * 3 * 4);
    }
}

#[derive(Default)]
pub struct AppState {
    pub capture: Mutex<Option<Shot>>,
    pub crop: Mutex<Option<Shot>>,
    pub busy: AtomicBool,
    /// Début de la capture en cours : sert à détecter une capture restée bloquée.
    pub busy_since: Mutex<Option<std::time::Instant>>,
    pub printscreen_registered: AtomicBool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CaptureMeta {
    width: u32,
    height: u32,
}

/// Lance une capture de l'écran sous le curseur et ouvre l'overlay de sélection.
pub fn start_capture(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.busy.swap(true, Ordering::SeqCst) {
        let overlay = app.get_webview_window("overlay");
        if overlay.as_ref().and_then(|o| o.is_visible().ok()).unwrap_or(false) {
            // Sélection déjà à l'écran : on la remet au premier plan.
            if let Some(o) = overlay {
                let _ = o.set_focus();
            }
            return;
        }
        // Capture lancée mais jamais affichée depuis plus de 3 s : elle est bloquée.
        // On la libère et on repart, au lieu d'ignorer la touche jusqu'au redémarrage.
        let stale = state
            .busy_since
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(3));
        if !stale {
            return;
        }
        eprintln!("[glossyshots] capture précédente bloquée : réinitialisation");
        if let Some(o) = overlay {
            let _ = o.hide();
        }
    }
    *state.busy_since.lock().unwrap() = Some(std::time::Instant::now());
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(err) = capture_under_cursor(&app) {
            eprintln!("[glossyshots] capture impossible : {err}");
            app.state::<AppState>().busy.store(false, Ordering::SeqCst);
        }
    });
}

fn capture_under_cursor(app: &AppHandle) -> Result<(), String> {
    // L'éditeur ne doit pas apparaître dans la nouvelle capture.
    if let Some(editor) = app.get_webview_window("editor") {
        if editor.is_visible().unwrap_or(false) {
            let _ = editor.hide();
            std::thread::sleep(std::time::Duration::from_millis(140));
        }
    }

    let cursor = app.cursor_position().unwrap_or(PhysicalPosition::new(0.0, 0.0));
    let monitor = xcap::Monitor::from_point(cursor.x as i32, cursor.y as i32)
        .or_else(|_| {
            xcap::Monitor::all()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .ok_or_else(|| "aucun écran".to_string())
        })
        .map_err(|e| e.to_string())?;

    let image = monitor.capture_image().map_err(|e| e.to_string())?;
    let (width, height) = (image.width(), image.height());
    let mx = monitor.x().map_err(|e| e.to_string())?;
    let my = monitor.y().map_err(|e| e.to_string())?;

    *app.state::<AppState>().capture.lock().unwrap() = Some(Shot {
        width,
        height,
        rgba: image.into_raw(),
    });

    let overlay = app
        .get_webview_window("overlay")
        .ok_or("fenêtre overlay absente")?;
    overlay
        .set_position(PhysicalPosition::new(mx, my))
        .map_err(|e| e.to_string())?;
    overlay
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    // L'overlay récupère les pixels, dessine, puis appelle `overlay_ready` pour s'afficher.
    overlay
        .emit("overlay:capture", CaptureMeta { width, height })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn show_editor(app: &AppHandle) {
    if let Some(editor) = app.get_webview_window("editor") {
        let _ = editor.unminimize();
        let _ = editor.show();
        let _ = editor.set_focus();
    }
}

fn raw_body(request: &Request<'_>) -> Result<Vec<u8>, String> {
    match request.body() {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        _ => Err("corps binaire attendu".into()),
    }
}

fn header<'a>(request: &'a Request<'_>, name: &str) -> Result<&'a str, String> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("en-tête {name} manquant"))
}

// ───────────────────────────── Commandes overlay ─────────────────────────────

#[tauri::command]
fn get_capture(state: tauri::State<'_, AppState>) -> Result<Response, String> {
    let guard = state.capture.lock().unwrap();
    let shot = guard.as_ref().ok_or("aucune capture en cours")?;
    Ok(Response::new(shot.to_wire()))
}

#[tauri::command]
fn overlay_ready(app: AppHandle) {
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.show();
        let _ = overlay.set_always_on_top(true);
        let _ = overlay.set_focus();
    }
}

#[tauri::command]
fn commit_selection(app: AppHandle, x: u32, y: u32, width: u32, height: u32) -> Result<(), String> {
    let state = app.state::<AppState>();
    let crop = {
        let guard = state.capture.lock().unwrap();
        let shot = guard.as_ref().ok_or("aucune capture en cours")?;
        shot.crop(x, y, width, height)
    };
    let meta = CaptureMeta { width: crop.width, height: crop.height };
    *state.crop.lock().unwrap() = Some(crop);
    *state.capture.lock().unwrap() = None;
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }
    state.busy.store(false, Ordering::SeqCst);
    let _ = app.emit_to("editor", "editor:capture", meta);
    show_editor(&app);
    Ok(())
}

#[tauri::command]
fn cancel_capture(app: AppHandle) {
    let state = app.state::<AppState>();
    *state.capture.lock().unwrap() = None;
    state.busy.store(false, Ordering::SeqCst);
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.hide();
    }
}

// ───────────────────────────── Commandes éditeur ─────────────────────────────

#[tauri::command]
fn get_crop(state: tauri::State<'_, AppState>) -> Result<Response, String> {
    let guard = state.crop.lock().unwrap();
    let shot = guard.as_ref().ok_or("aucune capture")?;
    Ok(Response::new(shot.to_wire()))
}

#[tauri::command]
fn request_capture(app: AppHandle) {
    start_capture(&app);
}

/// Copie une image RGBA brute (en-têtes `x-width`, `x-height`) dans le presse-papiers.
///
/// Le presse-papiers Windows est une ressource partagée : un autre programme (historique
/// Win+V, gestionnaire de mots de passe, outil de capture…) peut l'ouvrir au même instant,
/// d'où des échecs passagers (erreur 1418, « presse-papiers occupé »). On écrit donc depuis
/// un thread dédié et on réessaie quelques fois avant d'abandonner.
#[tauri::command]
async fn copy_image(request: Request<'_>) -> Result<(), String> {
    let width: usize = header(&request, "x-width")?.parse().map_err(|_| "largeur invalide")?;
    let height: usize = header(&request, "x-height")?.parse().map_err(|_| "hauteur invalide")?;
    let bytes = raw_body(&request)?;
    if bytes.len() != width * height * 4 {
        return Err("taille d'image incohérente".into());
    }
    tauri::async_runtime::spawn_blocking(move || write_clipboard_image(width, height, bytes))
        .await
        .map_err(|e| e.to_string())?
}

const CLIPBOARD_ATTEMPTS: u32 = 12;

fn write_clipboard_image(width: usize, height: usize, bytes: Vec<u8>) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=CLIPBOARD_ATTEMPTS {
        let result = arboard::Clipboard::new().and_then(|mut clipboard| {
            clipboard.set_image(arboard::ImageData {
                width,
                height,
                bytes: std::borrow::Cow::Borrowed(&bytes),
            })
        });
        match result {
            Ok(()) => return Ok(()),
            // Image invalide : réessayer ne changerait rien.
            Err(arboard::Error::ConversionFailure) => return Err("conversion de l'image impossible".into()),
            Err(err) => {
                last = err.to_string();
                // Attente croissante : ~1,6 s au total sur 12 essais.
                std::thread::sleep(std::time::Duration::from_millis(25 * u64::from(attempt)));
            }
        }
    }
    Err(format!("presse-papiers indisponible après {CLIPBOARD_ATTEMPTS} essais ({last})"))
}

/// Écrit un PNG déjà encodé dans `Images/Glossyshots/<x-filename>` et renvoie son chemin.
#[tauri::command]
fn save_png(app: AppHandle, request: Request<'_>) -> Result<String, String> {
    let name = header(&request, "x-filename")?;
    if name.contains(['/', '\\', ':']) || !name.ends_with(".png") {
        return Err("nom de fichier invalide".into());
    }
    let bytes = raw_body(&request)?;
    let dir = app
        .path()
        .picture_dir()
        .map_err(|e| e.to_string())?
        .join("Glossyshots");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
fn reveal_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").args(["-R", &path]).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(std::path::Path::new(&path).parent().unwrap_or(std::path::Path::new(".")))
        .spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}

// ─────────────────────────────────── App ───────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_editor(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .plugin(shortcut::plugin())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_capture,
            overlay_ready,
            commit_selection,
            cancel_capture,
            get_crop,
            request_capture,
            copy_image,
            save_png,
            reveal_path,
            settings::get_settings,
            settings::set_language,
            settings::get_system_state,
            settings::set_printscreen_for_glossyshots,
            settings::set_autostart,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let first_run = settings::load(app.handle());
            shortcut::register_all(app.handle());
            shortcut::start_watchdog(app.handle());
            tray::build(app.handle())?;

            // Au premier lancement (et hors démarrage automatique), on présente l'éditeur
            // pour expliquer le raccourci ; ensuite l'app vit dans la zone de notification.
            let background = std::env::args().any(|a| a == "--background");
            if first_run && !background {
                show_editor(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer l'éditeur le masque : l'app reste prête dans la zone de notification.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "editor" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Glossyshots");
}
