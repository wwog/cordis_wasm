# Cordis Website

The official website for the **Cordis (Rust)** plugin runtime. Built with React, TypeScript,
Vite, and Ant Design. It renders the repository's docs directly from `../docs/` — no copy, so the
site always matches the markdown in the repo.

## Features

- **Bilingual** (中 / EN) with a segmented-control language switcher in the header. Language is
  stored in React state; routes are language-neutral (`/tutorial/01-first-plugin`, `/api/service`,
  `/semantics/semantics`), so switching language reloads the same page in the other language.
- **Single source of truth**: `src/lib/docs.ts` globs every `../docs/**/*.md` file with Vite's
  `?raw` import and indexes them by section (`tutorial`, `api`, `sundry`, `semantics`) and language
  (English base `.md`, Chinese `.zh.md`). Edit a doc in `../docs/` and the site picks it up on the
  next build — no content regeneration step.
- **Markdown rendering** with `react-markdown` + `remark-gfm` (tables, task lists, strikethrough)
  and `rehype-highlight` + `highlight.js` for syntax-highlighted code blocks.
- **Smart link resolution**: internal `.md` links between docs are rewritten to client-side routes;
  links into parts of the repo we don't serve (e.g. `ts-docs/`) point at the GitHub `master` blob
  instead of 404ing.

## Getting started

```bash
npm install
npm run dev       # http://localhost:5173
npm run build     # production build to dist/
npm run preview   # serve the production build
```

## Deploying to GitHub Pages

The site is published to **https://wwog.github.io/cordis_wasm/** from the `master` branch via the
`.github/workflows/deploy-website.yml` workflow, which uses the official
`actions/deploy-pages` action.

- `vite.config.ts` sets `base: '/cordis_wasm/'` so asset URLs resolve under the project sub-path.
- Routing uses `react-router-dom`'s `HashRouter` because GitHub Pages has no SPA fallback —
  deep links like `/cordis_wasm/#/api/context` survive a refresh, where a browser-history router would 404.
- Pages must be enabled with **Source: GitHub Actions** in the repo settings (already done).

## Project layout

| Path | Purpose |
|---|---|
| `src/lib/docs.ts` | Globs and indexes `../docs/**/*.md` into a page list. |
| `src/i18n.tsx` | UI string tables for zh/en + the language context. |
| `src/I18nProvider.tsx` | Holds the active language. |
| `src/App.tsx` | Route table (home, `/tutorial|api|sundry|semantics/:slug`). |
| `src/components/SiteLayout.tsx` | Header (nav + language switch + GitHub), footer. |
| `src/components/Markdown.tsx` | The markdown renderer and link resolver. |
| `src/pages/HomePage.tsx` | Hero + "what problem does Cordis solve" + doc links. |
| `src/pages/DocPage.tsx` | Two-column doc layout: sidebar of section pages + content. |

## Notes

- The doc content lives one level above the Vite project root. `vite.config.ts` adds a `server.fs.allow`
  entry and a `@docs` alias so dev mode can serve the markdown.
- The bundle is large because all markdown is inlined at build time. This is intentional for a docs
  site; code-split the routes if it ever becomes an issue.
