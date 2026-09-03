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
 
      </div>
    </div>
  );
}
