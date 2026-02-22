import { useState, useEffect } from 'react';
import { getStatus } from './api';

// ---------------------------------------------------------------------------
// Translation dictionaries
// ---------------------------------------------------------------------------

export type Locale = 'en' | 'tr' | 'zh-CN';

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

  'zh-CN': {
    // Navigation
    'nav.dashboard': '仪表盘',
    'nav.agent': '智能体',
    'nav.tools': '工具',
    'nav.cron': '定时任务',
    'nav.integrations': '集成',
    'nav.memory': '记忆',
    'nav.config': '配置',
    'nav.cost': '成本追踪',
    'nav.logs': '日志',
    'nav.doctor': '诊断',

    // Dashboard
    'dashboard.title': '仪表盘',
    'dashboard.provider': '提供商',
    'dashboard.model': '模型',
    'dashboard.uptime': '运行时长',
    'dashboard.temperature': '温度',
    'dashboard.gateway_port': '网关端口',
    'dashboard.locale': '语言区域',
    'dashboard.memory_backend': '记忆后端',
    'dashboard.paired': '已配对',
    'dashboard.channels': '渠道',
    'dashboard.health': '健康状态',
    'dashboard.status': '状态',
    'dashboard.overview': '总览',
    'dashboard.system_info': '系统信息',
    'dashboard.quick_actions': '快捷操作',

    // Agent / Chat
    'agent.title': '智能体聊天',
    'agent.send': '发送',
    'agent.placeholder': '输入消息...',
    'agent.connecting': '连接中...',
    'agent.connected': '已连接',
    'agent.disconnected': '已断开连接',
    'agent.reconnecting': '重连中...',
    'agent.thinking': '思考中...',
    'agent.tool_call': '工具调用',
    'agent.tool_result': '工具结果',

    // Tools
    'tools.title': '可用工具',
    'tools.name': '名称',
    'tools.description': '描述',
    'tools.parameters': '参数',
    'tools.search': '搜索工具...',
    'tools.empty': '暂无可用工具。',
    'tools.count': '工具总数',

    // Cron
    'cron.title': '定时任务',
    'cron.add': '添加任务',
    'cron.delete': '删除',
    'cron.enable': '启用',
    'cron.disable': '禁用',
    'cron.name': '名称',
    'cron.command': '命令',
    'cron.schedule': '计划',
    'cron.next_run': '下次运行',
    'cron.last_run': '上次运行',
    'cron.last_status': '上次状态',
    'cron.enabled': '已启用',
    'cron.empty': '暂无定时任务。',
    'cron.confirm_delete': '确定要删除此任务吗？',

    // Integrations
    'integrations.title': '集成',
    'integrations.available': '可用',
    'integrations.active': '已激活',
    'integrations.coming_soon': '即将推出',
    'integrations.category': '分类',
    'integrations.status': '状态',
    'integrations.search': '搜索集成...',
    'integrations.empty': '未找到集成。',
    'integrations.activate': '激活',
    'integrations.deactivate': '停用',

    // Memory
    'memory.title': '记忆存储',
    'memory.search': '搜索记忆...',
    'memory.add': '存储记忆',
    'memory.delete': '删除',
    'memory.key': '键',
    'memory.content': '内容',
    'memory.category': '分类',
    'memory.timestamp': '时间戳',
    'memory.session': '会话',
    'memory.score': '评分',
    'memory.empty': '未找到记忆条目。',
    'memory.confirm_delete': '确定要删除此记忆条目吗？',
    'memory.all_categories': '全部分类',

    // Config
    'config.title': '配置',
    'config.save': '保存',
    'config.reset': '重置',
    'config.saved': '配置保存成功。',
    'config.error': '配置保存失败。',
    'config.loading': '配置加载中...',
    'config.editor_placeholder': 'TOML 配置...',

    // Cost
    'cost.title': '成本追踪',
    'cost.session': '会话成本',
    'cost.daily': '每日成本',
    'cost.monthly': '每月成本',
    'cost.total_tokens': 'Token 总数',
    'cost.request_count': '请求数',
    'cost.by_model': '按模型统计成本',
    'cost.model': '模型',
    'cost.tokens': 'Token',
    'cost.requests': '请求',
    'cost.usd': '成本（USD）',

    // Logs
    'logs.title': '实时日志',
    'logs.clear': '清空',
    'logs.pause': '暂停',
    'logs.resume': '继续',
    'logs.filter': '筛选日志...',
    'logs.empty': '暂无日志条目。',
    'logs.connected': '已连接到事件流。',
    'logs.disconnected': '与事件流断开连接。',

    // Doctor
    'doctor.title': '系统诊断',
    'doctor.run': '运行诊断',
    'doctor.running': '正在运行诊断...',
    'doctor.ok': '正常',
    'doctor.warn': '警告',
    'doctor.error': '错误',
    'doctor.severity': '严重级别',
    'doctor.category': '分类',
    'doctor.message': '消息',
    'doctor.empty': '尚未运行诊断。',
    'doctor.summary': '诊断摘要',

    // Auth / Pairing
    'auth.pair': '设备配对',
    'auth.pairing_code': '配对码',
    'auth.pair_button': '配对',
    'auth.logout': '退出登录',
    'auth.pairing_success': '配对成功！',
    'auth.pairing_failed': '配对失败，请重试。',
    'auth.enter_code': '输入配对码以连接到智能体。',

    // Common
    'common.loading': '加载中...',
    'common.error': '发生错误。',
    'common.retry': '重试',
    'common.cancel': '取消',
    'common.confirm': '确认',
    'common.save': '保存',
    'common.delete': '删除',
    'common.edit': '编辑',
    'common.close': '关闭',
    'common.yes': '是',
    'common.no': '否',
    'common.search': '搜索...',
    'common.no_data': '暂无数据。',
    'common.refresh': '刷新',
    'common.back': '返回',
    'common.actions': '操作',
    'common.name': '名称',
    'common.description': '描述',
    'common.status': '状态',
    'common.created': '创建时间',
    'common.updated': '更新时间',

    // Health
    'health.title': '系统健康',
    'health.component': '组件',
    'health.status': '状态',
    'health.last_ok': '最近正常',
    'health.last_error': '最近错误',
    'health.restart_count': '重启次数',
    'health.pid': '进程 ID',
    'health.uptime': '运行时长',
    'health.updated_at': '最后更新',
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

function normalizeLocale(locale: string | undefined): Locale {
  const lowered = locale?.toLowerCase();
  if (lowered?.startsWith('tr')) return 'tr';
  if (lowered === 'zh' || lowered?.startsWith('zh-')) return 'zh-CN';
  return 'en';
}

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
        const detected = normalizeLocale(status.locale);
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
