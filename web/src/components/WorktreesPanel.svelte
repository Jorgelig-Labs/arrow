<script lang="ts">
  import { loadWorktrees, removeWorktree, pruneWorktrees, inTauri } from '../lib/api'
  import type { WorktreeAudit, WorktreeEntry, CleanupResult } from '../lib/types'

  interface Props {
    onClose: () => void
  }
  let { onClose }: Props = $props()

  let audit = $state<WorktreeAudit | null>(null)
  let error = $state<string | null>(null)
  let loading = $state(false)
  let scanned = $state(false) // no escanea al abrir; el usuario dispara con un botón
  let withSizes = $state(false) // si el escaneo actual incluyó el `du` (tamaños)
  let copied = $state<string | null>(null) // path cuyo comando se acaba de copiar

  // Etapa 2 (ejecución, solo Tauri): dry-run -> confirmación -> ejecutar -> re-scan.
  // Dos tipos de acción: 'remove' (UN worktree, no-fantasma) y 'prune' (a nivel de
  // REPO: poda TODOS los fantasmas a la vez — `git worktree prune` no es por-fila).
  type Target =
    | { kind: 'remove'; repo: string; wt: WorktreeEntry }
    | { kind: 'prune'; repo: string; paths: string[]; n: number }

  let confirmKey = $state<string | null>(null) // qué acción espera confirmación
  let pending = $state<Target | null>(null)
  let dryRun = $state<CleanupResult | null>(null) // preview del dry-run
  let busy = $state<string | null>(null) // key de la acción en curso
  let outcome = $state<Record<string, CleanupResult>>({}) // resultado por path

  // Una acción 'prune' afecta a todos los fantasmas del repo, así que su key es por
  // repo (no por fila); 'remove' es por worktree.
  function targetKey(t: Target): string {
    return t.kind === 'remove' ? t.wt.path : 'prune:' + t.repo
  }
  function run(t: Target, dry: boolean): Promise<CleanupResult> {
    return t.kind === 'remove' ? removeWorktree(t.repo, t.wt.path, dry) : pruneWorktrees(t.repo, dry)
  }
  // Marca el resultado en TODOS los paths afectados (1 para remove, N para prune) para
  // que la desaparición tras el re-scan quede explicada por fila.
  function mark(t: Target, res: CleanupResult) {
    const o = { ...outcome }
    const paths = t.kind === 'remove' ? [t.wt.path] : t.paths
    for (const p of paths) o[p] = res
    outcome = o
  }

  async function startClean(t: Target) {
    const k = targetKey(t)
    busy = k
    try {
      dryRun = await run(t, true) // preview, no toca disco
      pending = t
      confirmKey = k
    } catch (e) {
      mark(t, { ok: false, dryRun: true, command: '', output: String(e) })
    } finally {
      busy = null
    }
  }

  async function confirmClean() {
    const t = pending
    if (!t) return
    const k = targetKey(t)
    busy = k
    confirmKey = null
    try {
      const res = await run(t, false)
      mark(t, res)
      if (res.ok) await load(withSizes) // re-scan: lo limpiado desaparece
    } catch (e) {
      mark(t, { ok: false, dryRun: false, command: '', output: String(e) })
    } finally {
      busy = null
      pending = null
    }
  }

  function cancelClean() {
    confirmKey = null
    pending = null
    dryRun = null
  }

  // Escaneo bajo demanda. `sizes=false` por defecto = solo git (rápido, no congela);
  // `sizes=true` corre el `du` (lento) para mostrar tamaños/espacio reclamable.
  async function load(sizes: boolean) {
    loading = true
    error = null
    try {
      audit = await loadWorktrees(sizes)
      withSizes = sizes
      scanned = true
    } catch (e) {
      error = String(e)
    } finally {
      loading = false
    }
  }

  // KB (unidades de du -sk) -> humano.
  function humanKb(kb: number | null): string {
    if (kb == null) return '—'
    if (kb >= 1024 * 1024) return (kb / (1024 * 1024)).toFixed(1) + ' GB'
    if (kb >= 1024) return Math.round(kb / 1024) + ' MB'
    return kb + ' KB'
  }
  function basename(p: string): string {
    const parts = p.split('/').filter(Boolean)
    return parts[parts.length - 1] || p
  }
  function dotClass(wt: WorktreeEntry): string {
    if (wt.reclaimable) return 'safe'
    if (wt.stale) return 'stale'
    return 'keep'
  }
  async function copyCmd(wt: WorktreeEntry) {
    try {
      await navigator.clipboard.writeText(wt.command)
      copied = wt.path
      setTimeout(() => (copied === wt.path ? (copied = null) : null), 1500)
    } catch {
      /* clipboard bloqueado: el comando ya es visible para copiar a mano */
    }
  }

  let totalReclaimable = $derived(audit ? audit.repos.reduce((a, r) => a + r.reclaimableKb, 0) : 0)
  let totalAll = $derived(audit ? audit.repos.reduce((a, r) => a + r.totalKb, 0) : 0)

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose()
  }
</script>

<svelte:window onkeydown={onKey} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="panel" onclick={(e) => e.stopPropagation()}>
    <header class="head">
      <span class="title">Worktree cleanup</span>
      {#if scanned && withSizes}
        <span class="summary">
          {humanKb(totalReclaimable)} reclaimable <span class="dim">/ {humanKb(totalAll)} in worktrees</span>
        </span>
      {/if}
      {#if scanned}
        <button class="refresh" onclick={() => load(withSizes)} disabled={loading} title="Re-scan">↻</button>
      {/if}
      <button class="close" onclick={onClose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      {#if !scanned && !loading}
        <!-- Explainer: no auto-scan (scanning runs git/du and would lag a tight machine).
             Tell the user exactly what this does before they trigger it. -->
        <div class="intro">
          <p>
            Finds the <strong>git worktrees</strong> of the repos you work in and flags which are
            <strong>safe to remove</strong> — so you can reclaim disk and keep things tidy.
          </p>
          <p class="dim">
            “Safe” means git says so: the branch is <em>merged into the default branch</em>, it’s
            <em>on the default branch</em>, its <em>PR was merged</em>, or it’s a <em>prunable phantom</em>
            (its folder is already gone). Staleness alone is never counted as reclaimable.
          </p>
          <p class="dim">
            This is read-only. It runs <code>git</code> (+ <code>gh</code> if available) on demand —
            a few seconds. Disk sizes use <code>du</code>, which is slower, so they’re a separate step.
            {#if inTauri}Removal later asks for confirmation and runs <code>git worktree remove</code>
            without <code>--force</code> (git refuses if there are uncommitted changes).{/if}
          </p>
          <button class="primary" onclick={() => load(false)}>Scan worktrees</button>
        </div>
      {:else if loading}
        <div class="state">
          Scanning… <span class="dim">{withSizes ? '(git + du — measuring sizes, slower)' : '(git only — a few seconds)'}</span>
        </div>
      {:else if error}
        <div class="state err">{error}</div>
      {:else if audit && audit.repos.length === 0}
        <div class="state">No worktrees found in the repos you work in.</div>
      {:else if audit}
        {#if withSizes && !audit.ghAvailable}
          <div class="note">gh not authenticated — PR-merged detection skipped (best-effort).</div>
        {/if}
        <div class="toolbar">
          <div class="legend">
            <span><i class="dot safe"></i> safe to remove</span>
            <span><i class="dot stale"></i> stale (old, unmerged)</span>
            <span><i class="dot keep"></i> active</span>
          </div>
          {#if !withSizes}
            <button class="sizes-btn" onclick={() => load(true)} disabled={loading}>
              Calculate sizes
            </button>
          {/if}
        </div>
        {#each audit.repos as repo}
          {@const phantoms = repo.worktrees.filter((w) => w.prunable)}
          {@const pruneKey = 'prune:' + repo.parentRepo}
          <section class="repo">
            <div class="repo-head">
              <span class="repo-name">{basename(repo.parentRepo)}</span>
              {#if repo.defaultBranch}<span class="chip">{repo.defaultBranch}</span>{/if}
              <span class="repo-meta">
                {repo.worktrees.length} wt{#if withSizes} · {humanKb(repo.totalKb)}{#if repo.reclaimableKb > 0} · <span class="safe-text">{humanKb(repo.reclaimableKb)} reclaimable</span>{/if}{/if}
              </span>
              <!-- Prune is repo-wide (`git worktree prune` clears ALL phantoms at once),
                   so it's ONE repo-level action, not a per-row button. -->
              {#if inTauri && phantoms.length}
                <button
                  class="clean"
                  disabled={busy === pruneKey}
                  onclick={() => startClean({ kind: 'prune', repo: repo.parentRepo, paths: phantoms.map((p) => p.path), n: phantoms.length })}
                >
                  {busy === pruneKey ? '…' : `Prune ${phantoms.length} phantom${phantoms.length > 1 ? 's' : ''}`}
                </button>
              {/if}
            </div>

            {#if confirmKey === pruneKey}
              <div class="confirm">
                <code>{dryRun?.command}</code>
                <p class="confirm-note">
                  Prunes <strong>all {phantoms.length}</strong> phantom worktree entries in this repo
                  (their directories are already gone). Removes only the stale git bookkeeping.
                </p>
                <div class="confirm-actions">
                  <button class="danger" onclick={confirmClean}>Prune {phantoms.length}</button>
                  <button class="cancel" onclick={cancelClean}>Cancel</button>
                </div>
              </div>
            {/if}

            {#each repo.worktrees as wt}
              {@const done = outcome[wt.path]}
              {@const gone = done?.ok && !done.dryRun}
              <div class="wt-block">
                <div class="wt" class:reclaimable={wt.reclaimable} class:removed={gone}>
                  <i class="dot {dotClass(wt)}"></i>
                  <span class="wt-name" title={wt.path}>{wt.name}</span>
                  <span class="wt-branch">{wt.branch ?? '(detached)'}</span>
                  {#if withSizes}<span class="wt-size">{humanKb(wt.sizeKb)}</span>{/if}
                  {#if wt.reasons.length}
                    <span class="reasons">{wt.reasons.join(' · ')}</span>
                  {/if}
                  <!-- Per-row action: only REMOVE of a non-phantom reclaimable worktree.
                       Phantoms are handled by the repo-level Prune above. -->
                  {#if wt.reclaimable && !wt.prunable && !gone}
                    <button class="copy" onclick={() => copyCmd(wt)} title={wt.command}>
                      {copied === wt.path ? 'copied' : 'copy cmd'}
                    </button>
                    {#if inTauri}
                      <button
                        class="clean"
                        disabled={busy === wt.path}
                        onclick={() => startClean({ kind: 'remove', repo: repo.parentRepo, wt })}
                      >
                        {busy === wt.path ? '…' : 'Remove'}
                      </button>
                    {:else}
                      <!-- browser: read-only, copy only -->
                    {/if}
                  {:else if wt.prunable && !inTauri}
                    <button class="copy" onclick={() => copyCmd(wt)} title={wt.command}>
                      {copied === wt.path ? 'copied' : 'copy cmd'}
                    </button>
                  {/if}
                </div>

                {#if confirmKey === wt.path}
                  <div class="confirm">
                    <code>{dryRun?.command}</code>
                    <p class="confirm-note">
                      Removes this worktree{#if withSizes && wt.sizeKb} (~{humanKb(wt.sizeKb)}){/if}.
                      No <code>--force</code>: git refuses if there are uncommitted or untracked changes.
                    </p>
                    <div class="confirm-actions">
                      <button class="danger" onclick={confirmClean}>Remove</button>
                      <button class="cancel" onclick={cancelClean}>Cancel</button>
                    </div>
                  </div>
                {/if}

                {#if done}
                  <div class="result" class:bad={!done.ok}>
                    {#if gone}✓ {wt.prunable ? 'pruned' : 'removed'}{:else if !done.ok}✕ {done.output}{/if}
                  </div>
                {/if}
              </div>
            {/each}
          </section>
        {/each}
        <p class="foot dim">
          {#if inTauri}
            Remove/prune run behind a dry-run + confirmation.
          {:else}
            Read-only here — copy the command to run it yourself.
          {/if}
          “Reclaimable” = on the default branch, no commits beyond the default branch, or a prunable
          phantom — never mere staleness, and never a locked worktree.
          {#if !withSizes}Sizes not measured yet — use “Calculate sizes”.{/if}
        </p>
      {/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .panel {
    width: min(760px, 92vw);
    max-height: 82vh;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    overflow: hidden;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.4);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    font-weight: 700;
  }
  .summary {
    color: var(--green);
    font-size: 12px;
  }
  .summary .dim {
    color: var(--dim);
  }
  .refresh,
  .close {
    border: none;
    background: transparent;
    color: var(--dim);
    cursor: pointer;
    font-size: 14px;
    border-radius: 5px;
    padding: 2px 6px;
  }
  .refresh {
    margin-left: auto;
  }
  .refresh:hover,
  .close:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .body {
    overflow: auto;
    padding: 10px 14px 14px;
  }
  .state {
    color: var(--dim);
    font-size: 13px;
    padding: 16px 4px;
  }
  .state.err {
    color: var(--red);
    white-space: pre-wrap;
  }
  .note {
    color: var(--warn);
    font-size: 12px;
    margin-bottom: 8px;
  }
  /* Explainer shown before the first scan. */
  .intro {
    padding: 6px 4px 4px;
    font-size: 13px;
    line-height: 1.55;
  }
  .intro p {
    margin: 0 0 10px;
  }
  .intro code {
    font-family: var(--mono);
    font-size: 11px;
  }
  .primary {
    margin-top: 4px;
    border: 1px solid var(--accent, var(--border));
    background: var(--accent, var(--chip));
    color: #fff;
    font: inherit;
    font-size: 13px;
    padding: 6px 16px;
    border-radius: 6px;
    cursor: pointer;
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 10px;
  }
  .sizes-btn {
    margin-left: auto;
    border: 1px solid var(--border);
    background: var(--chip);
    color: var(--fg);
    font: inherit;
    font-size: 11px;
    padding: 2px 10px;
    border-radius: 5px;
    cursor: pointer;
    flex: none;
  }
  .sizes-btn:hover:not(:disabled) {
    background: var(--hover);
  }
  .legend {
    display: flex;
    gap: 16px;
    font-size: 11px;
    color: var(--dim);
  }
  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .repo {
    margin-bottom: 14px;
  }
  .repo-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 4px;
  }
  .repo-name {
    font-weight: 600;
  }
  .chip {
    font-size: 10px;
    color: var(--dim);
    background: var(--chip);
    padding: 1px 6px;
    border-radius: 999px;
  }
  .repo-meta {
    margin-left: auto;
    font-size: 11px;
    color: var(--dim);
  }
  .safe-text {
    color: var(--green);
  }
  .wt {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 2px;
    font-size: 12px;
  }
  .wt.reclaimable {
    background: color-mix(in srgb, var(--green) 7%, transparent);
    border-radius: 5px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex: none;
  }
  .dot.safe {
    background: var(--green);
  }
  .dot.stale {
    background: var(--warn);
  }
  .dot.keep {
    background: var(--dim);
    opacity: 0.5;
  }
  .wt-name {
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 200px;
  }
  .wt-branch {
    color: var(--dim);
    font-family: var(--mono);
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 200px;
  }
  .wt-size {
    color: var(--fg);
    font-variant-numeric: tabular-nums;
    min-width: 52px;
  }
  .reasons {
    color: var(--dim);
    font-size: 11px;
    flex: 1 1 auto;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .copy {
    margin-left: auto;
    border: 1px solid var(--border);
    background: var(--chip);
    color: var(--fg);
    font: inherit;
    font-size: 11px;
    padding: 1px 8px;
    border-radius: 5px;
    cursor: pointer;
    flex: none;
  }
  .copy:hover {
    background: var(--hover);
  }
  .clean {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--fg);
    font: inherit;
    font-size: 11px;
    padding: 1px 8px;
    border-radius: 5px;
    cursor: pointer;
    flex: none;
  }
  .clean:hover:not(:disabled) {
    background: var(--hover);
  }
  .clean:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .wt.removed {
    opacity: 0.5;
    text-decoration: line-through;
  }
  .confirm {
    margin: 2px 0 6px 16px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--chip);
  }
  .confirm code {
    display: block;
    font-family: var(--mono);
    font-size: 11px;
    color: var(--fg);
    word-break: break-all;
  }
  .confirm-note {
    font-size: 11px;
    color: var(--dim);
    margin: 6px 0;
    line-height: 1.4;
  }
  .confirm-actions {
    display: flex;
    gap: 8px;
  }
  .danger,
  .cancel {
    border: 1px solid var(--border);
    font: inherit;
    font-size: 11px;
    padding: 2px 12px;
    border-radius: 5px;
    cursor: pointer;
  }
  .danger {
    background: var(--red);
    color: #fff;
    border-color: var(--red);
  }
  .cancel {
    background: transparent;
    color: var(--fg);
  }
  .cancel:hover {
    background: var(--hover);
  }
  .result {
    margin: 0 0 6px 16px;
    font-size: 11px;
    color: var(--green);
  }
  .result.bad {
    color: var(--red);
    white-space: pre-wrap;
  }
  .foot {
    font-size: 11px;
    margin-top: 6px;
    line-height: 1.5;
  }
  .dim {
    color: var(--dim);
  }
</style>
