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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// A worktree is "stale" past this many days without a commit. Informational
/// only — it does not by itself mark a worktree reclaimable (honesty).
const STALE_DAYS: i64 = 60;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeAudit {
    pub repos: Vec<WorktreeRepo>,
    /// True if a `gh` PR-list call actually succeeded for some repo (full scan only),
    /// i.e. PR-merged signals were available. False on a fast scan or when gh is absent.
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
    pub merged: bool,     // head is contained in the default branch (local check)
    pub on_default: bool, // checked out on the default branch itself
    pub pr_merged: Option<bool>, // gh: a merged PR for this branch (advisory, best-effort)
    pub prunable: bool,   // git flagged it (its gitdir points nowhere)
    pub locked: bool,     // git worktree remove (no --force) would refuse it
    pub stale: bool,      // age_days > STALE_DAYS
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

/// Branch names with a MERGED PR (one `gh` call per repo). `None` if gh is missing,
/// unauthenticated, or errors (so the caller can tell "gh unavailable" from "no merged
/// PRs"); `Some(set)` — possibly empty — on success. Replaces a separate `gh auth status`
/// probe: success here IS the availability signal.
fn merged_pr_branches(main: &str) -> Option<HashSet<String>> {
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
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;
    let mut set = HashSet::new();
    if let Some(arr) = v.as_array() {
        for pr in arr {
            if let Some(b) = pr.get("headRefName").and_then(|x| x.as_str()) {
                set.insert(b.to_string());
            }
        }
    }
    Some(set)
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
    prunable: bool,         // git's own flag: gitdir points nowhere
    locked: bool,           // `git worktree remove` (no --force) refuses a locked worktree
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
                locked: false,
            });
        } else if let Some(w) = cur.as_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                w.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                // `branch refs/heads/<name>` -> short name (only strips the leading ref prefix).
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "prunable" || line.starts_with("prunable ") {
                w.prunable = true;
            } else if line == "locked" || line.starts_with("locked ") {
                w.locked = true;
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
        // e.g. "origin/master" -> "master", "origin/release/2.x" -> "release/2.x"
        // (strip only the remote prefix, not every slash — rsplit broke slashed branches).
        let short = s.strip_prefix("origin/").unwrap_or(&s).to_string();
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

/// Run `f` over `items` with a BOUNDED thread pool (≤ cores, capped at 8, never
/// more than `items.len()`), preserving input order. Each item is isolated with
/// `catch_unwind`, so one item's panic yields `None` for that slot (printed by the
/// default panic hook) instead of taking down a worker's whole batch. Replaces the
/// old "one OS thread per worktree" fan-out, which thrashed the disk with unbounded
/// concurrent `du` on many-worktree repos.
fn bounded_map<T, R, F>(items: &[T], f: F) -> Vec<Option<R>>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let workers = {
        let cores = std::thread::available_parallelism()
            .map(|c| c.get())
            .unwrap_or(4);
        cores.clamp(1, 8).min(n)
    };
    let next = AtomicUsize::new(0);
    let batches: Vec<Vec<(usize, Option<R>)>> = std::thread::scope(|s| {
        (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let r =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&items[i])))
                                .ok();
                        local.push((i, r));
                    }
                    local
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut slots: Vec<Option<R>> = (0..n).map(|_| None).collect();
    for batch in batches {
        for (i, r) in batch {
            slots[i] = r;
        }
    }
    slots
}

/// Per-repo context. `repo_dir` is the working dir arrow discovered (a real cwd from
/// a transcript), used as `git -C` and as the displayed parent — always a valid work
/// tree, unlike the porcelain's first entry which for a SUBMODULE is the internal
/// `.git/modules/<name>` gitdir.
struct RepoCtx {
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
    let now = now_epoch();

    // Phase 1a (serial, cheap LOCAL git): porcelain + dedup by canonical main worktree.
    // Keep only repos with at least one linked worktree.
    struct Pre {
        repo_dir: String,
        raws: Vec<RawWorktree>,
    }
    let mut seen_main: HashSet<String> = HashSet::new();
    let mut pre: Vec<Pre> = Vec::new();
    for root in roots {
        let porcelain = match git(root, &["worktree", "list", "--porcelain"]) {
            Some(p) => p,
            None => continue, // not a git repo
        };
        let raws = parse_porcelain(&porcelain);
        let canon = match raws.first() {
            Some(w) => w.path.clone(),
            None => continue,
        };
        if raws.len() <= 1 || !seen_main.insert(canon) {
            continue;
        }
        pre.push(Pre {
            repo_dir: root.clone(),
            raws,
        });
    }

    // Phase 1b (parallel, bounded): per-repo default branch + (full scan) gh PR list.
    // These are the slow per-repo bits (git symbolic-ref + a network `gh` call), so
    // run them concurrently across repos instead of serially.
    let prepped = bounded_map(&pre, |p| {
        let default = default_branch(&p.repo_dir);
        let merged = if include_sizes {
            merged_pr_branches(&p.repo_dir)
        } else {
            None
        };
        (default, merged)
    });
    let mut gh_available = false;
    let mut ctxs: Vec<RepoCtx> = Vec::with_capacity(pre.len());
    for (p, res) in pre.into_iter().zip(prepped) {
        let (default, merged) = res.unwrap_or((None, None));
        if merged.is_some() {
            gh_available = true; // a gh call actually succeeded for some repo
        }
        ctxs.push(RepoCtx {
            default_ref: default.as_ref().map(|(r, _)| r.clone()),
            default_short: default.as_ref().map(|(_, s)| s.clone()),
            merged_prs: merged.unwrap_or_default(),
            raws: p.raws,
            repo_dir: p.repo_dir,
        });
    }

    // Phase 2 (parallel, bounded): analyze every linked worktree. `du` is I/O-bound,
    // so a bounded pool beats unbounded fan-out (which thrashes the disk).
    let tasks: Vec<(usize, usize)> = ctxs
        .iter()
        .enumerate()
        .flat_map(|(i, c)| (1..c.raws.len()).map(move |j| (i, j)))
        .collect();
    let analyzed = bounded_map(&tasks, |&(i, j)| {
        let ctx = &ctxs[i];
        (
            i,
            analyze(
                &ctx.raws[j],
                &ctx.repo_dir,
                ctx.default_ref.as_deref(),
                ctx.default_short.as_deref(),
                &ctx.merged_prs,
                now,
                include_sizes,
            ),
        )
    });
    let mut per_repo: Vec<Vec<WorktreeEntry>> = ctxs.iter().map(|_| Vec::new()).collect();
    for (i, entry) in analyzed.into_iter().flatten() {
        per_repo[i].push(entry);
    }

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
        gh_available,
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
    // Phantom = git's OWN `prunable` flag — never our Path::exists() guess (a live
    // worktree on an unmounted/permission-denied mount must NOT be called deletable).
    // `dir_present` is only used to decide whether running du/git on it is worthwhile.
    let prunable = raw.prunable;
    let dir_present = !prunable && Path::new(&path).exists();

    // Last commit: ISO (display) + epoch (age). One `git log` on the worktree HEAD.
    let (last_commit, age_days) = match raw.head.as_deref() {
        Some(head) if dir_present => match git(main, &["log", "-1", "--format=%cI%n%ct", head]) {
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
    // `merged` = HEAD is contained in the default branch (is-ancestor) — removing
    // the worktree then loses no unique commits. Only checked off-default and when
    // the dir is present. An errored/unknown merge-base stays `false` (conservative).
    let merged = !on_default
        && dir_present
        && match (raw.head.as_deref(), default_ref) {
            (Some(head), Some(dref)) => git_ok(main, &["merge-base", "--is-ancestor", head, dref]),
            _ => false,
        };
    // gh PR verdict is ADVISORY (branch-name match against possibly-stale data): it
    // never makes a worktree reclaimable on its own, only corroborates a local signal.
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
    let size_kb = if include_sizes && dir_present {
        du_kb(&path)
    } else {
        None
    };
    let dflt = default_short.unwrap_or("default");

    let mut reasons = Vec::new();
    let mut reclaimable = false;
    if prunable {
        reasons.push("directory gone → prunable".to_string());
        reclaimable = true;
    } else if raw.locked {
        // `git worktree remove` without --force refuses a locked worktree, so we must
        // NOT advertise it as safe to remove — that would overpromise.
        reasons.push("locked — unlock first to remove".to_string());
    } else {
        if on_default {
            // Not "merged": it's simply a checkout of the default branch (no feature
            // work to lose). Honest wording instead of implying a merge happened.
            reasons.push(format!("on default branch ({dflt})"));
            reclaimable = true;
        } else if merged {
            reasons.push(format!("no commits beyond {dflt}"));
            reclaimable = true;
        }
        // PR-merged: corroborates if local says so; otherwise advisory only (no flag).
        if pr_merged == Some(true) {
            if reclaimable {
                reasons.push("PR merged".to_string());
            } else {
                reasons.push(format!("PR merged (branch not in local {dflt})"));
            }
        }
        if stale {
            // Informational: surfaced but does NOT mark reclaimable on its own.
            reasons.push(format!("no commits in {}d", age_days.unwrap_or(0)));
        }
    }

    // Exact command, shell-quoted so it's actually paste-able (paths with spaces).
    let command = if prunable {
        format!("git -C {} worktree prune", shq(main))
    } else {
        format!("git -C {} worktree remove {}", shq(main), shq(&path))
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
        prunable,
        locked: raw.locked,
        stale,
        size_kb,
        reasons,
        reclaimable,
        command,
    }
}

/// Single-quote a string for safe copy-paste into a POSIX shell.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

    #[test]
    fn honestidad_reclaimable_solo_lo_seguro() {
        // Reglas honestas: un worktree SIN commits más allá de la rama default es
        // reclaimable ("no commits beyond"); uno DIVERGIDO no lo es; uno LOCKED no lo
        // es aunque calificara (git worktree remove sin --force lo rechazaría).
        let base = std::env::temp_dir().join(format!("arrow-wt-honesty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@t"]);
        git_in(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "x\n").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);

        // (1) worktree sin commits propios: HEAD == main -> reclaimable, honesto.
        let same = base.join("same");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                same.to_str().unwrap(),
                "-b",
                "nodiff",
            ],
        );

        // (2) worktree divergido: una rama con un commit que NO está en main.
        let diverged = base.join("diverged");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                diverged.to_str().unwrap(),
                "-b",
                "feat",
            ],
        );
        std::fs::write(diverged.join("b.txt"), "y\n").unwrap();
        git_in(&diverged, &["add", "-A"]);
        git_in(&diverged, &["commit", "-q", "-m", "feature work"]);

        // (3) worktree que calificaría pero está LOCKED.
        let locked = base.join("locked");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                locked.to_str().unwrap(),
                "-b",
                "locked-branch",
            ],
        );
        git_in(&repo, &["worktree", "lock", locked.to_str().unwrap()]);

        let repo_s = repo.to_string_lossy().to_string();
        let audit = audit_worktrees(std::slice::from_ref(&repo_s), false);
        let by = |n: &str| {
            audit.repos[0]
                .worktrees
                .iter()
                .find(|w| w.name == n)
                .unwrap_or_else(|| panic!("falta worktree {n}"))
        };

        let same_wt = by("same");
        assert!(
            same_wt.reclaimable,
            "un worktree sin commits propios es reclaimable"
        );
        assert!(
            same_wt
                .reasons
                .iter()
                .any(|r| r.contains("no commits beyond")),
            "razón honesta, no 'merged': {:?}",
            same_wt.reasons
        );

        let div = by("diverged");
        assert!(
            !div.reclaimable,
            "un worktree con commits propios NO es reclaimable"
        );

        let lck = by("locked");
        assert!(!lck.reclaimable, "un worktree locked NO es reclaimable");
        assert!(lck.locked, "debe detectarse locked");
        assert!(
            lck.reasons.iter().any(|r| r.contains("locked")),
            "razón locked: {:?}",
            lck.reasons
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
