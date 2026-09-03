// Content index for the Cordis docs.
//
// The doc sources live in the workspace `docs/` directory (one level up), not
// inside this website package. Each page exists in two flavours:
//   <name>.md    -> English (the "base" language)
//   <name>.zh.md -> Chinese
//
// We glob them with Vite's raw import so the website builds directly from the
// same markdown that ships in the repository — no copy, single source of truth.

export type Lang = 'en' | 'zh';

export type Section = 'tutorial' | 'api' | 'sundry' | 'semantics';

export interface DocPage {
  /** Stable slug, e.g. "01-first-plugin" or "cli". */
  slug: string;
  /** Human title from the first `# ` heading. */
  title: string;
  /** Which docs sub-tree the page belongs to. */
  section: Section;
  /** The page's language. */
  lang: Lang;
  /** Raw markdown content. */
  markdown: string;
  /** Path of the file, for debugging. */
  file: string;
  /** Repo-relative path, e.g. "docs/tutorial/index.zh.md". */
  repoFile: string;
}

// Vite replaces this at build time. `?raw` imports the file's text content.
// Pattern is relative to this module (website/src/lib/), so three levels up
// reaches the repo root's docs/ directory.
const modules = import.meta.glob('../../../docs/**/*.md', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

// Glob keys are relative to this file (src/lib/), e.g.
//   ../../../docs/tutorial/index.md   or   ../../../docs/semantics.md
// We strip everything up to and including the "docs/" segment.
function sectionFromPath(path: string): Section | null {
  const idx = path.lastIndexOf('docs/');
  if (idx === -1) return null;
  const rel = path.slice(idx + 'docs/'.length);
  if (rel.startsWith('tutorial/')) return 'tutorial';
  if (rel.startsWith('api/')) return 'api';
  if (rel.startsWith('sundry/')) return 'sundry';
  if (rel === 'semantics.md' || rel === 'semantics.zh.md') return 'semantics';
  return null;
}

function basename(path: string): string {
  const file = path.split('/').pop()!;
  return file.replace(/\.md$/, '');
}

function titleFromMarkdown(markdown: string): string {
  const m = markdown.match(/^#\s+(.+)$/m);
  if (!m) return '';
  // Strip inline markdown so titles like `CLI（`cordis-cli`）` read cleanly.
  return m[1]
    .replace(/`([^`]+)`/g, '$1') // inline code
    .replace(/\*\*([^*]+)\*\*/g, '$1') // bold
    .replace(/\*([^*]+)\*/g, '$1') // italic
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1') // links
    .trim();
}

const allPages: DocPage[] = [];

for (const [path, markdown] of Object.entries(modules)) {
  const section = sectionFromPath(path);
  if (!section) continue; // ignore unknown md (e.g. README at repo root)

  const base = basename(path);
  const lang: Lang = base.endsWith('.zh') ? 'zh' : 'en';
  const slug = base.replace(/\.zh$/, '');

  // Convert "../../../docs/tutorial/index.zh.md" to repo-relative "docs/tutorial/index.zh.md".
  const docIdx = path.lastIndexOf('docs/');
  const repoFile = docIdx === -1 ? path : `docs/${path.slice(docIdx + 'docs/'.length)}`;

  allPages.push({
    slug,
    title: titleFromMarkdown(markdown),
    section,
    lang,
    markdown,
    file: path,
    repoFile,
  });
}

export function pagesFor(section: Section): DocPage[] {
  return allPages.filter((p) => p.section === section);
}

export function getPage(section: Section, slug: string, lang: Lang): DocPage | undefined {
  return allPages.find(
    (p) => p.section === section && p.slug === slug && p.lang === lang,
  );
}

export function getPageAnyLang(section: Section, slug: string, lang: Lang): DocPage | undefined {
  // Prefer the requested language; fall back to the other if the page is not
  // translated yet.
  const exact = allPages.find(
    (p) => p.section === section && p.slug === slug && p.lang === lang,
  );
  if (exact) return exact;
  return allPages.find((p) => p.section === section && p.slug === slug);
}

export function sortedPages(section: Section, lang: Lang): DocPage[] {
  return pagesFor(section)
    .filter((p) => p.lang === lang)
    .sort((a, b) => a.slug.localeCompare(b.slug, undefined, { numeric: true }));
}
