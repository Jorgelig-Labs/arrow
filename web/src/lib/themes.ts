// Theme METADATA only (id/label/dark) — no CodeMirror theme extensions here, so this
// module stays tiny and the heavy `@uiw/codemirror-themes-all` bundle is NOT dragged
// into the entry chunk by the topbar menu. The actual extensions live in `themes-ext.ts`,
// imported only by the (lazy-loaded) DiffView. See ROADMAP "lazy-load CodeMirror".

export interface ThemeMeta {
  id: string
  label: string
  dark: boolean
}

export const THEMES: ThemeMeta[] = [
  { id: 'githubDark', label: 'GitHub Dark', dark: true },
  { id: 'dracula', label: 'Dracula', dark: true },
  { id: 'tokyoNight', label: 'Tokyo Night', dark: true },
  { id: 'vscodeDark', label: 'VS Code Dark', dark: true },
  { id: 'nord', label: 'Nord', dark: true },
  { id: 'monokai', label: 'Monokai', dark: true },
  { id: 'materialDark', label: 'Material Dark', dark: true },
  { id: 'gruvboxDark', label: 'Gruvbox Dark', dark: true },
  { id: 'atomone', label: 'Atom One', dark: true },
  { id: 'aura', label: 'Aura', dark: true },
  { id: 'androidstudio', label: 'Android Studio', dark: true },
  { id: 'sublime', label: 'Sublime', dark: true },
  { id: 'githubLight', label: 'GitHub Light', dark: false },
  { id: 'solarizedLight', label: 'Solarized Light', dark: false },
]

export const DEFAULT_THEME = 'githubDark'
