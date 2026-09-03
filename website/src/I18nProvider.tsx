import { useMemo, useState } from 'react';
import type { Lang } from './lib/docs';
import { I18nContext, uiStrings } from './i18n';

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLang] = useState<Lang>('zh');

  const value = useMemo(
    () => ({
      lang,
      otherLang: (lang === 'zh' ? 'en' : 'zh') as Lang,
      setLang,
      t: uiStrings[lang],
    }),
    [lang],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}
