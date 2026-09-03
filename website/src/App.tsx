import { Routes, Route, Navigate } from 'react-router-dom';
import SiteLayout from './components/SiteLayout';
import HomePage from './pages/HomePage';
import DocPage from './pages/DocPage';
import NotFound from './pages/NotFound';

export default function App() {
  return (
    <SiteLayout>
      <Routes>
        <Route path="/" element={<HomePage />} />
        {/* Language-neutral doc routes: /tutorial/:slug, /api/:slug, /sundry/:slug, /semantics/:slug */}
        <Route path="/tutorial/:slug" element={<DocPage section="tutorial" />} />
        <Route path="/api/:slug" element={<DocPage section="api" />} />
        <Route path="/sundry/:slug" element={<DocPage section="sundry" />} />
        <Route path="/semantics/:slug" element={<DocPage section="semantics" />} />
        <Route path="/tutorial" element={<Navigate to="/tutorial/index" replace />} />
        <Route path="/api" element={<Navigate to="/api/index" replace />} />
        <Route path="/sundry" element={<Navigate to="/sundry/benchmarks" replace />} />
        <Route path="/semantics" element={<Navigate to="/semantics/semantics" replace />} />
        <Route path="*" element={<NotFound />} />
      </Routes>
    </SiteLayout>
  );
}
