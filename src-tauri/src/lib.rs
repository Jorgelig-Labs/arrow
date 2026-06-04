//! arrow-app — backend nativo de Tauri.
//!
//! Envuelve la librería `arrow` (el parser) en dos comandos `invoke` que
//! devuelven EXACTAMENTE el mismo JSON que consume la UI web hoy:
//!   - `report()`  -> ReportOut  (equivale a `arrow --json`)
//!   - `content()` -> ContentOut (equivale a `arrow --content --file …`)
//!
//! No hay sidecar ni servidor HTTP: la UI llama a Rust directamente. El refresco
//! en vivo lo provee un watcher `notify` sobre `~/.claude/projects` que emite el
//! evento `report-changed` (con debounce) al frontend.

use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use arrow::{CleanupResult, ContentOut, ReportOut, WorktreeAudit};
use notify::{RecursiveMode, Watcher};
#[cfg(target_os = "macos")]
use tauri::Manager;
use tauri::{AppHandle, Emitter};

/// `~/.claude/projects` (fuente de verdad nativa de Claude Code).
fn projects_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.claude/projects")
}

/// Reporte completo repo→sesión→archivo (sin filtros). Contrato idéntico a `--json`.
/// `async` + `spawn_blocking`: el parseo del corpus es pesado, así que NO debe correr
/// en el hilo del runtime (si no, atasca la entrega del evento `report-changed`).
#[tauri::command]
async fn report() -> ReportOut {
    tauri::async_runtime::spawn_blocking(|| arrow::build_report(&projects_dir()))
        .await
        .expect("report task panicked")
}

/// Before/after de un archivo para la vista de diff. Idéntico a `--content --file`.
#[tauri::command]
async fn content(file: String, session: Option<String>) -> ContentOut {
    tauri::async_runtime::spawn_blocking(move || {
        arrow::file_content(&projects_dir(), &file, session.as_deref())
    })
    .await
    .expect("content task panicked")
}

/// Worktree hygiene audit (Stage 1, read-only). Idéntico a `--worktrees --json`.
/// Usa git/du/gh en vivo; puede tardar segundos en repos con worktrees grandes,
/// así que la UI lo dispara bajo demanda (no en cada refresco del report).
#[tauri::command]
async fn worktrees(include_sizes: bool) -> WorktreeAudit {
    // Off the main thread: the audit blocks on git/du subprocesses. `include_sizes`
    // false = fast git-only scan (no multi-GB `du`), so opening the panel never hangs.
    tauri::async_runtime::spawn_blocking(move || {
        arrow::worktree_audit(&projects_dir(), include_sizes)
    })
    .await
    .unwrap_or(WorktreeAudit {
        repos: Vec::new(),
        gh_available: false,
    })
}

/// Stage 2 (mutating): remove a worktree via `git worktree remove` (no `--force`).
/// `dry_run` reports the command without touching disk. On a real success, emits
/// `report-changed` so the tree refreshes.
#[tauri::command]
async fn remove_worktree(
    app: AppHandle,
    repo: String,
    path: String,
    dry_run: bool,
) -> CleanupResult {
    let res = tauri::async_runtime::spawn_blocking(move || {
        arrow::worktree::remove_worktree(&repo, &path, dry_run)
    })
    .await
    .unwrap_or(CleanupResult {
        ok: false,
        dry_run,
        command: String::new(),
        output: "internal error running the cleanup task".to_string(),
    });
    if res.ok && !res.dry_run {
        let _ = app.emit("report-changed", ());
    }
    res
}

/// Stage 2 (mutating): prune phantom worktree entries via `git worktree prune`.
/// `dry_run` uses git's native `-n` preview. On a real success, emits `report-changed`.
#[tauri::command]
async fn prune_worktrees(app: AppHandle, repo: String, dry_run: bool) -> CleanupResult {
    let res = tauri::async_runtime::spawn_blocking(move || {
        arrow::worktree::prune_worktrees(&repo, dry_run)
    })
    .await
    .unwrap_or(CleanupResult {
        ok: false,
        dry_run,
        command: String::new(),
        output: "internal error running the prune task".to_string(),
    });
    if res.ok && !res.dry_run {
        let _ = app.emit("report-changed", ());
    }
    res
}

/// Watcher nativo: vigila `~/.claude/projects` y, con debounce, emite
/// `report-changed` para que el frontend refresque sin polling.
///
/// Resiliente al ciclo de vida del directorio: si `~/.claude/projects` aún no
/// existe (instalación nueva) o se borra y se recrea, el watcher reintenta
/// establecerse cada `RETRY` en vez de rendirse para siempre. El polling lento
/// del frontend (App.svelte) es el respaldo último si el watcher fallara.
fn spawn_watcher(app: AppHandle) {
    const RETRY: Duration = Duration::from_secs(5);
    const DEBOUNCE: Duration = Duration::from_millis(400);
    const REVALIDATE: Duration = Duration::from_secs(30);

    let dir = projects_dir();
    std::thread::spawn(move || {
        // Bucle externo: (re)establecer el watch. `continue 'establish` suelta el
        // watcher actual y vuelve a empezar (p.ej. tras borrarse el directorio).
        'establish: loop {
            if !Path::new(&dir).exists() {
                std::thread::sleep(RETRY); // aún no existe: reintenta más tarde
                continue;
            }
            let (tx, rx) = channel();
            let mut watcher =
                match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if res.is_ok() {
                        let _ = tx.send(());
                    }
                }) {
                    Ok(w) => w,
                    Err(_) => {
                        std::thread::sleep(RETRY);
                        continue;
                    }
                };
            // RecursiveMode: los transcripts viven en subdirectorios por proyecto.
            if watcher
                .watch(Path::new(&dir), RecursiveMode::Recursive)
                .is_err()
            {
                std::thread::sleep(RETRY);
                continue;
            }
            // Armado. El `watcher` se mantiene vivo en este scope mientras dure el
            // bucle interno; al hacer `continue 'establish` se libera y se re-crea.
            loop {
                match rx.recv_timeout(REVALIDATE) {
                    Ok(()) => {
                        // Drena ráfagas: emite una sola vez tras DEBOUNCE de calma.
                        while rx.recv_timeout(DEBOUNCE).is_ok() {}
                        let _ = app.emit("report-changed", ());
                    }
                    // Idle: revalida que el directorio siga existiendo (inotify
                    // pierde el watch si el dir se borra y recrea sin avisar).
                    Err(RecvTimeoutError::Timeout) => {
                        if !Path::new(&dir).exists() {
                            continue 'establish; // se borró: re-establecer al volver
                        }
                    }
                    // El watcher se cayó (canal cerrado): re-establecer.
                    Err(RecvTimeoutError::Disconnected) => continue 'establish,
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Mitigaciones WebKitGTK en Linux (pantalla en blanco por DMABUF/NVIDIA/Wayland).
    // DEBEN fijarse ANTES de construir la app. El bug de font-weight (+100) ya está
    // compensado en web/src/app.css (font-weight: 350).
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        // Si aparece pantalla en blanco en Wayland, descomentar:
        // std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            report,
            content,
            worktrees,
            remove_worktree,
            prune_worktrees
        ])
        .setup(|app| {
            spawn_watcher(app.handle().clone());
            // macOS: restaurar la decoración nativa (semáforos rojo/amarillo/verde). En
            // Linux/Windows mantenemos la titlebar custom (decorations:false del config),
            // porque ahí el WM no pinta los botones de min/max de forma fiable.
            #[cfg(target_os = "macos")]
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_decorations(true);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error al arrancar la app de arrow");
}
