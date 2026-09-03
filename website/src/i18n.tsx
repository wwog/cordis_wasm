import { createContext, useContext } from 'react';
import type { Lang } from './lib/docs';

export interface UiStrings {
  // Brand / navigation
  brand: string;
  tagline: string;
  home: string;
  tutorial: string;
  api: string;
  sundry: string;
  semantics: string;
  repo: string;
  // Home hero
  viewRepo: string;
  getStarted: string;
  readTutorial: string;
  problemTitle: string;
  exploreDocs: string;
  // Footer
  footerNote: string;
  footerLicense: string;
  notFound: string;
  backHome: string;
}

const en: UiStrings = {
  brand: 'Cordis',
  tagline: 'Reversible-effect, reactive-DI plugin runtime for Rust & Wasmtime',
  home: 'Home',
  tutorial: 'Tutorial',
  api: 'API',
  sundry: 'Miscellaneous',
  semantics: 'Semantics',
  repo: 'GitHub',
  viewRepo: 'View on GitHub',
  getStarted: 'Get started',
  readTutorial: 'Read the tutorial',
  problemTitle: 'What problem does Cordis solve?',
  exploreDocs: 'Explore the docs',
  footerNote:
    'Cordis is a Rust + Wasmtime rewrite of the Cordis TypeScript implementation, preserving its semantics.',
  footerLicense: 'MIT licensed.',
  notFound: 'Page not found',
  backHome: 'Back home',
};

const zh: UiStrings = {
  brand: 'Cordis',
  tagline: '以可逆 effect 与响应式依赖注入为内核的 Rust 与 Wasmtime 插件运行时',
  home: '首页',
  tutorial: '快速入门',
  api: 'API',
  sundry: '杂项',
  semantics: '语义',
  repo: 'GitHub',
  viewRepo: '查看仓库',
  getStarted: '开始使用',
  readTutorial: '阅读快速入门',
  problemTitle: 'Cordis 解决了什么问题？',
  exploreDocs: '浏览文档',
  footerNote:
    'Cordis 是 Cordis TypeScript 实现的 Rust + Wasmtime 重写，保持了语义一致。',
  footerLicense: 'MIT 许可。',
  notFound: '页面不存在',
  backHome: '返回首页',
};

export const uiStrings: Record<Lang, UiStrings> = { en, zh };

interface I18nValue {
  lang: Lang;
  otherLang: Lang;
  setLang: (lang: Lang) => void;
  t: UiStrings;
}

export const I18nContext = createContext<I18nValue>({
  lang: 'en',
  otherLang: 'zh',
  setLang: () => {},
  t: en,
});

export function useI18n(): I18nValue {
  return useContext(I18nContext);
}
