// Heavy CodeMirror theme extensions (the `@uiw/codemirror-themes-all` bundle). Imported
// ONLY by DiffView, which is itself lazy-loaded on first file open — so this stays out of
// the entry chunk and loads with the editor, not at boot. Metadata lives in `themes.ts`.
import type { Extension } from '@codemirror/state'
import {
  githubDark,
  githubLight,
  dracula,
  tokyoNight,
  vscodeDark,
  nord,
  monokai,
  materialDark,
  gruvboxDark,
  atomone,
  aura,
  androidstudio,
  sublime,
  solarizedLight,
} from '@uiw/codemirror-themes-all'
import { DEFAULT_THEME } from './themes'

const EXT: Record<string, Extension> = {
  githubDark,
  dracula,
  tokyoNight,
  vscodeDark,
  nord,
  monokai,
  materialDark,
  gruvboxDark,
  atomone,
  aura,
  androidstudio,
  sublime,
  githubLight,
  solarizedLight,
}

export function themeExt(id: string): Extension {
  return EXT[id] ?? EXT[DEFAULT_THEME]
}
