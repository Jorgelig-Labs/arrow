# ROADMAP / workfile de arrow

Archivo de seguimiento entre sesiones. El **roadmap canónico de fases** vive en
[README.md](README.md#roadmap); aquí va el detalle operativo: estado fino, deuda técnica y un
backlog de ideas que aún no están comprometidas a ninguna fase. Las **convenciones** del proyecto
están en [CLAUDE.md](CLAUDE.md); el contrato de datos en [SPEC.md](SPEC.md).

> Última actualización: 2026-06-03 (worktree awareness + limpieza por etapas: agrupar worktrees bajo
> su repo padre + auditoría/limpieza opt-in. Antes: distribución macOS por matrix de CI + `install.sh`;
> Fase 2 + pulido: zoom, titlebar custom, foco por sesión activa, orden por recencia, tick de reloj).

## Estado actual

| Fase | Estado | Notas |
|---|---|---|
| 0 — parser/CLI | ✅ | `src/lib.rs` (lib) + `src/main.rs` (CLI). |
| 1 — UI web (Svelte + CodeMirror) | ✅ | `web/`, dev-server ejecuta el binario. |
| 2 — empaquetado Tauri 2.x | ✅ | `.deb` + AppImage; backend nativo `src-tauri/`; frontend dual-mode. |
| 3 — honestidad + git | ⏳ pendiente | Ver README. Tiene nota de diseño del badge `live` en SPEC.md. |
| 4 — edición + GitHub | ⏳ postergado | Ver README. |

### Pulido aplicado (post-Fase 2)
- **Tests del parser**: 24 tests unitarios (`src/lib.rs` + `src/worktree.rs`, `#[cfg(test)] mod tests`) con
  transcripts-fixture en tempdir. Cubren lo NO obvio: parsing defensivo, solo-top-level, agrupación
  por raíz git, conteo +/-, filtro de `~/.claude/`, orden de repos y de archivos por recencia,
  metadatos, `file_content`. Correr con `cargo test --release`. Complementan a `/verify-parser` (datos reales).
- **Watcher resiliente** (`src-tauri/src/lib.rs`): reintenta establecer el watch si
  `~/.claude/projects` no existe aún o se borra y recrea (antes se rendía para siempre). El polling
  del frontend sigue siendo el respaldo último.
- **Binario optimizado**: perfil release en `src-tauri/Cargo.toml` (`strip` + `lto` +
  `codegen-units=1` + `opt-level="s"` + `panic="abort"`) para reducir el tamaño del bundle.
- **Skill `/rust-review`**: clippy + rustfmt + checklist de best-practices (ver
  `.claude/skills/rust-review/`).

### Worktrees: awareness + limpieza por etapas (post-Fase 2)
Perfil objetivo: dev que corre muchos worktrees en paralelo sobre varios proyectos y necesita
limpiar los mergeados/fantasmas, liberar espacio y mantener orden. Tres capas:
- **Etapa 0 — detección + agrupación (parser, read-only).** `resolve_git_context` (`src/lib.rs`)
  distingue `.git` ARCHIVO (worktree) de `.git` CARPETA (repo) por `metadata().is_file()` y, para un
  worktree, lee `gitdir:` y deriva `name` + `parent_repo` exigiendo el segmento `/worktrees/` (así un
  **submódulo** —`.git/modules/…`— NO se confunde con worktree). El contrato gana `RepoOut.worktree`
  (aditivo). La UI (`web/src/lib/worktrees.ts` `groupRepos` + `Sidebar.svelte`) **anida** cada worktree
  bajo su repo padre con badge de rama, en vez de mostrarlo como repo suelto con nombre aleatorio
  (resolvía la confusión de “¿por qué arrow abre `wise-plotting-graham`?”). Tests:
  `detecta_worktree_y_su_repo_padre`, `submodulo_no_se_confunde_con_worktree`, `repo_normal_no_marca_worktree`.
- **Etapa 1 — auditoría (read-only, capa git/fs SEPARADA y opt-in).** `src/worktree.rs`
  (`audit_worktrees`) sale del parser puro: usa `git worktree list --porcelain` + `git log`/`merge-base`
  + `du -sk` + `gh` best-effort. Marca **reclaimable** SOLO con un hecho de git (mergeado a la rama
  default, en la rama default, PR mergeado, o fantasma podable) — la **antigüedad NO cuenta** (honesto).
  Paraleliza `du` con `thread::scope` (31s→11s en datos reales). Usa el **working dir descubierto** como
  `git -C` (no el primer entry del porcelain, que para submódulos es el gitdir interno). Expuesto en CLI
  (`--worktrees [--json]`), Tauri (`worktrees()`), dev-server (`/api/worktrees`) y panel
  `WorktreesPanel.svelte` (botón en la topbar; muestra candidatos + tamaño + comando copiable).
  Verificado en datos reales: 4.4 GB en worktrees, 639 MB reclaimable; clip-hive (8 merged),
  backoffice (2 merged+PR), escapadas (2 fantasmas).
- **Etapa 2 — ejecución (opt-in, MUTA disco; única parte que muta).** `remove_worktree`/`prune_worktrees`
  (`src/worktree.rs`) corren `git worktree remove` **sin `--force`** (git rehúsa con cambios sin
  commitear) / `git worktree prune`. `remove` no tiene `--dry-run` nativo ⇒ el dry-run reporta el comando
  sin tocar disco; `prune` usa `-n`. Comandos Tauri (`remove_worktree`/`prune_worktrees`) emiten
  `report-changed` al terminar. UI: botón **Clean** → dry-run → **confirmación** → ejecutar → re-scan;
  en el navegador (dev) degrada a “copy cmd” (no exponemos remove/prune por HTTP). Test:
  `audit_detecta_y_remove_elimina_un_worktree` (repo git temporal real + worktree → remove). 24 tests en total.

### Endurecimiento por auditoría multi-agente (post-worktrees)
Una auditoría de 15 agentes (5 módulos × 3 lentes: correctitud, perf Rust, footprint) sobre la feature
de worktrees produjo 3 frentes de mejora, todos aplicados:
- **Honestidad (lo más grave).** El audit overprometía "seguro de borrar": (a) un worktree **en la rama
  default / sin divergir** ahora dice "on default branch" / "no commits beyond <default>" (no falso
  "merged"); (b) un worktree **locked** ya NO es reclaimable (git `worktree remove` sin `--force` lo
  rechaza) — esto atrapó un bug real: 8 worktrees de `clip-hive` bloqueados por agentes ACTIVOS que
  arrow habría dicho "elimínalos"; (c) `prunable` se decide **solo por el flag de git**, no por
  `Path::exists()` (un disco desmontado ya no se confunde con fantasma); (d) el **Prune** del panel es
  ahora **a nivel de repo** ("Prune N phantoms", marca todos), no por-fila silencioso; (e) `pr_merged`
  es **advisory** (no reclaimable por sí solo); (f) comandos **shell-quoted**; (g) `strip_prefix`
  para `refs/heads/` y `origin/` (ramas con slash). Tests: `honestidad_reclaimable_solo_lo_seguro`.
- **Performance.** (1) `worktree_audit` ya NO re-corre `build_report` completo: nuevo
  `collect_repo_roots` (solo raíces, sin diff/hunks). (2) Fan-out **acotado** (`bounded_map`,
  ≤cores≤8, `catch_unwind` por item) en vez de un hilo por worktree (el `du`-thrash era el
  congelamiento). (3) Fase 1 (`gh` + default branch) **paralela**; se quitó el probe `gh auth status`
  (se deriva del éxito de la llamada). (4) `report()`/`content()` de Tauri pasan a `async` +
  `spawn_blocking`. Resultado real: escaneo completo **~10s → ~3.3s**; el rápido sigue ~2s.
- **Footprint (arranque).** **Lazy-load de CodeMirror**: `DiffView` se importa dinámicamente al abrir
  el primer archivo, y los themes se parten en metadata (`themes.ts`) vs extensiones pesadas
  (`themes-ext.ts`, solo en el chunk de DiffView). El **entry de boot bajó de ~463 KB a ~76 KB**.
  **Nota honesta:** esto NO baja el piso de ~200 MB de RAM (es el WKWebView del sistema) — es solo
  arranque/TTI/bundle; la única palanca que cruzaría ese piso (servir en navegador, sin webview) se
  **descartó** porque la ventana nativa Mac/Linux/Windows es innegociable.
- **Pendientes/notas:** macOS sin GUI verificada en hardware aquí (lib + comandos compilan, tests y
  dev-server verificados); PR-merged es señal secundaria (en algunos repos pega, en branches de agente
  no-pusheadas no); la verificación de la auditoría es contra el estado en vivo de git (no del dato del
  transcript), por diseño.

### Mejoras visuales / UX (post-Fase 2)
- **Zoom de la UI** (`Ctrl +` / `Ctrl −` / `Ctrl 0`, estilo VSCode): zoom **nativo** del webview en la
  app Tauri (`getCurrentWebview().setZoom`; no toca el layout → no descoloca a CodeMirror ni depende
  de la versión de WebKitGTK) y CSS `zoom` como fallback en el navegador (dev). Persiste en
  `localStorage` (`arrow.zoom`) y se reaplica al arrancar; rango 60–200%, widget `− % +` en la topbar.
  Lógica aislada en `web/src/lib/zoom.ts` (misma convención que `api.ts`). Requirió el permiso ACL
  `core:webview:allow-set-webview-zoom` en `src-tauri/capabilities/default.json`.
- **Titlebar custom** (`decorations:false` + `resizable:true`): botones min/max/close propios
  (`web/src/components/WindowControls.svelte` → `getCurrentWindow().minimize()/toggleMaximize()/close()`,
  aislados en `web/src/lib/window.ts`), barra como zona de arrastre (`data-tauri-drag-region`),
  doble-click = maximizar, y borde CSS sutil (GTK pierde la sombra sin decoraciones). Garantiza los
  botones **cross-distro**: en Pop!_OS/GNOME el WM no los pintaba de forma fiable bajo Pop Shell, y el
  `gsettings button-layout` ya estaba correcto pero no ayudaba (ni viaja con la app, ni aplica en COSMIC).
  Permisos ACL: `core:window:allow-{minimize,toggle-maximize,close,start-dragging}`.
- **Fix de arranque Tauri**: `beforeDevCommand`/`beforeBuildCommand` en `tauri.conf.json` eran
  `npm --prefix web run …`, pero el CLI 2.11 los ejecuta desde `web/` (dir del frontend), NO desde la
  raíz → buscaba `web/web/package.json` y `cargo tauri dev` fallaba. Corregidos a `npm run dev` /
  `npm run build` (sin `--prefix`); nota de CLAUDE.md actualizada. ✅ Verificado: `cargo tauri dev`
  levanta la ventana y `cargo tauri build` genera `.deb` (2.0M) + AppImage (77M).
- **Foco = sesión activa** (rediseño de la semántica `live`, resuelve la deuda de abajo): el sidebar
  muestra arriba los repos de la **sesión activa** (actividad más reciente) + ráfaga de ~10 min
  (`BURST_WINDOW`), en vez de "todos los repos con actividad <20 min" — así un repo de una sesión
  anterior deja de quedar "pegado" arriba. **Punto verde** como único indicador (solo en repos del
  foco con actividad reciente); se quitó el badge de texto `● live`. Lógica única en
  `web/src/lib/time.ts` (`focusRepos()`), reusada por `Sidebar.svelte` y la topbar de `App.svelte`
  (antes el cálculo `isLive` estaba duplicado en 4 sitios). Verificado contra datos reales. Detalle de
  honestidad: "sesión activa" = actividad más reciente en disco, NO proceso en ejecución; el foco se
  ancla al timestamp del dato (no al reloj). Nota del modelo: una sesión (mismo `sessionId`) vive en
  UN repo (raíz git del cwd), así que el foco son ≥1 repo según cuántas sesiones recientes haya en la
  ráfaga, no "una sesión tocando varios repos".
- **Archivos ordenados por recencia de edición** (no alfabético): dentro de una sesión, el archivo
  editado más recientemente va arriba y re-tocar uno lo vuelve a subir al tope en el siguiente
  refresco. El parser (`build_report_from` en `src/lib.rs`) ahora rastrea `last_ts` por archivo
  (`FileChange`) y emite `lastTouched` en `FileOut`, ordenando los archivos desc (sin fecha al final,
  desempate por path). Mismo split que repos/sesiones: el contrato `--json`/UI va por recencia, el
  `--list` (terminal) sigue alfabético/browsable. Cada fila de archivo muestra su "Nm ago"
  (`Sidebar.svelte`). Anclado al **dato** (timestamp), no al reloj. Test: `archivos_ordenados_por_ultima_edicion`.
- **Tick de reloj para el tiempo relativo** (resuelve la deuda de abajo): un `setInterval` de 30 s en
  `App.svelte` reasigna un `now` reactivo que se pasa a `relative()`/`isLive()` (`web/src/lib/time.ts`)
  y al `Sidebar` (prop `now`), así los "Nm ago" envejecen y el **punto verde** caduca a los 20 min sin
  esperar una edición nueva. Es **display-only**: no refetcha ni reordena (la reactividad granular de
  Svelte 5 recalcula solo las etiquetas, no el árbol). Se **pausa con `visibilitychange`** cuando la
  ventana se oculta y se pone al día al volver al foco. Costo: una decena de strings recalculados cada
  30 s — más barato que el poll de respaldo de 15 s que ya existía.

## Backlog de ideas (sin comprometer fase)

Mejoras propuestas que NO están en el roadmap de fases. A discutir/priorizar antes de implementar;
todas deben respetar la honestidad del producto (no afirmar más de lo que el dato sabe).

- **Búsqueda / filtro en el sidebar** — filtrar el árbol por nombre de repo, de archivo o por
  contenido del diff. Mejora de navegación cuando hay muchos repos/sesiones. (Solo frontend:
  `web/src/components/Sidebar.svelte`; no toca el parser.)
- **Stats de cambios** — totales de líneas `+/-` por repo / sesión / día; quizá un mini-resumen en
  la topbar o un panel. Refuerza el ángulo de "auditoría". (El parser ya expone `added`/`removed`
  por archivo; sería agregación, posible en frontend o como campo nuevo en el contrato.)
- **Export** — copiar el diff de un archivo al portapapeles o exportar un resumen de sesión a
  Markdown (lista de archivos + `+/-` + título). Útil para PRs o reportes.
- **Atajos de teclado** — navegar el árbol y abrir diffs sin ratón (j/k, enter, etc.). (Los atajos de
  **zoom** `Ctrl +/−/0` ya están; faltan los de navegación. Reusarían el mismo `keydown` global de
  `App.svelte` (`onKey`).)

## Deuda técnica / notas

- **Semántica del badge `live`** — ✅ resuelto (post-Fase 2; ver "Mejoras visuales / UX"). El foco del
  sidebar pasó de "todos los repos con actividad <20 min" a "los de la **sesión activa** (actividad más
  reciente) + ráfaga ~10 min"; el **punto verde** quedó como único indicador (se quitó el badge de
  texto `live`). Sigue honesto: "activa" = actividad más reciente en disco, no proceso vivo. Bonus: el
  foco se ancla al **dato** (no al reloj), así que no sufre el congelamiento de tiempo relativo (abajo).
- **Tiempo relativo congelado** — ✅ resuelto (ver "Mejoras visuales / UX"). `relative()`/`isLive()`
  solo se recalculaban al re-render; si el report no cambiaba, el "Nm ago" no avanzaba y el punto verde
  no caducaba. Ahora un tick de reloj independiente (30 s, `App.svelte`) reasigna un `now` reactivo que
  reciben `relative()`/`isLive()` y el `Sidebar`; se pausa con la ventana oculta. Display-only (no
  refetcha ni reordena). La nota de diseño en SPEC.md quedó actualizada (decisión (b) tomada).
- **Reconstrucción del `before` cuando `originalFile` es `null`** — ✅ resuelto (fix
  `before-null-originalfile`). Claude Code emite `originalFile:null` en parte de los Edit aunque
  haya `structuredPatch`; antes eso hacía que un archivo existente se pintara como "new file" (una
  sola columna, sin el diff). Ahora `resolve_before` (`src/lib.rs`) lo resuelve en cascada: (1)
  `originalFile` inline del 1er Edit; (2) el `originalFile` exacto más temprano de una edición posterior
  reaplicando en reversa las previas; (3) el último `Read` **completo** (los parciales/offset se
  descartan); (4) reaplicar en reversa TODAS las ediciones de la sesión sobre el `after` de disco. Cada
  reconstrucción se **verifica por hunk** (un hunk que no cuadra ⇒ drift ⇒ se aborta y queda "no
  disponible", honesto). Verificado con datos reales: `EstadisticasV2.tsx` (vía Read completo) y
  `StatsUI.tsx` (4 Edits todos con `originalFile:null` + solo Reads parciales ⇒ reverse-apply desde
  disco, before=1488 líneas) muestran su diff. **Endurecido tras auditoría adversarial** (20 agentes):
  (a) `create` (Write de archivo nuevo, `originalFile:null`, 0 hunks) ⇒ before vacío = "new file" en vez
  de "no disponible"; (b) el `Read` snapshot solo se usa si cuadra con el 1er Edit (`snapshot_consistent`),
  evitando misatribuir un cambio no rastreado entre Read y Edit; (c) `--content` sin `--session` elige la
  sesión más reciente, sin mezclar ediciones de varias sesiones; (d) hunks solapados ⇒ se aborta.
  **Límites conocidos (documentados en el código, best-effort, no mienten):** la verificación es local a
  las regiones tocadas, así que el reverse-apply desde un disco desfasado puede arrastrar cambios
  fuera-de-hunk (aparecen igual en before y after ⇒ no se atribuyen como diff); el marcador
  `\ No newline at end of file` no se modela (no aparece en `structuredPatch` real). **Posible mejora
  (Fase 3):** usar `~/.claude/file-history/<sessionId>/<sha256(path)[:16]>@v<n>` (snapshot canónico) para
  los casos con drift donde el reverse-apply aborta.
- **Build de macOS en CI** — ✅ resuelto. `release.yml` es ahora una **matrix** (Linux `ubuntu-22.04`
  + `macos-14` arm64 + `macos-13` Intel): cada tag `v*` compila y publica los dos `.dmg` (ad-hoc
  signed, sin secretos) junto al `.deb` + AppImage, en runners de macOS **gratis** (sin Mac física).
  Encima va `install.sh` (raíz), el one-liner `curl … | bash` que baja el `.dmg` del último Release y
  lo deja en `/Applications`. **Pendiente:** firma/notarización (Apple Developer, ~99 USD/año) para
  quitar la fricción de Gatekeeper, y un **Homebrew Cask** (`brew install --cask`) con upgrade/uninstall.
- **macOS sin verificar en hardware**: el código ya está adaptado a Mac (titlebar nativa + font-weight
  por OS; ver [MACOS.md](MACOS.md)) y el `.dmg` ya se construye en CI, pero el bloque
  `cfg(target_os = "macos")` y el `install.sh` aún **no se probaron en una Mac real** → falta correr
  un tag y confirmar el look de la titlebar, que el `.dmg` monta y que el one-liner instala bien.
