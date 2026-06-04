//! arrow — CLI de auditoría de Claude Code
//!
//! Capa fina sobre la librería `arrow` (`src/lib.rs`): parsea flags, llama a la
//! lib y presenta el resultado (terminal coloreado o JSON). TODA la lógica del
//! parser vive en la lib y se comparte con el backend de Tauri (`src-tauri/`).
//!
//! Flags: `--list`, `--json`, `--repo`, `--session`, `--content --file`,
//! `--projects-dir`. Su comportamiento es idéntico al de antes del refactor.

use anyhow::Result;
use clap::Parser;

use arrow::{Collected, Repo, SessionMeta, WorktreeAudit};

#[derive(Parser, Debug)]
#[command(
    name = "arrow",
    version,
    about = "Audita qué archivos tocó Claude Code y con qué diff (datos nativos: ~/.claude/projects, sin hooks ni git)"
)]
struct Cli {
    /// Directorio de proyectos de Claude Code (por defecto ~/.claude/projects)
    #[arg(long)]
    projects_dir: Option<String>,

    /// Filtra repos cuyo cwd contenga este texto
    #[arg(long)]
    repo: Option<String>,

    /// Filtra por sessionId (acepta prefijo)
    #[arg(long)]
    session: Option<String>,

    /// Solo resumen (repos -> sesiones -> archivos), sin cuerpos de diff
    #[arg(long)]
    list: bool,

    /// Emite el modelo normalizado como JSON (el contrato para la UI)
    #[arg(long)]
    json: bool,

    /// Modo contenido: emite JSON {before, after} de UN archivo (requiere --file)
    #[arg(long)]
    content: bool,

    /// Ruta exacta del archivo (para --content)
    #[arg(long)]
    file: Option<String>,

    /// Auditoría de worktrees: candidatos a limpieza (mergeados/fantasmas) + tamaño.
    /// Usa git/du en vivo (capa opt-in, fuera del parser de transcripts). Con --json
    /// emite el contrato WorktreeAudit.
    #[arg(long)]
    worktrees: bool,

    /// Con --worktrees: omite el cálculo de tamaño en disco (`du`), el paso lento.
    /// Escaneo rápido solo-git (los tamaños salen como desconocidos).
    #[arg(long)]
    no_sizes: bool,
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let projects_dir = cli
        .projects_dir
        .clone()
        .unwrap_or_else(|| format!("{home}/.claude/projects"));

    if cli.content {
        let target = cli
            .file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--content requiere --file <ruta>"))?;
        let out = arrow::file_content(&projects_dir, target, cli.session.as_deref());
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }

    if cli.worktrees {
        let audit = arrow::worktree_audit(&projects_dir, !cli.no_sizes);
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&audit)?);
        } else {
            render_worktrees(&audit);
        }
        return Ok(());
    }

    let collected = arrow::collect(&projects_dir, cli.repo.as_deref(), cli.session.as_deref());

    if cli.json {
        let report = arrow::build_report_from(&projects_dir, &collected.repos, &collected.metas);
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    render_terminal(&projects_dir, &collected, cli.list);
    Ok(())
}

fn render_terminal(projects_dir: &str, collected: &Collected, list_only: bool) {
    let Collected {
        repos,
        metas,
        jsonl_files,
        lines_total,
        lines_skipped,
    } = collected;

    println!("{BOLD}arrow{RESET} {DIM}· fuente: {projects_dir}{RESET}");
    println!(
        "{DIM}{jsonl_files} sesiones · {lines_total} records · {lines_skipped} líneas ignoradas (parsing defensivo){RESET}",
    );

    if repos.is_empty() {
        println!("\n{YELLOW}Sin ediciones atribuibles a Claude (Edit/Write/MultiEdit) con los filtros dados.{RESET}");
        return;
    }

    for (cwd, repo) in repos {
        render_repo(cwd, repo, metas, list_only);
    }
}

/// Human-readable size from KB (du -sk units): KB / MB / GB.
fn human_kb(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1} GB", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{:.0} MB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KB")
    }
}

fn render_worktrees(audit: &WorktreeAudit) {
    println!("{BOLD}arrow{RESET} {DIM}· worktree audit{RESET}");
    if audit.repos.is_empty() {
        println!("\n{YELLOW}Sin worktrees en los repos auditados.{RESET}");
        return;
    }
    // Sizes are optional (fast scan skips `du`); detect whether any were measured so
    // we don't print a dishonest "0 KB" when sizes simply weren't computed.
    let measured = audit
        .repos
        .iter()
        .any(|r| r.worktrees.iter().any(|w| w.size_kb.is_some()));
    // `gh_available` is only meaningful on a full scan (the fast scan skips gh on purpose).
    if measured && !audit.gh_available {
        println!("{DIM}gh no autenticado: detección de PR-merged omitida (best-effort){RESET}");
    }
    let mut grand_total = 0u64;
    let mut grand_reclaim = 0u64;
    for repo in &audit.repos {
        grand_total += repo.total_kb;
        grand_reclaim += repo.reclaimable_kb;
        let reclaimable_n = repo.worktrees.iter().filter(|w| w.reclaimable).count();
        let def = repo.default_branch.as_deref().unwrap_or("—");
        let size_part = if measured {
            format!(
                " · {} · {GREEN}{} reclaimable{RESET}",
                human_kb(repo.total_kb),
                human_kb(repo.reclaimable_kb)
            )
        } else {
            format!(" · {GREEN}{reclaimable_n} reclaimable{RESET}")
        };
        println!(
            "\n{CYAN}{BOLD}{}{RESET} {DIM}[default: {def}] · {} worktrees{}",
            repo.parent_repo,
            repo.worktrees.len(),
            size_part,
        );
        for wt in &repo.worktrees {
            let mark = if wt.reclaimable {
                format!("{GREEN}●{RESET}")
            } else if wt.stale {
                format!("{YELLOW}●{RESET}")
            } else {
                format!("{DIM}○{RESET}")
            };
            let branch = wt.branch.as_deref().unwrap_or("(detached)");
            let size = wt.size_kb.map(human_kb).unwrap_or_else(|| "—".into());
            let reasons = if wt.reasons.is_empty() {
                String::new()
            } else {
                format!("  {DIM}({}){RESET}", wt.reasons.join(", "))
            };
            println!(
                "  {mark} {BOLD}{}{RESET} {DIM}{branch}{RESET}  {size}{reasons}",
                wt.name
            );
            if wt.reclaimable {
                println!("      {DIM}$ {}{RESET}", wt.command);
            }
        }
    }
    if measured {
        println!(
            "\n{BOLD}Total:{RESET} {} en worktrees · {GREEN}{} reclaimable{RESET} {DIM}(merged/on-default/PR-merged/prunable){RESET}",
            human_kb(grand_total),
            human_kb(grand_reclaim),
        );
    } else {
        println!("\n{DIM}(tamaños no medidos: usa sin --no-sizes para el `du`){RESET}");
    }
}

fn render_repo(
    cwd: &str,
    repo: &Repo,
    metas: &std::collections::BTreeMap<String, SessionMeta>,
    list_only: bool,
) {
    let branch = repo.git_branch.as_deref().unwrap_or("—");
    println!("\n{CYAN}{BOLD}repo {cwd}{RESET}  {DIM}[{branch}]{RESET}");

    for (sid, sess) in &repo.sessions {
        let short: String = sid.chars().take(8).collect();
        let title = metas
            .get(sid)
            .and_then(|m| m.title.as_deref())
            .unwrap_or("(sin título)");
        let (mut add, mut rem) = (0usize, 0usize);
        for fc in sess.files.values() {
            add += fc.added;
            rem += fc.removed;
        }
        println!(
            "  {BOLD}{title}{RESET} {DIM}{short} · {} archivo(s) · {GREEN}+{add}{RESET}{DIM} {RED}-{rem}{RESET}",
            sess.files.len()
        );

        for (path, fc) in &sess.files {
            let tag = match fc.write_type.as_deref() {
                Some("create") => " (nuevo)",
                Some("update") => " (write)",
                _ => "",
            };
            let warn = if fc.user_modified {
                format!("  {YELLOW}⚠ modificado también fuera de Claude{RESET}")
            } else {
                String::new()
            };
            println!(
                "    {path}{DIM}{tag}{RESET}  {GREEN}+{}{RESET} {RED}-{}{RESET}{warn}",
                fc.added, fc.removed
            );

            if list_only {
                continue;
            }
            for h in &fc.hunks {
                println!(
                    "      {CYAN}@@ -{},{} +{},{} @@{RESET}",
                    h.old_start, h.old_lines, h.new_start, h.new_lines
                );
                for line in &h.lines {
                    match line.as_bytes().first() {
                        Some(b'+') => println!("      {GREEN}{line}{RESET}"),
                        Some(b'-') => println!("      {RED}{line}{RESET}"),
                        _ => println!("      {DIM}{line}{RESET}"),
                    }
                }
            }
        }
    }
}
