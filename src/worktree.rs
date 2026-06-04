//! Worktree hygiene — a SECONDARY, opt-in layer that DOES use live `git`/`du`
//! (and `gh` best-effort). Unlike the transcript parser (`lib.rs`, pure, no git),
//! this inspects the working copies on disk to answer: "which worktrees are safe
//! to remove, how much disk would that reclaim, and what's the exact command?"
//!
//! Honesty: every "reclaimable" flag is backed by a fact git tells us (merged
//! into the default branch, on the default branch, a merged PR, or a prunable
//! phantom entry). Staleness (old last commit) is surfaced but NOT counted as
//! reclaimable — old ≠ merged. We never delete here; Stage 2 (`remove`/`prune`)
//! does, behind dry-run + confirmation.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// A worktree is "stale" past this many days without a commit. Informational
/// only — it does not by itself mark a worktree reclaimable (honesty).
const STALE_DAYS: i64 = 60;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeAudit {
    pub repos: Vec<WorktreeRepo>,
    /// `gh` is installed AND authenticated → PR-merged checks were attempted.
    pub gh_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRepo {
    /// The main checkout (the first `worktree` in `git worktree list`).
    pub parent_repo: String,
    pub default_branch: Option<String>,
    pub worktrees: Vec<WorktreeEntry>,
    pub total_kb: u64,
    /// Disk on worktrees flagged reclaimable (merged / on-default / PR-merged /
    /// prunable). Honest lower bound; stale-but-unmerged is excluded.
    pub reclaimable_kb: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeEntry {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub last_commit: Option<String>, // ISO 8601 of the last commit
    pub age_days: Option<i64>,
    pub merged: bool,            // ancestor of the default branch (local check)
    pub on_default: bool,        // checked out on the default branch itself
    pub pr_merged: Option<bool>, // gh: a merged PR for this branch (best-effort)
    pub prunable: bool,          // git flagged it (its directory is gone)
    pub stale: bool,             // age_days > STALE_DAYS
    pub size_kb: Option<u64>,
    pub reasons: Vec<String>, // honest "why this is listed" notes
    pub reclaimable: bool,    // a clearly-safe reason holds (not mere staleness)
    pub command: String,      // exact command to remove / prune it
}

/// Run `git -C <cwd> <args>`, returning trimmed stdout only on success.
fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `git -C <cwd> <args>` only for its exit status (e.g. `merge-base --is-ancestor`).
fn git_ok(cwd: &str, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Disk usage of `path` in KB via `du -sk` (the worktree's `.git` is a tiny file
/// pointing at the shared object store, so this is the real reclaimable size).
fn du_kb(path: &str) -> Option<u64> {
    let out = Command::new("du").arg("-sk").arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<u64>().ok())
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `gh` installed AND authenticated.
fn gh_authenticated() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Set of branch names that have a MERGED PR (one `gh` call per repo). Empty on
/// any failure (no remote, not authed, gh missing) — strictly best-effort.
fn merged_pr_branches(main: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let out = Command::new("gh")
        .current_dir(main)
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "headRefName",
            "--limit",
            "200",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                if let Some(arr) = v.as_array() {
                    for pr in arr {
                        if let Some(b) = pr.get("headRefName").and_then(|x| x.as_str()) {
                            set.insert(b.to_string());
                        }
                    }
                }
            }
        }
    }
    set
}

// ---------------------------------------------------------------------------
// Stage 2 — execution (the ONLY part of arrow that mutates the disk). Always
// behind an explicit, confirmed call; never auto. `remove` runs WITHOUT
// `--force`, so git itself refuses to drop a worktree with uncommitted or
// untracked changes — a free safety net we deliberately keep.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub ok: bool,
    pub dry_run: bool,
    pub command: String, // the exact command (for display / the audit log)
    pub output: String,  // what git said (stdout + stderr), trimmed
}

/// Run `git -C <repo> <args>`, capturing stdout+stderr and success.
fn run_git_capture(repo: &str, args: &[&str]) -> (bool, String) {
    match Command::new("git").arg("-C").arg(repo).args(args).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            let err = err.trim();
            if !err.is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(err);
            }
            (o.status.success(), s)
        }
        Err(e) => (false, format!("failed to run git: {e}")),
    }
}

/// Remove a linked worktree. No `--force` (git refuses on uncommitted/untracked
/// changes). `git worktree remove` has no native dry-run, so `dry_run` reports
/// the command without touching anything.
pub fn remove_worktree(repo: &str, path: &str, dry_run: bool) -> CleanupResult {
    let command = format!("git -C {repo} worktree remove {path}");
    if dry_run {
        return CleanupResult {
            ok: true,
            dry_run: true,
            command,
            output: "dry run — not executed (git would refuse if the worktree has \
                     uncommitted or untracked changes)"
                .to_string(),
        };
    }
    let (ok, output) = run_git_capture(repo, &["worktree", "remove", path]);
    CleanupResult {
        ok,
        dry_run: false,
        command,
        output,
    }
}

/// Prune phantom worktree entries (their directories are gone). Uses git's native
/// dry-run (`-n`) to preview.
pub fn prune_worktrees(repo: &str, dry_run: bool) -> CleanupResult {
    let args: &[&str] = if dry_run {
        &["worktree", "prune", "-n", "-v"]
    } else {
        &["worktree", "prune", "-v"]
    };
    let command = format!(
        "git -C {repo} worktree prune -v{}",
        if dry_run { " -n" } else { "" }
    );
    let (ok, output) = run_git_capture(repo, args);
    CleanupResult {
        ok,
        dry_run,
        command,
        output,
    }
}

/// One worktree as parsed from `git worktree list --porcelain`.
struct RawWorktree {
    path: String,
    head: Option<String>,
    branch: Option<String>, // short name; None when detached
    prunable: bool,
}

/// Parse `git worktree list --porcelain` into blocks (separated by blank lines).
fn parse_porcelain(text: &str) -> Vec<RawWorktree> {
    let mut out = Vec::new();
    let mut cur: Option<RawWorktree> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(RawWorktree {
                path: p.to_string(),
                head: None,
                branch: None,
                prunable: false,
            });
        } else if let Some(w) = cur.as_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                w.branch = Some(b.trim_start_matches("refs/heads/").to_string());
            } else if line == "prunable" || line.starts_with("prunable ") {
                w.prunable = true;
            }
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

/// Default branch of `main`: prefer the remote HEAD (`origin/master`), falling
/// back to a local `main`/`master`. Returns `(ref_for_merge_base, short_name)`.
fn default_branch(main: &str) -> Option<(String, String)> {
    if let Some(s) = git(
        main,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        // e.g. "origin/master" -> short "master".
        let short = s.rsplit('/').next().unwrap_or(&s).to_string();
        return Some((s, short));
    }
    for cand in ["main", "master"] {
        if git_ok(
            main,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{cand}"),
            ],
        ) {
            return Some((cand.to_string(), cand.to_string()));
        }
    }
    None
}

/// Audit every worktree of the given main-repo roots. Roots are de-duplicated by
/// their canonical main worktree, so passing several worktrees of the same repo
/// yields one [`WorktreeRepo`]. Repos with no linked worktrees are omitted.
/// Per-repo context gathered in the cheap serial pass (only fast git calls).
struct RepoCtx {
    /// The working dir arrow discovered (a real cwd from a transcript). Used as
    /// `git -C` for all commands AND as the displayed parent, because it is always
    /// a valid working tree — unlike the porcelain's first entry, which for a
    /// SUBMODULE is the internal `.git/modules/<name>` gitdir (not a work tree).
    repo_dir: String,
    default_ref: Option<String>,
    default_short: Option<String>,
    merged_prs: HashSet<String>,
    raws: Vec<RawWorktree>, // [0] is the main checkout; [1..] are linked worktrees
}

/// `include_sizes` toggles the SLOW extras as one "full scan" flag: when false we
/// skip both the multi-GB `du` pass AND the per-repo `gh` PR lookups (network), for
/// a fast LOCAL git-only scan — used by the UI's default scan so opening the panel
/// never blocks. The local merge check (`merge-base`) and phantom detection still run,
/// so "reclaimable" stays meaningful; sizes and PR-merged are fetched on demand.
pub fn audit_worktrees(roots: &[String], include_sizes: bool) -> WorktreeAudit {
    let gh = include_sizes && gh_authenticated();
    let now = now_epoch();
    let mut seen_main: HashSet<String> = HashSet::new();

    // Phase 1 (serial, cheap): one porcelain + default-branch + merged-PR lookup
    // per repo. Skip repos with no linked worktrees.
    let mut ctxs: Vec<RepoCtx> = Vec::new();
    for root in roots {
        let porcelain = match git(root, &["worktree", "list", "--porcelain"]) {
            Some(p) => p,
            None => continue, // not a git repo
        };
        let raws = parse_porcelain(&porcelain);
        // Dedup by the canonical main worktree (porcelain's first entry), so several
        // worktrees of the same repo collapse to one audit. But run commands against
        // `root` (a guaranteed work tree), not this canonical path.
        let canon = match raws.first() {
            Some(w) => w.path.clone(),
            None => continue,
        };
        if raws.len() <= 1 || !seen_main.insert(canon) {
            continue; // no linked worktrees, or already audited via another worktree
        }
        let default = default_branch(root);
        ctxs.push(RepoCtx {
            default_ref: default.as_ref().map(|(r, _)| r.clone()),
            default_short: default.as_ref().map(|(_, s)| s.clone()),
            merged_prs: if gh {
                merged_pr_branches(root)
            } else {
                HashSet::new()
            },
            raws,
            repo_dir: root.clone(),
        });
    }

    // Phase 2 (parallel): `analyze` is dominated by `du` over big trees, so run
    // every linked worktree concurrently in a scope (threads borrow `ctxs`; the
    // scope joins them before returning). For our scale (tens of worktrees) the
    // thread count is fine; a huge repo would still be bounded by its worktree count.
    let mut per_repo: Vec<Vec<WorktreeEntry>> = ctxs.iter().map(|_| Vec::new()).collect();
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for (i, ctx) in ctxs.iter().enumerate() {
            for raw in ctx.raws.iter().skip(1) {
                handles.push(s.spawn(move || {
                    (
                        i,
                        analyze(
                            raw,
                            &ctx.repo_dir,
                            ctx.default_ref.as_deref(),
                            ctx.default_short.as_deref(),
                            &ctx.merged_prs,
                            now,
                            include_sizes,
                        ),
                    )
                }));
            }
        }
        for h in handles {
            if let Ok((i, entry)) = h.join() {
                per_repo[i].push(entry);
            }
        }
    });

    // Phase 3 (serial): assemble, total, and order.
    let mut repos: Vec<WorktreeRepo> = Vec::new();
    for (ctx, mut entries) in ctxs.into_iter().zip(per_repo) {
        if entries.is_empty() {
            continue;
        }
        let mut total_kb = 0u64;
        let mut reclaimable_kb = 0u64;
        for e in &entries {
            if let Some(kb) = e.size_kb {
                total_kb += kb;
                if e.reclaimable {
                    reclaimable_kb += kb;
                }
            }
        }
        // Reclaimable first, then largest, then oldest — the cleanup-worthy on top.
        entries.sort_by(|a, b| {
            b.reclaimable
                .cmp(&a.reclaimable)
                .then(b.size_kb.cmp(&a.size_kb))
                .then(b.age_days.cmp(&a.age_days))
        });
        repos.push(WorktreeRepo {
            parent_repo: ctx.repo_dir,
            default_branch: ctx.default_short,
            worktrees: entries,
            total_kb,
            reclaimable_kb,
        });
    }

    // Repos with cleanup-worthy worktrees first, then by total disk.
    repos.sort_by(|a, b| {
        b.reclaimable_kb
            .cmp(&a.reclaimable_kb)
            .then(b.total_kb.cmp(&a.total_kb))
    });
    WorktreeAudit {
        repos,
        gh_available: gh,
    }
}

fn analyze(
    raw: &RawWorktree,
    main: &str,
    default_ref: Option<&str>,
    default_short: Option<&str>,
    merged_prs: &HashSet<String>,
    now: i64,
    include_sizes: bool,
) -> WorktreeEntry {
    let path = raw.path.clone();
    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let exists = !raw.prunable && Path::new(&path).exists();

    // Last commit: ISO (display) + epoch (age). One `git log` on the worktree HEAD.
    let (last_commit, age_days) = match raw.head.as_deref() {
        Some(head) if exists => match git(main, &["log", "-1", "--format=%cI%n%ct", head]) {
            Some(s) => {
                let mut it = s.lines();
                let iso = it.next().map(|s| s.to_string());
                let age = it
                    .next()
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|ct| (now - ct) / 86_400);
                (iso, age)
            }
            None => (None, None),
        },
        _ => (None, None),
    };
    let stale = age_days.map(|d| d > STALE_DAYS).unwrap_or(false);

    let on_default = match (raw.branch.as_deref(), default_short) {
        (Some(b), Some(d)) => b == d,
        _ => false,
    };
    // Merged = HEAD is an ancestor of the default branch (only meaningful off-default).
    let merged = !on_default
        && match (raw.head.as_deref(), default_ref) {
            (Some(head), Some(dref)) if exists => {
                git_ok(main, &["merge-base", "--is-ancestor", head, dref])
            }
            _ => false,
        };
    // Only assert a PR verdict when gh returned merged-PR data for this repo.
    let pr_merged = if merged_prs.is_empty() {
        None
    } else {
        Some(
            raw.branch
                .as_deref()
                .map(|b| merged_prs.contains(b))
                .unwrap_or(false),
        )
    };

    // `du` over big trees is the slow part (and the freeze culprit on a tight
    // machine), so it's opt-in: a fast scan skips it and shows sizes as unknown.
    let size_kb = if include_sizes && exists {
        du_kb(&path)
    } else {
        None
    };

    let mut reasons = Vec::new();
    let mut reclaimable = false;
    if raw.prunable || !exists {
        reasons.push("directory gone → prunable".to_string());
        reclaimable = true;
    } else {
        if on_default {
            reasons.push(format!(
                "on default branch ({})",
                default_short.unwrap_or("?")
            ));
            reclaimable = true;
        } else if merged {
            reasons.push(format!(
                "merged into {}",
                default_short.unwrap_or("default")
            ));
            reclaimable = true;
        }
        if pr_merged == Some(true) {
            reasons.push("PR merged".to_string());
            reclaimable = true;
        }
        if stale {
            // Informational: surfaced but does NOT mark reclaimable on its own.
            reasons.push(format!("no commits in {}d", age_days.unwrap_or(0)));
        }
    }

    let command = if raw.prunable || !exists {
        format!("git -C {main} worktree prune")
    } else {
        format!("git -C {main} worktree remove {path}")
    };

    WorktreeEntry {
        path,
        name,
        branch: raw.branch.clone(),
        last_commit,
        age_days,
        merged,
        on_default,
        pr_merged,
        prunable: raw.prunable || !exists,
        stale,
        size_kb,
        reasons,
        reclaimable,
        command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in {dir:?}");
    }

    #[test]
    fn audit_detecta_y_remove_elimina_un_worktree() {
        // Hermetic: a real temp git repo + a linked worktree, then remove it.
        let base = std::env::temp_dir().join(format!("arrow-wt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@t"]);
        git_in(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x\n").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);
        // Linked worktree on a new branch.
        let wt = base.join("wt-feature");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );

        let repo_s = repo.to_string_lossy().to_string();
        let audit = audit_worktrees(std::slice::from_ref(&repo_s), true);
        assert_eq!(
            audit.repos.len(),
            1,
            "el repo con 1 worktree debe auditarse"
        );
        assert_eq!(audit.repos[0].worktrees.len(), 1);
        assert_eq!(audit.repos[0].worktrees[0].name, "wt-feature");

        // Dry-run: no toca disco.
        let dry = remove_worktree(&repo_s, wt.to_str().unwrap(), true);
        assert!(dry.ok && dry.dry_run);
        assert!(wt.exists(), "dry-run NO debe borrar el worktree");

        // Real: elimina el worktree (sin cambios sin commitear, git lo permite).
        let res = remove_worktree(&repo_s, wt.to_str().unwrap(), false);
        assert!(res.ok, "remove debió tener éxito: {}", res.output);
        assert!(!wt.exists(), "el worktree debe quedar eliminado");

        // prune dry-run corre sin error (no hay fantasmas, no rompe).
        let pr = prune_worktrees(&repo_s, true);
        assert!(pr.ok && pr.dry_run);

        let _ = std::fs::remove_dir_all(&base);
    }
}
