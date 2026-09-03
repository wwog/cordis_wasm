import { Link } from 'react-router-dom';
import { Button, Result } from 'antd';
import { useI18n } from '../i18n';

export default function NotFound() {
  const { t } = useI18n();
  return (
    <div style={{ padding: 48, maxWidth: 800, margin: '0 auto' }}>
      <Result
        status="404"
        title="404"
        subTitle={t.notFound}
        extra={
          <Link to="/">
            <Button type="primary">{t.backHome}</Button>
          </Link>
        }
      />
    </div>
  );
}
