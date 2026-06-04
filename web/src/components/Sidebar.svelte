<script lang="ts">
  import type { Report, Repo, Session } from '../lib/types'
  import { isLive, focusRepos as focusReposOf, relative, dateBucket, BUCKET_ORDER } from '../lib/time'
  import { groupRepos, type RepoNode } from '../lib/worktrees'

  interface Props {
    report: Report
    selected: { session: string; path: string } | null
    now: number // reloj reactivo (tick en App.svelte): envejece los "Nm ago" y el punto verde
    onSelect: (session: string, path: string) => void
  }
  let { report, selected, now, onSelect }: Props = $props()

  // Nodos: cada git worktree se anida bajo su repo padre (groupRepos); un repo normal
  // es un nodo hoja. Foco: reusa focusRepos (a nivel de repo) y marca en foco el nodo
  // si CUALQUIER miembro (padre o worktree) está en el foco. El resto va a "Other repos".
  let nodes = $derived(groupRepos(report.repos))
  let focusSet = $derived(new Set(focusReposOf(report.repos).map((r) => r.cwd)))
  let focusNodes = $derived(nodes.filter((n) => n.repos.some((r) => focusSet.has(r.cwd))))
  let otherNodes = $derived(nodes.filter((n) => !focusNodes.includes(n)))

  // Estado de expansión (una sola tabla, indexada por node.key y por cwd de cada miembro:
  // como key de grupo = parentCwd y los worktrees tienen cwd propio, no hay colisión).
  function focusDefaults() {
    const o: Record<string, boolean> = {}
    const fset = new Set(focusReposOf(report.repos).map((r) => r.cwd))
    for (const n of groupRepos(report.repos)) {
      if (!n.repos.some((r) => fset.has(r.cwd))) continue
      o[n.key] = true
      // Auto-expande el miembro activo (p.ej. el worktree de la sesión activa).
      for (const r of n.repos) if (fset.has(r.cwd)) o[r.cwd] = true
    }
    return o
  }
  let openRepos = $state<Record<string, boolean>>(focusDefaults())

  // Tras un refresco, auto-expandir los nodos del foco nuevos sin pisar los toggles del usuario.
  $effect(() => {
    let changed = false
    const next = { ...openRepos }
    for (const n of focusNodes) {
      if (!(n.key in next)) {
        next[n.key] = true
        changed = true
      }
      for (const r of n.repos) {
        if (focusSet.has(r.cwd) && !(r.cwd in next)) {
          next[r.cwd] = true
          changed = true
        }
      }
    }
    if (changed) openRepos = next
  })
  let openOthers = $state(false)
  let openHist = $state<Record<string, boolean>>({})
  let openBucket = $state<Record<string, boolean>>({})
  let openSess = $state<Record<string, boolean>>({})

  function basename(p: string): string {
    const parts = p.split('/').filter(Boolean)
    return parts[parts.length - 1] || p
  }
  // Carpeta del archivo relativa a la raíz del repo (sin el nombre del archivo).
  function relDir(path: string, root: string): string {
    let p = path
    if (root && p.startsWith(root)) p = p.slice(root.length).replace(/^\/+/, '')
    const i = p.lastIndexOf('/')
    return i >= 0 ? p.slice(0, i) : ''
  }
  // Compacta a las últimas 2 carpetas: web/src/components -> …/src/components
  function shortDir(dir: string): string {
    if (!dir) return ''
    const parts = dir.split('/').filter(Boolean)
    return parts.length <= 2 ? parts.join('/') : '…/' + parts.slice(-2).join('/')
  }
  function titleOf(s: Session): string {
    return s.title || (s.lastPrompt ? s.lastPrompt.slice(0, 60) : '') || s.sessionId.slice(0, 8)
  }
  function buckets(sessions: Session[]) {
    const map = new Map<string, Session[]>()
    for (const s of sessions) {
      const b = dateBucket(s.lastActivity)
      if (!map.has(b)) map.set(b, [])
      map.get(b)!.push(s)
    }
    return BUCKET_ORDER.filter((b) => map.has(b)).map((b) => ({ bucket: b, sessions: map.get(b)! }))
  }
  function toggle(o: Record<string, boolean>, k: string): Record<string, boolean> {
    return { ...o, [k]: !o[k] }
  }
  // Punto verde: solo si el nodo está en foco y el repo tiene actividad reciente.
  function repoLive(repo: Repo | null, inFocus: boolean): boolean {
    return inFocus && isLive(repo?.sessions[0]?.lastActivity, now)
  }
</script>

{#snippet fileRow(sessionId: string, f: any, deep: boolean, root: string)}
  {@const dir = shortDir(relDir(f.path, root))}
  {@const ft = relative(f.lastTouched, now)}
  <button
    class="row file"
    class:deep
    class:active={selected?.session === sessionId && selected?.path === f.path}
    onclick={() => onSelect(sessionId, f.path)}
    title={f.path}
  >
    <span class="fname">{basename(f.path)}</span>
    {#if dir}<span class="fdir">{dir}</span>{/if}
    <span class="meta">
      {#if ft}<span class="ftime">{ft}</span>{/if}
      <span class="stats"><span class="add">+{f.added}</span><span class="del">-{f.removed}</span></span>
      {#if f.userModified}<span class="flag" title="Modified outside Claude">⚠</span>{/if}
    </span>
  </button>
{/snippet}

<!-- Cuerpo de un repo (sesión actual + historial). Reutilizado por nodos hoja y por
     cada worktree hijo: un worktree ES un repo con sus propias sesiones/archivos. -->
{#snippet repoBody(repo: Repo)}
  {@const current = repo.sessions[0]}
  {@const rest = repo.sessions.slice(1)}
  {#if current}
    <div class="current-head">
      <span class="stitle" title={titleOf(current)}>{titleOf(current)}</span>
      <span class="time">{relative(current.lastActivity, now)}</span>
    </div>
    {#each current.files as f}
      {@render fileRow(current.sessionId, f, false, repo.cwd)}
    {/each}
  {/if}

  {#if rest.length}
    <button class="row hist-head" onclick={() => (openHist = toggle(openHist, repo.cwd))}>
      <span class="chev">{openHist[repo.cwd] ? '▾' : '▸'}</span>
      <span class="hist-label">History</span>
      <span class="count">{rest.length}</span>
    </button>
    {#if openHist[repo.cwd]}
      {#each buckets(rest) as grp}
        {@const bkey = repo.cwd + '::' + grp.bucket}
        <button class="row bucket-head" onclick={() => (openBucket = toggle(openBucket, bkey))}>
          <span class="chev">{openBucket[bkey] ? '▾' : '▸'}</span>
          <span class="bucket-label">{grp.bucket}</span>
          <span class="count">{grp.sessions.length}</span>
        </button>
        {#if openBucket[bkey]}
          {#each grp.sessions as s}
            {@const skey = repo.cwd + '::' + s.sessionId}
            <button class="row hist-session" onclick={() => (openSess = toggle(openSess, skey))} title={titleOf(s)}>
              <span class="chev">{openSess[skey] ? '▾' : '▸'}</span>
              <span class="stitle small">{titleOf(s)}</span>
              <span class="time">{relative(s.lastActivity, now)}</span>
            </button>
            {#if openSess[skey]}
              {#each s.files as f}
                {@render fileRow(s.sessionId, f, true, repo.cwd)}
              {/each}
            {/if}
          {/each}
        {/if}
      {/each}
    {/if}
  {/if}
{/snippet}

<!-- Nodo hoja: un repo normal (sin worktrees). Igual que antes. -->
{#snippet leafNode(repo: Repo, inFocus: boolean)}
  <div class="repo">
    <button class="row repo-head" onclick={() => (openRepos = toggle(openRepos, repo.cwd))} title={repo.cwd}>
      <span class="chev">{openRepos[repo.cwd] ? '▾' : '▸'}</span>
      {#if repoLive(repo, inFocus)}<span class="dot"></span>{/if}
      <span class="repo-name">{basename(repo.cwd)}</span>
      {#if repo.gitBranch}<span class="branch">{repo.gitBranch}</span>{/if}
    </button>
    {#if openRepos[repo.cwd]}
      {@render repoBody(repo)}
    {/if}
  </div>
{/snippet}

<!-- Worktree hijo: anidado bajo el padre, etiquetado con su nombre + rama. -->
{#snippet worktreeChild(repo: Repo, inFocus: boolean)}
  {@const label = repo.worktree?.name ?? basename(repo.cwd)}
  <div class="repo wt-child">
    <button class="row wt-head" onclick={() => (openRepos = toggle(openRepos, repo.cwd))} title={repo.cwd}>
      <span class="chev">{openRepos[repo.cwd] ? '▾' : '▸'}</span>
      {#if repoLive(repo, inFocus)}<span class="dot"></span>{/if}
      <span class="wt-tag" title="git worktree">wt</span>
      <span class="wt-name">{label}</span>
      {#if repo.gitBranch}<span class="branch">{repo.gitBranch}</span>{/if}
    </button>
    {#if openRepos[repo.cwd]}
      {@render repoBody(repo)}
    {/if}
  </div>
{/snippet}

<!-- Nodo grupo: un repo padre con ≥1 worktree. Cabecera = nombre del padre + nº de
     worktrees; cuerpo = (sesiones propias del checkout principal, si las hay) + hijos. -->
{#snippet groupNode(node: RepoNode, inFocus: boolean)}
  {@const anyLive = inFocus && node.repos.some((r) => isLive(r.sessions[0]?.lastActivity, now))}
  <div class="repo">
    <button class="row repo-head" onclick={() => (openRepos = toggle(openRepos, node.key))} title={node.displayCwd}>
      <span class="chev">{openRepos[node.key] ? '▾' : '▸'}</span>
      {#if anyLive}<span class="dot"></span>{/if}
      <span class="repo-name">{basename(node.displayCwd)}</span>
      <span class="wt-count" title="git worktrees">{node.worktrees.length} wt</span>
    </button>
    {#if openRepos[node.key]}
      {#if node.parent}
        {@render repoBody(node.parent)}
      {/if}
      {#each node.worktrees as wt}
        {@render worktreeChild(wt, inFocus)}
      {/each}
    {/if}
  </div>
{/snippet}

{#snippet repoNode(node: RepoNode, inFocus: boolean)}
  {#if node.worktrees.length === 0 && node.parent}
    {@render leafNode(node.parent, inFocus)}
  {:else}
    {@render groupNode(node, inFocus)}
  {/if}
{/snippet}

<nav class="tree">
  {#each focusNodes as node (node.key)}
    {@render repoNode(node, true)}
  {/each}

  {#if otherNodes.length}
    <button class="row others-head" onclick={() => (openOthers = !openOthers)}>
      <span class="chev">{openOthers ? '▾' : '▸'}</span>
      <span class="others-label">Other repos</span>
      <span class="count">{otherNodes.length}</span>
    </button>
    {#if openOthers}
      <div class="others">
        {#each otherNodes as node (node.key)}
          {@render repoNode(node, false)}
        {/each}
      </div>
    {/if}
  {/if}
</nav>

<style>
  .tree {
    font-size: 13px;
    user-select: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    border: none;
    background: transparent;
    color: var(--fg);
    text-align: left;
    padding: 4px 8px;
    cursor: pointer;
    border-radius: 5px;
    font: inherit;
    overflow: hidden;
  }
  .row:hover {
    background: var(--hover);
  }
  .chev {
    color: var(--dim);
    width: 10px;
    flex: none;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--green);
    flex: none;
    box-shadow: 0 0 6px var(--green);
  }
  .repo-name {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .branch {
    margin-left: auto;
    font-size: 10px;
    color: var(--dim);
    background: var(--chip);
    padding: 1px 6px;
    border-radius: 999px;
    white-space: nowrap;
    max-width: 120px;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: none;
  }
  /* Cuenta de worktrees en la cabecera del grupo. */
  .wt-count {
    margin-left: auto;
    font-size: 10px;
    color: var(--dim);
    background: var(--chip);
    padding: 1px 6px;
    border-radius: 999px;
    white-space: nowrap;
    flex: none;
  }
  /* Worktree hijo: indentado bajo el padre. */
  .wt-child {
    margin-left: 14px;
    border-left: 1px solid var(--border);
  }
  .wt-head {
    padding-left: 8px;
  }
  .wt-tag {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.5px;
    color: var(--dim);
    background: var(--chip);
    padding: 0 4px;
    border-radius: 3px;
    flex: none;
  }
  .wt-name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .current-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px 2px 22px;
    overflow: hidden;
  }
  .stitle {
    font-size: 12px;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .stitle.small {
    color: var(--dim);
  }
  .time {
    margin-left: auto;
    font-size: 10px;
    color: var(--dim);
    white-space: nowrap;
    flex: none;
  }
  .file {
    padding-left: 28px;
  }
  .file.deep {
    padding-left: 48px;
  }
  .file.active {
    background: var(--active);
  }
  .fname {
    flex: 0 1 auto;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fdir {
    flex: 1 1 auto;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 10px;
    color: var(--dim);
    opacity: 0.6;
  }
  /* Cluster de metadata a la derecha de la fila de archivo: "Nm ago" + +/− + ⚠.
     margin-left:auto lo ancla a la derecha tanto si hay carpeta (fdir crece) como si no. */
  .meta {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .ftime {
    font-size: 10px;
    color: var(--dim);
    white-space: nowrap;
  }
  .stats {
    font-family: var(--mono);
    font-size: 11px;
    display: flex;
    gap: 6px;
    flex: none;
  }
  .add {
    color: var(--green);
  }
  .del {
    color: var(--red);
  }
  .flag {
    flex: none;
    color: var(--warn);
  }
  .hist-head {
    padding-left: 22px;
    color: var(--dim);
  }
  .hist-label {
    font-size: 12px;
  }
  .bucket-head {
    padding-left: 36px;
    color: var(--dim);
    font-size: 12px;
  }
  .hist-session {
    padding-left: 50px;
  }
  .count {
    margin-left: auto;
    color: var(--dim);
    font-size: 11px;
    flex: none;
  }
  .others-head {
    margin-top: 8px;
    border-top: 1px solid var(--border);
    border-radius: 0;
    padding-top: 8px;
    color: var(--dim);
    font-size: 12px;
  }
  .others-label {
    text-transform: uppercase;
    letter-spacing: 0.5px;
    font-size: 11px;
  }
  .others {
    opacity: 0.85;
  }
</style>
