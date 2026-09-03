import { Link } from 'react-router-dom';
import { Button, Space, Typography, Tag } from 'antd';
import { GithubOutlined, CodeOutlined, ApiOutlined } from '@ant-design/icons';
import { useI18n } from '../i18n';
import { REPO_URL } from '../constants';

const { Title } = Typography;

// Three compact value points. Kept minimal — the docs carry the depth.
function ValuePoints() {
  const { lang } = useI18n();
  const items =
    lang === 'en'
      ? [
          { icon: '⟲', title: 'Revertible effects', text: 'Every effect carries an inverse, applied LIFO on teardown.' },
          { icon: '⇄', title: 'Reactive DI', text: 'A component activates only when its dependencies are met.' },
          { icon: '⬡', title: 'Wasmtime components', text: 'Plugins are WASM components, not Rust ABI.' },
        ]
      : [
          { icon: '⟲', title: '可逆 effect', text: '每个 effect 都带逆操作，卸载时按 LIFO 执行。' },
          { icon: '⇄', title: '响应式依赖注入', text: '组件仅在依赖满足时才激活。' },
          { icon: '⬡', title: 'Wasmtime 组件', text: '插件是 WASM 组件，而非 Rust ABI。' },
        ];

  return (
    <div className="value-row">
      {items.map((it) => (
        <div className="value-item" key={it.title}>
          <span className="value-dot">{it.icon}</span>
          <h4>{it.title}</h4>
          <p>{it.text}</p>
        </div>
      ))}
    </div>
  );
}

export default function HomePage() {
  const { lang, t } = useI18n();

  const docCards = [
    { to: '/tutorial', title: t.tutorial, desc: lang === 'zh' ? '从零写一个真正的 Wasmtime 插件。' : 'Write a real Wasmtime plugin from scratch.' },
    { to: '/api', title: t.api, desc: lang === 'zh' ? '每个 crate 的公共 API。' : 'The public API surface of every crate.' },
    { to: '/semantics', title: t.semantics, desc: lang === 'zh' ? '可逆 effect 与九条规则。' : 'Revertible effects and the nine rules.' },
  ];

  return (
    <div className="home-page">
      <div className="bg-blob blob-1" />
      <div className="bg-blob blob-2" />
      <div className="bg-blob blob-3" />

      <div className="home-content">
        {/* Hero */}
        <div className="fade-up" style={{ textAlign: 'center', padding: '56px 0 0' }}>
          <Space direction="vertical" size={12} align="center" style={{ width: '100%' }}>
            <Tag color="blue" style={{ fontSize: 12 }}>
              Rust · Wasmtime Component Model
            </Tag>
            <Title level={1} style={{ margin: 0, fontSize: 46, letterSpacing: -1 }}>
              {t.brand} <span style={{ fontWeight: 400, color: 'rgba(0,0,0,0.4)' }}>(Rust)</span>
            </Title>
            <Title level={4} type="secondary" style={{ margin: 0, fontWeight: 400, maxWidth: 640 }}>
              {t.tagline}
            </Title>
            <Space size="middle" style={{ marginTop: 10 }} wrap>
              <Button type="primary" size="large" icon={<GithubOutlined />} href={REPO_URL} target="_blank">
                {t.viewRepo}
              </Button>
              <Link to="/tutorial">
                <Button size="large" icon={<CodeOutlined />}>
                  {t.getStarted}
                </Button>
              </Link>
            </Space>
          </Space>
        </div>

        {/* Compact value strip */}
        <div className="fade-up d2">
          <ValuePoints />
        </div>

        {/* Three doc entry cards */}
        <div className="fade-up d3" style={{ marginTop: 64 }}>
          <Title level={3} style={{ textAlign: 'center', fontWeight: 600, marginBottom: 24 }}>
            {t.exploreDocs}
          </Title>
          <div className="value-row" style={{ gridTemplateColumns: 'repeat(3, 1fr)', maxWidth: 860 }}>
            {docCards.map((c) => (
              <Link to={c.to} key={c.to} style={{ textDecoration: 'none' }}>
                <div className="value-item">
                  <span className="value-dot">
                    <ApiOutlined />
                  </span>
                  <h4>{c.title}</h4>
                  <p>{c.desc}</p>
                </div>
              </Link>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
