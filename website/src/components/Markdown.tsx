import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeHighlight from 'rehype-highlight';
import { useNavigate } from 'react-router-dom';
import 'highlight.js/styles/github-dark.css';
import type { Section } from '../lib/docs';
import { REPO_URL } from '../constants';

interface MarkdownProps {
  markdown: string;
  /** The section the markdown belongs to, used to resolve relative links. */
  section: Section;
  /** Repo-relative source path of the current page, e.g. "docs/tutorial/index.zh.md". */
  repoFile: string;
}

// Resolve a relative `.md` link to an internal, language-neutral route.
// Markdown links in the docs use two shapes:
//   <name>.md / <name>.zh.md   -> sibling in the same section
//   ../<name>.md               -> cross-section (e.g. ../../api/index.md, ../semantics.md)
// Links that point outside the sections we serve (e.g. ../ts-docs/...) are
// resolved against the repo, so we can fall back to the GitHub blob page.
const KNOWN_SECTIONS = new Set(['api', 'tutorial', 'sundry']);

function repoPathFromLink(href: string, file: string): string {
  // Resolve the link relative to the current page's directory, naively, to get
  // a repo-relative path. Both are POSIX-style in the docs.
  const currentDir = file.slice(0, file.lastIndexOf('/') + 1);
  const parts = (currentDir + href).split('/');
  const stack: string[] = [];
  for (const part of parts) {
    if (part === '..') stack.pop();
    else if (part !== '.' && part !== '') stack.push(part);
  }
  return stack.join('/');
}

function resolveDocLink(href: string, section: Section): string | null | 'external' {
  if (!href.endsWith('.md')) return null;

  const isCrossSection = href.startsWith('../');
  const base = href.split('/').pop()!;
  const slug = base.replace(/\.md$/, '').replace(/\.zh$/, '');

  if (isCrossSection) {
    const parts = href.split('/');
    // Meaningful segments after the leading ".." (e.g. ["semantics.zh.md"]).
    const rest = parts.filter((p) => p && !p.startsWith('..'));
    // A single segment is a file living at docs/ root — the only one is semantics.md.
    if (rest.length <= 1) return `/semantics/semantics`;
    const sectionName = rest[0];
    if (KNOWN_SECTIONS.has(sectionName)) return `/${sectionName}/${slug}`;
    // Anything else (ts-docs, a README, ...) is not part of this site.
    return 'external';
  }

  // Same-section sibling.
  return `/${section}/${slug}`;
}

export default function Markdown({ markdown, section, repoFile }: MarkdownProps) {
  const navigate = useNavigate();

  return (
    <div className="doc-content">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: false }]]}
        components={{
          a: ({ href, children, ...props }) => {
            const resolved = resolveDocLink(href ?? '', section);
            if (resolved === 'external') {
              // Cross-section link into a part of the repo we don't serve.
              const target = `${REPO_URL}/blob/master/${repoPathFromLink(href ?? '', repoFile)}`;
              return (
                <a {...props} href={target} target="_blank" rel="noreferrer">
                  {children}
                </a>
              );
            }
            if (resolved) {
              return (
                <a
                  {...props}
                  href={`#${resolved}`}
                  onClick={(e) => {
                    e.preventDefault();
                    navigate(resolved);
                  }}
                >
                  {children}
                </a>
              );
            }
            // External links: open in a new tab.
            return (
              <a {...props} href={href} target="_blank" rel="noreferrer">
                {children}
              </a>
            );
          },
          h2: ({ children }) => <h2>{children}</h2>,
          h3: ({ children }) => <h3>{children}</h3>,
        }}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}
