// Contrato producido por `arrow --json` (modo reporte) y `arrow --content`.

export interface Report {
  projectsDir: string
  repoCount: number
  repos: Repo[]
}

export interface Repo {
  cwd: string
  gitBranch: string | null
  worktree?: WorktreeInfo | null // presente si cwd es un git worktree; la UI lo anida bajo su padre
  sessions: Session[] // ordenadas por última actividad (más reciente primero)
}

export interface WorktreeInfo {
  name: string // nombre del worktree (último componente del gitdir)
  parentRepo: string | null // ruta del repo padre; null si el layout no es el canónico
}

export interface Session {
  sessionId: string
  title: string | null // ai-title generado por Claude
  lastPrompt: string | null
  firstActivity: string | null // ISO 8601
  lastActivity: string | null // ISO 8601
  fileCount: number
  files: FileChange[]
}

export interface FileChange {
  path: string
  writeType: string | null // "create" | "update" | null
  userModified: boolean
  ops: number
  added: number
  removed: number
  lastTouched: string | null // ISO 8601 — última edición de este archivo; los archivos vienen ordenados por esto (más reciente primero)
}

export interface FileContent {
  file: string
  session: string | null
  before: string
  after: string
  beforeAvailable: boolean
  afterAvailable: boolean
  userModified: boolean
  ops: number
}

// Contrato de `arrow --worktrees --json` (auditoría de higiene de worktrees, Etapa 1).
export interface WorktreeAudit {
  repos: WorktreeRepoAudit[]
  ghAvailable: boolean // gh autenticado -> se intentó detección de PR-merged
}

export interface WorktreeRepoAudit {
  parentRepo: string
  defaultBranch: string | null
  worktrees: WorktreeEntry[]
  totalKb: number
  reclaimableKb: number // solo lo claramente seguro (merged/on-default/PR-merged/prunable)
}

export interface WorktreeEntry {
  path: string
  name: string
  branch: string | null
  lastCommit: string | null // ISO 8601
  ageDays: number | null
  merged: boolean // head contenido en la rama default (chequeo local)
  onDefault: boolean // está en la rama default
  prMerged: boolean | null // gh: PR mergeado para esta rama (advisory, best-effort)
  prunable: boolean // git lo marca (su gitdir no apunta a nada)
  locked: boolean // git worktree remove (sin --force) lo rechazaría
  stale: boolean // ageDays > umbral
  sizeKb: number | null
  reasons: string[] // por qué se lista (honesto)
  reclaimable: boolean // hay una razón claramente segura (no mera antigüedad)
  command: string // comando exacto para eliminarlo/podarlo
}

// Resultado de una acción de limpieza (Etapa 2). Espeja arrow::CleanupResult.
export interface CleanupResult {
  ok: boolean
  dryRun: boolean
  command: string
  output: string // lo que dijo git (stdout+stderr)
}
