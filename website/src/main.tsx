import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import { ConfigProvider } from 'antd';
import 'antd/dist/reset.css';
import './index.css';
import App from './App';
import { I18nProvider } from './I18nProvider';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <BrowserRouter>
      <ConfigProvider
        theme={{
          token: {
            colorPrimary: '#2f6fed',
            borderRadius: 8,
          },
        }}
      >
        <I18nProvider>
          <App />
        </I18nProvider>
      </ConfigProvider>
    </BrowserRouter>
  </StrictMode>,
);
