// Agrupa los repos planos del reporte en NODOS: un repo normal es un nodo hoja;
// un git worktree se anida bajo su repo padre (resuelto por el parser en
// `repo.worktree.parentRepo`). Así el sidebar deja de mostrar cada worktree como
// un repo suelto con nombre aleatorio. Lógica pura y aislada (misma convención que
// time.ts / api.ts); el Sidebar solo renderiza los nodos.
import type { Repo } from './types'

export interface RepoNode {
  key: string // = parentCwd (grupo) o repo.cwd (hoja)
  parent: Repo | null // el repo en parentCwd, si alguna sesión tocó el checkout principal
  displayCwd: string // ruta para el nombre mostrado (basename)
  worktrees: Repo[] // worktrees hijos, por recencia
  repos: Repo[] // parent (si hay) + worktrees: miembros para el cálculo de foco
  lastActivity: string | null // máx actividad sobre los miembros (ancla el orden/foco)
}

function activityOf(r: Repo): string | null {
  return r.sessions[0]?.lastActivity ?? null
}

// Comparador desc por ISO (null al final). localeCompare basta para ISO 8601.
function byActivityDesc(a: string | null, b: string | null): number {
  return (b ?? '').localeCompare(a ?? '')
}

export function groupRepos(repos: Repo[]): RepoNode[] {
  const groups = new Map<string, { parent: Repo | null; worktrees: Repo[] }>()
  const ensure = (k: string) => {
    let g = groups.get(k)
    if (!g) {
      g = { parent: null, worktrees: [] }
      groups.set(k, g)
    }
    return g
  }

  for (const r of repos) {
    const parentRepo = r.worktree?.parentRepo
    if (r.worktree && parentRepo) {
      // worktree con padre resuelto -> se anida bajo el grupo del padre.
      ensure(parentRepo).worktrees.push(r)
    } else {
      // repo normal (o worktree sin padre resoluble) -> su propio nodo hoja.
      ensure(r.cwd).parent = r
    }
  }

  const nodes: RepoNode[] = []
  for (const [key, g] of groups) {
    const members = [g.parent, ...g.worktrees].filter((r): r is Repo => r !== null)
    let last: string | null = null
    for (const m of members) {
      const a = activityOf(m)
      if (a && (!last || a > last)) last = a
    }
    const worktrees = g.worktrees.slice().sort((a, b) => byActivityDesc(activityOf(a), activityOf(b)))
    nodes.push({ key, parent: g.parent, displayCwd: key, worktrees, repos: members, lastActivity: last })
  }
  nodes.sort((a, b) => byActivityDesc(a.lastActivity, b.lastActivity))
  return nodes
}
