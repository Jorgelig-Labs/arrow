import { invoke } from '@tauri-apps/api/core'
import type { Report, FileContent, WorktreeAudit, CleanupResult } from './types'

// Única capa que conoce el transporte. Dos modos, MISMO contrato:
//   - App de escritorio (Tauri): llama al backend Rust vía invoke() — sin HTTP.
//   - Navegador (npm run dev): pega al dev-server que ejecuta el binario `arrow`.
// Los componentes Svelte no saben en cuál están: solo consumen loadReport/loadContent.
export const inTauri = '__TAURI_INTERNALS__' in window

export async function loadReport(): Promise<Report> {
  if (inTauri) return invoke<Report>('report')

  const res = await fetch('/api/report')
  const data = await res.json()
  if (!res.ok || data.error) throw new Error(data.error ?? `report HTTP ${res.status}`)
  return data as Report
}

// Auditoría de worktrees (Etapa 1, read-only). Bajo demanda: usa git/gh en vivo (y
// `du` solo si includeSizes). NO se carga con el report; la UI la dispara con un botón.
// `includeSizes=false` = escaneo rápido solo-git (sin el `du` que congela en máquinas
// con poca RAM); los tamaños se piden aparte. Mismo patrón dual-mode que loadReport.
export async function loadWorktrees(includeSizes = false): Promise<WorktreeAudit> {
  if (inTauri) return invoke<WorktreeAudit>('worktrees', { includeSizes })

  const res = await fetch('/api/worktrees' + (includeSizes ? '?sizes=1' : ''))
  const data = await res.json()
  if (!res.ok || data.error) throw new Error(data.error ?? `worktrees HTTP ${res.status}`)
  return data as WorktreeAudit
}

// Etapa 2 (muta disco): ejecutar la limpieza. Solo en la app de escritorio (Tauri):
// no exponemos `git worktree remove/prune` por el dev-server HTTP. En el navegador
// (dev) degradamos con un error claro; la UI ofrece "copy cmd" como alternativa.
const NO_EXEC = 'Worktree cleanup runs in the desktop app — copy the command instead.'

export async function removeWorktree(repo: string, path: string, dryRun: boolean): Promise<CleanupResult> {
  if (!inTauri) throw new Error(NO_EXEC)
  return invoke<CleanupResult>('remove_worktree', { repo, path, dryRun })
}

export async function pruneWorktrees(repo: string, dryRun: boolean): Promise<CleanupResult> {
  if (!inTauri) throw new Error(NO_EXEC)
  return invoke<CleanupResult>('prune_worktrees', { repo, dryRun })
}

// Cache de contenidos ya cargados: revisitar un archivo (al navegar el árbol) es
// instantáneo, sin volver a pegar al backend. El `before` de un (archivo, sesión)
// es estable, pero `after`/`ops` cambian con ediciones nuevas; por eso el cache se
// invalida (clearContentCache) cada vez que el report cambia. Ver App.svelte.
const contentCache = new Map<string, FileContent>()

export function clearContentCache(): void {
  contentCache.clear()
}

export async function loadContent(file: string, session?: string | null): Promise<FileContent> {
  const key = (session ?? '') + ' ' + file
  const hit = contentCache.get(key)
  if (hit) return hit

  let data: FileContent
  if (inTauri) {
    data = await invoke<FileContent>('content', { file, session: session ?? null })
  } else {
    const params = new URLSearchParams({ file })
    if (session) params.set('session', session)
    const res = await fetch('/api/content?' + params.toString())
    const json = await res.json()
    if (!res.ok || json.error) throw new Error(json.error ?? `content HTTP ${res.status}`)
    data = json as FileContent
  }
  contentCache.set(key, data)
  return data
}
