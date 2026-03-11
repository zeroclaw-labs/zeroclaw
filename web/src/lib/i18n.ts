import { useState, useEffect } from 'react';
import { getStatus } from './api';

// ---------------------------------------------------------------------------
// Translation dictionaries
// ---------------------------------------------------------------------------

export type Locale = 'en' | 'tr';

const translations: Record<Locale, Record<string, string>> = {
  en: {
    // Navigation
    'nav.dashboard': 'Dashboard',
    'nav.agent': 'Agent',
    'nav.tools': 'Tools',
    'nav.cron': 'Scheduled Jobs',
    'nav.integrations': 'Integrations',
    'nav.memory': 'Memory',
    'nav.config': 'Configuration',
    'nav.cost': 'Cost Tracker',
    'nav.logs': 'Logs',
    'nav.doctor': 'Doctor',

    // Dashboard
    'dashboard.title': 'Dashboard',
    'dashboard.provider': 'Provider',
    'dashboard.model': 'Model',
    'dashboard.uptime': 'Uptime',
    'dashboard.temperature': 'Temperature',
    'dashboard.gateway_port': 'Gateway Port',
    'dashboard.locale': 'Locale',
    'dashboard.memory_backend': 'Memory Backend',
    'dashboard.paired': 'Paired',
    'dashboard.channels': 'Channels',
    'dashboard.health': 'Health',
    'dashboard.status': 'Status',
    'dashboard.overview': 'Overview',
    'dashboard.system_info': 'System Information',
    'dashboard.quick_actions': 'Quick Actions',

    // Agent / Chat
    'agent.title': 'Agent Chat',
    'agent.send': 'Send',
    'agent.placeholder': 'Type a message...',
    'agent.connecting': 'Connecting...',
    'agent.connected': 'Connected',
    'agent.disconnected': 'Disconnected',
    'agent.reconnecting': 'Reconnecting...',
    'agent.thinking': 'Thinking...',
    'agent.tool_call': 'Tool Call',
    'agent.tool_result': 'Tool Result',

    // Tools
    'tools.title': 'Available Tools',
    'tools.name': 'Name',
    'tools.description': 'Description',
    'tools.parameters': 'Parameters',
    'tools.search': 'Search tools...',
    'tools.empty': 'No tools available.',
    'tools.count': 'Total tools',

    // Cron
    'cron.title': 'Scheduled Jobs',
    'cron.add': 'Add Job',
    'cron.delete': 'Delete',
    'cron.enable': 'Enable',
    'cron.disable': 'Disable',
    'cron.name': 'Name',
    'cron.command': 'Command',
    'cron.schedule': 'Schedule',
    'cron.next_run': 'Next Run',
    'cron.last_run': 'Last Run',
    'cron.last_status': 'Last Status',
    'cron.enabled': 'Enabled',
    'cron.empty': 'No scheduled jobs.',
    'cron.confirm_delete': 'Are you sure you want to delete this job?',

    // Integrations
    'integrations.title': 'Integrations',
    'integrations.available': 'Available',
    'integrations.active': 'Active',
    'integrations.coming_soon': 'Coming Soon',
    'integrations.category': 'Category',
    'integrations.status': 'Status',
    'integrations.search': 'Search integrations...',
    'integrations.empty': 'No integrations found.',
    'integrations.activate': 'Activate',
    'integrations.deactivate': 'Deactivate',

    // Memory
    'memory.title': 'Memory Store',
    'memory.search': 'Search memory...',
    'memory.add': 'Store Memory',
    'memory.delete': 'Delete',
    'memory.key': 'Key',
    'memory.content': 'Content',
    'memory.category': 'Category',
    'memory.timestamp': 'Timestamp',
    'memory.session': 'Session',
    'memory.score': 'Score',
    'memory.empty': 'No memory entries found.',
    'memory.confirm_delete': 'Are you sure you want to delete this memory entry?',
    'memory.all_categories': 'All Categories',

    // Config
    'config.title': 'Configuration',
    'config.save': 'Save',
    'config.reset': 'Reset',
    'config.saved': 'Configuration saved successfully.',
    'config.error': 'Failed to save configuration.',
    'config.loading': 'Loading configuration...',
    'config.editor_placeholder': 'TOML configuration...',

    // Cost
    'cost.title': 'Cost Tracker',
    'cost.session': 'Session Cost',
    'cost.daily': 'Daily Cost',
    'cost.monthly': 'Monthly Cost',
    'cost.total_tokens': 'Total Tokens',
    'cost.request_count': 'Requests',
    'cost.by_model': 'Cost by Model',
    'cost.model': 'Model',
    'cost.tokens': 'Tokens',
    'cost.requests': 'Requests',
    'cost.usd': 'Cost (USD)',

    // Logs
    'logs.title': 'Live Logs',
    'logs.clear': 'Clear',
    'logs.pause': 'Pause',
    'logs.resume': 'Resume',
    'logs.filter': 'Filter logs...',
    'logs.empty': 'No log entries.',
    'logs.connected': 'Connected to event stream.',
    'logs.disconnected': 'Disconnected from event stream.',

    // Doctor
    'doctor.title': 'System Diagnostics',
    'doctor.run': 'Run Diagnostics',
    'doctor.running': 'Running diagnostics...',
    'doctor.ok': 'OK',
    'doctor.warn': 'Warning',
    'doctor.error': 'Error',
    'doctor.severity': 'Severity',
    'doctor.category': 'Category',
    'doctor.message': 'Message',
    'doctor.empty': 'No diagnostics have been run yet.',
    'doctor.summary': 'Diagnostic Summary',

    // Auth / Pairing
    'auth.pair': 'Pair Device',
    'auth.pairing_code': 'Pairing Code',
    'auth.pair_button': 'Pair',
    'auth.logout': 'Logout',
    'auth.pairing_success': 'Pairing successful!',
    'auth.pairing_failed': 'Pairing failed. Please try again.',
    'auth.enter_code': 'Enter your pairing code to connect to the agent.',

    // Common
    'common.loading': 'Loading...',
    'common.error': 'An error occurred.',
    'common.retry': 'Retry',
    'common.cancel': 'Cancel',
    'common.confirm': 'Confirm',
    'common.save': 'Save',
    'common.delete': 'Delete',
    'common.edit': 'Edit',
    'common.close': 'Close',
    'common.yes': 'Yes',
    'common.no': 'No',
    'common.search': 'Search...',
    'common.no_data': 'No data available.',
    'common.refresh': 'Refresh',
    'common.back': 'Back',
    'common.actions': 'Actions',
    'common.name': 'Name',
    'common.description': 'Description',
    'common.status': 'Status',
    'common.created': 'Created',
    'common.updated': 'Updated',

    // Health
    'health.title': 'System Health',
    'health.component': 'Component',
    'health.status': 'Status',
    'health.last_ok': 'Last OK',
    'health.last_error': 'Last Error',
    'health.restart_count': 'Restarts',
    'health.pid': 'Process ID',
    'health.uptime': 'Uptime',
    'health.updated_at': 'Last Updated',
  },

  tr: {
    // Navigation
    'nav.dashboard': 'Kontrol Paneli',
    'nav.agent': 'Ajan',
    'nav.tools': 'Araclar',
    'nav.cron': 'Zamanlanmis Gorevler',
    'nav.integrations': 'Entegrasyonlar',
    'nav.memory': 'Hafiza',
    'nav.config': 'Yapilandirma',
    'nav.cost': 'Maliyet Takibi',
    'nav.logs': 'Kayitlar',
    'nav.doctor': 'Doktor',

    // Dashboard
    'dashboard.title': 'Kontrol Paneli',
    'dashboard.provider': 'Saglayici',
    'dashboard.model': 'Model',
    'dashboard.uptime': 'Calisma Suresi',
    'dashboard.temperature': 'Sicaklik',
    'dashboard.gateway_port': 'Gecit Portu',
    'dashboard.locale': 'Yerel Ayar',
    'dashboard.memory_backend': 'Hafiza Motoru',
    'dashboard.paired': 'Eslestirilmis',
    'dashboard.channels': 'Kanallar',
    'dashboard.health': 'Saglik',
    'dashboard.status': 'Durum',
    'dashboard.overview': 'Genel Bakis',
    'dashboard.system_info': 'Sistem Bilgisi',
    'dashboard.quick_actions': 'Hizli Islemler',

    // Agent / Chat
    'agent.title': 'Ajan Sohbet',
    'agent.send': 'Gonder',
    'agent.placeholder': 'Bir mesaj yazin...',
    'agent.connecting': 'Baglaniyor...',
    'agent.connected': 'Bagli',
    'agent.disconnected': 'Baglanti Kesildi',
    'agent.reconnecting': 'Yeniden Baglaniyor...',
    'agent.thinking': 'Dusunuyor...',
    'agent.tool_call': 'Arac Cagrisi',
    'agent.tool_result': 'Arac Sonucu',

    // Tools
    'tools.title': 'Mevcut Araclar',
    'tools.name': 'Ad',
    'tools.description': 'Aciklama',
    'tools.parameters': 'Parametreler',
    'tools.search': 'Arac ara...',
    'tools.empty': 'Mevcut arac yok.',
    'tools.count': 'Toplam arac',

    // Cron
    'cron.title': 'Zamanlanmis Gorevler',
    'cron.add': 'Gorev Ekle',
    'cron.delete': 'Sil',
    'cron.enable': 'Etkinlestir',
    'cron.disable': 'Devre Disi Birak',
    'cron.name': 'Ad',
    'cron.command': 'Komut',
    'cron.schedule': 'Zamanlama',
    'cron.next_run': 'Sonraki Calistirma',
    'cron.last_run': 'Son Calistirma',
    'cron.last_status': 'Son Durum',
    'cron.enabled': 'Etkin',
    'cron.empty': 'Zamanlanmis gorev yok.',
    'cron.confirm_delete': 'Bu gorevi silmek istediginizden emin misiniz?',

    // Integrations
    'integrations.title': 'Entegrasyonlar',
    'integrations.available': 'Mevcut',
    'integrations.active': 'Aktif',
    'integrations.coming_soon': 'Yakinda',
    'integrations.category': 'Kategori',
    'integrations.status': 'Durum',
    'integrations.search': 'Entegrasyon ara...',
    'integrations.empty': 'Entegrasyon bulunamadi.',
    'integrations.activate': 'Etkinlestir',
    'integrations.deactivate': 'Devre Disi Birak',

    // Memory
    'memory.title': 'Hafiza Deposu',
    'memory.search': 'Hafizada ara...',
    'memory.add': 'Hafiza Kaydet',
    'memory.delete': 'Sil',
    'memory.key': 'Anahtar',
    'memory.content': 'Icerik',
    'memory.category': 'Kategori',
    'memory.timestamp': 'Zaman Damgasi',
    'memory.session': 'Oturum',
    'memory.score': 'Skor',
    'memory.empty': 'Hafiza kaydi bulunamadi.',
    'memory.confirm_delete': 'Bu hafiza kaydini silmek istediginizden emin misiniz?',
    'memory.all_categories': 'Tum Kategoriler',

    // Config
    'config.title': 'Yapilandirma',
    'config.save': 'Kaydet',
    'config.reset': 'Sifirla',
    'config.saved': 'Yapilandirma basariyla kaydedildi.',
    'config.error': 'Yapilandirma kaydedilemedi.',
    'config.loading': 'Yapilandirma yukleniyor...',
    'config.editor_placeholder': 'TOML yapilandirmasi...',

    // Cost
    'cost.title': 'Maliyet Takibi',
    'cost.session': 'Oturum Maliyeti',
    'cost.daily': 'Gunluk Maliyet',
    'cost.monthly': 'Aylik Maliyet',
    'cost.total_tokens': 'Toplam Token',
    'cost.request_count': 'Istekler',
    'cost.by_model': 'Modele Gore Maliyet',
    'cost.model': 'Model',
    'cost.tokens': 'Token',
    'cost.requests': 'Istekler',
    'cost.usd': 'Maliyet (USD)',

    // Logs
    'logs.title': 'Canli Kayitlar',
    'logs.clear': 'Temizle',
    'logs.pause': 'Duraklat',
    'logs.resume': 'Devam Et',
    'logs.filter': 'Kayitlari filtrele...',
    'logs.empty': 'Kayit girisi yok.',
    'logs.connected': 'Olay akisina baglandi.',
    'logs.disconnected': 'Olay akisi baglantisi kesildi.',

    // Doctor
    'doctor.title': 'Sistem Teshisleri',
    'doctor.run': 'Teshis Calistir',
    'doctor.running': 'Teshisler calistiriliyor...',
    'doctor.ok': 'Tamam',
    'doctor.warn': 'Uyari',
    'doctor.error': 'Hata',
    'doctor.severity': 'Ciddiyet',
    'doctor.category': 'Kategori',
    'doctor.message': 'Mesaj',
    'doctor.empty': 'Henuz teshis calistirilmadi.',
    'doctor.summary': 'Teshis Ozeti',

    // Auth / Pairing
    'auth.pair': 'Cihaz Esle',
    'auth.pairing_code': 'Eslestirme Kodu',
    'auth.pair_button': 'Esle',
    'auth.logout': 'Cikis Yap',
    'auth.pairing_success': 'Eslestirme basarili!',
    'auth.pairing_failed': 'Eslestirme basarisiz. Lutfen tekrar deneyin.',
    'auth.enter_code': 'Ajana baglanmak icin eslestirme kodunuzu girin.',

    // Common
    'common.loading': 'Yukleniyor...',
    'common.error': 'Bir hata olustu.',
    'common.retry': 'Tekrar Dene',
    'common.cancel': 'Iptal',
    'common.confirm': 'Onayla',
    'common.save': 'Kaydet',
    'common.delete': 'Sil',
    'common.edit': 'Duzenle',
    'common.close': 'Kapat',
    'common.yes': 'Evet',
    'common.no': 'Hayir',
    'common.search': 'Ara...',
    'common.no_data': 'Veri mevcut degil.',
    'common.refresh': 'Yenile',
    'common.back': 'Geri',
    'common.actions': 'Islemler',
    'common.name': 'Ad',
    'common.description': 'Aciklama',
    'common.status': 'Durum',
    'common.created': 'Olusturulma',
    'common.updated': 'Guncellenme',

    // Health
    'health.title': 'Sistem Sagligi',
    'health.component': 'Bilesen',
    'health.status': 'Durum',
    'health.last_ok': 'Son Basarili',
    'health.last_error': 'Son Hata',
    'health.restart_count': 'Yeniden Baslatmalar',
    'health.pid': 'Islem Kimligi',
    'health.uptime': 'Calisma Suresi',
    'health.updated_at': 'Son Guncelleme',
  },
};

// ---------------------------------------------------------------------------
// Current locale state
// ---------------------------------------------------------------------------

let currentLocale: Locale = 'en';

export function getLocale(): Locale {
  return currentLocale;
}

export function setLocale(locale: Locale): void {
  currentLocale = locale;
}

// ---------------------------------------------------------------------------
// Translation function
// ---------------------------------------------------------------------------

/**
 * Translate a key using the current locale. Returns the key itself if no
 * translation is found.
 */
export function t(key: string): string {
  return translations[currentLocale]?.[key] ?? translations.en[key] ?? key;
}

/**
 * Get the translation for a specific locale. Falls back to English, then to the
 * raw key.
 */
export function tLocale(key: string, locale: Locale): string {
  return translations[locale]?.[key] ?? translations.en[key] ?? key;
}

// ---------------------------------------------------------------------------
// React hook
// ---------------------------------------------------------------------------

/**
 * React hook that fetches the locale from /api/status on mount and keeps the
 * i18n module in sync. Returns the current locale and a `t` helper bound to it.
 */
export function useLocale(): { locale: Locale; t: (key: string) => string } {
  const [locale, setLocaleState] = useState<Locale>(currentLocale);

  useEffect(() => {
    let cancelled = false;

    getStatus()
      .then((status) => {
        if (cancelled) return;
        const detected = status.locale?.toLowerCase().startsWith('tr')
          ? 'tr'
          : 'en';
        setLocale(detected);
        setLocaleState(detected);
      })
      .catch(() => {
        // Keep default locale on error
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return {
    locale,
    t: (key: string) => tLocale(key, locale),
  };
}
