import { useMemo } from 'react';
import { useParams, Link } from 'react-router-dom';
import { Layout, Menu, Tag, Empty } from 'antd';
import { useI18n } from '../i18n';
import { getPageAnyLang, sortedPages, type Section } from '../lib/docs';
import Markdown from '../components/Markdown';

const { Sider, Content } = Layout;

const SECTION_ROUTE: Record<Section, string> = {
  tutorial: '/tutorial',
  api: '/api',
  sundry: '/sundry',
  semantics: '/semantics',
};

export default function DocPage({ section }: { section: Section }) {
  const { slug } = useParams<{ slug: string }>();
  const { lang, t } = useI18n();

  // Find the page in the requested language; fall back to the other language.
  const page = useMemo(() => {
    if (!slug) return undefined;
    return getPageAnyLang(section, slug, lang);
  }, [section, slug, lang]);

  // Sidebar list for the current section and language.
  const sidebarPages = useMemo(() => sortedPages(section, lang), [section, lang]);

  const currentSlug = slug ?? '';
  const activePage = page?.slug ?? currentSlug;

  if (page == null) {
    return (
      <Content style={{ padding: 48, maxWidth: 1200, margin: '0 auto' }}>
        <Empty description={t.notFound}>
          <Link to="/">← {t.backHome}</Link>
        </Empty>
      </Content>
    );
  }

  // Whether the page exists in the requested language; if not, we show the
  // fallback language and flag it in the sidebar.
  const hasRequestedLang = page.lang === lang;

  return (
    <Layout style={{ background: '#fff' }}>
      <Sider
        width={280}
        style={{
          borderRight: '1px solid #f0f0f0',
          background: '#fff',
          minHeight: 'calc(100vh - 64px)',
          position: 'sticky',
          top: 64,
          height: 'calc(100vh - 64px)',
          overflow: 'auto',
        }}
      >
        <div style={{ padding: '16px 16px 8px' }}>
          {!hasRequestedLang && (
            <Tag color="orange" style={{ marginBottom: 8 }}>
              {lang === 'zh' ? '暂无中文版，显示英文' : 'English only — showing English'}
            </Tag>
          )}
        </div>
        <Menu
          mode="inline"
          selectedKeys={[activePage]}
          style={{ borderRight: 'none' }}
          items={sidebarPages.map((p) => ({
            key: p.slug,
            label: (
              <Link to={`${SECTION_ROUTE[section]}/${p.slug}`} style={{ color: 'inherit' }}>
                {p.title}
              </Link>
            ),
          }))}
        />
      </Sider>

      <Content style={{ padding: '24px 48px 64px', minWidth: 0 }}>
        <Markdown markdown={page.markdown} section={section} repoFile={page.repoFile} />
      </Content>
    </Layout>
  );
}
