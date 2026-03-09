'use client';

import { useState, useEffect } from 'react';
import { getStatus } from './gateway-api';

export type Locale = 'en' | 'ko' | 'zh-CN';

const translations: Record<Locale, Record<string, string>> = {
  en: {
    'nav.dashboard': 'Dashboard',
    'nav.agent': 'Agent',
    'nav.tools': 'Tools',
    'nav.cron': 'Scheduled Jobs',
    'nav.integrations': 'Integrations',
    'nav.memory': 'Memory',
    'nav.devices': 'Devices',
    'nav.config': 'Configuration',
    'nav.cost': 'Cost Tracker',
    'nav.logs': 'Logs',
    'nav.doctor': 'Doctor',
    'auth.logout': 'Logout',
    'common.loading': 'Loading...',
  },
  ko: {
    'nav.dashboard': '대시보드',
    'nav.agent': '에이전트',
    'nav.tools': '도구',
    'nav.cron': '예약 작업',
    'nav.integrations': '통합',
    'nav.memory': '메모리',
    'nav.devices': '디바이스',
    'nav.config': '설정',
    'nav.cost': '비용 추적',
    'nav.logs': '로그',
    'nav.doctor': '진단',
    'auth.logout': '로그아웃',
    'common.loading': '로딩 중...',
  },
  'zh-CN': {
    'nav.dashboard': '仪表盘',
    'nav.agent': '智能体',
    'nav.tools': '工具',
    'nav.cron': '定时任务',
    'nav.integrations': '集成',
    'nav.memory': '记忆',
    'nav.devices': '设备',
    'nav.config': '配置',
    'nav.cost': '成本追踪',
    'nav.logs': '日志',
    'nav.doctor': '诊断',
    'auth.logout': '退出登录',
    'common.loading': '加载中...',
  },
};

let currentLocale: Locale = 'en';

export function getLocale(): Locale {
  return currentLocale;
}

export function setLocale(locale: Locale): void {
  currentLocale = locale;
}

export function t(key: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? key;
}

export function tLocale(key: string, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}

function normalizeLocale(locale: string | undefined): Locale {
  const lowered = locale?.toLowerCase();
  if (lowered?.startsWith('ko')) return 'ko';
  if (lowered === 'zh' || lowered?.startsWith('zh-')) return 'zh-CN';
  return 'en';
}

export function useLocale(): { locale: Locale; t: (key: string) => string } {
  const [locale, setLocaleState] = useState<Locale>(currentLocale);

  useEffect(() => {
    let cancelled = false;

    getStatus()
      .then((status) => {
        if (cancelled) return;
        const detected = normalizeLocale(status.locale);
        setLocale(detected);
        setLocaleState(detected);
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  }, []);

  return {
    locale,
    t: (key: string) => tLocale(key, locale),
  };
}
