import { useEffect } from 'react';
import { Link, useLocation } from 'react-router-dom';
import { Layout, Menu, Button, Space, Segmented } from 'antd';
import { GithubOutlined } from '@ant-design/icons';
import { useI18n } from '../i18n';
import { REPO_URL } from '../constants';
import type { Lang } from '../lib/docs';

const { Header, Content, Footer } = Layout;

export default function SiteLayout({ children }: { children: React.ReactNode }) {
  const { lang, setLang, t } = useI18n();
  const location = useLocation();

  // Sync <html lang> and the document title with the active language.
  useEffect(() => {
    document.documentElement.lang = lang;
    document.title = `${t.brand} — ${t.tagline}`;
  }, [lang, t]);

  const navItems = [
    { key: '/', label: <Link to="/">{t.home}</Link> },
    { key: '/tutorial', label: <Link to="/tutorial">{t.tutorial}</Link> },
    { key: '/api', label: <Link to="/api">{t.api}</Link> },
    { key: '/sundry', label: <Link to="/sundry">{t.sundry}</Link> },
    { key: '/semantics', label: <Link to="/semantics">{t.semantics}</Link> },
  ];

  // Highlight the top-level section for the current route.
  let selectedKey = '/';
  if (location.pathname.startsWith('/tutorial')) selectedKey = '/tutorial';
  else if (location.pathname.startsWith('/api')) selectedKey = '/api';
  else if (location.pathname.startsWith('/sundry')) selectedKey = '/sundry';
  else if (location.pathname.startsWith('/semantics')) selectedKey = '/semantics';

  const onLangChange = (value: Lang | string) => setLang(value as Lang);

  return (
    <Layout style={{ minHeight: '100vh', background: '#fff' }}>
      <Header
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          background: '#fff',
          borderBottom: '1px solid #f0f0f0',
          paddingInline: 24,
          position: 'sticky',
          top: 0,
          zIndex: 100,
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 32 }}>
          <Link to="/" style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <div
              style={{
                width: 32,
                height: 32,
                borderRadius: 8,
                background: 'linear-gradient(135deg,#2f6fed,#6aa0ff)',
                color: '#fff',
                fontWeight: 700,
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'center',
                fontSize: 16,
              }}
            >
              C
            </div>
            <span style={{ fontSize: 20, fontWeight: 700, color: '#1f2733' }}>{t.brand}</span>
          </Link>
          <Menu
            mode="horizontal"
            selectedKeys={[selectedKey]}
            items={navItems}
            style={{ flex: 1, minWidth: 0, borderBottom: 'none' }}
          />
        </div>
        <Space size="middle" align="center">
          <Segmented
            value={lang}
            onChange={onLangChange}
            options={[
              { label: '中', value: 'zh' },
              { label: 'EN', value: 'en' },
            ]}
          />
          <Button
            type="text"
            icon={<GithubOutlined />}
            href={REPO_URL}
            target="_blank"
            rel="noreferrer"
          >
            {t.repo}
          </Button>
        </Space>
      </Header>

      <Content>{children}</Content>

      <Footer style={{ textAlign: 'center', color: 'rgba(0,0,0,0.55)', background: '#fff' }}>
        <Space direction="vertical" size={4}>
          <span>
            {t.brand} · {t.footerNote}
          </span>
          <span>{t.footerLicense}</span>
        </Space>
      </Footer>
    </Layout>
  );
}
