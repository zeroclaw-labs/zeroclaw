# مركز توثيق JhedaiClaw

هذه الصفحة هي نقطة الدخول الرئيسية لنظام التوثيق.

آخر تحديث: **20 فبراير 2026**.

المراكز المترجمة: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## ابدأ من هنا

| أريد أن…                                          | اقرأ هذا                                                                                   |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| تثبيت وتشغيل JhedaiClaw بسرعة                     | [README.md (البدء السريع)](../README.md#quick-start)                                       |
| إعداد بأمر واحد                                   | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| البحث عن أوامر حسب المهمة                         | [commands-reference.md](reference/cli/commands-reference.md)                               |
| التحقق السريع من مفاتيح وقيم الإعدادات الافتراضية | [config-reference.md](reference/api/config-reference.md)                                   |
| إعداد مزودين/نقاط وصول مخصصة                      | [custom-providers.md](contributing/custom-providers.md)                                    |
| إعداد مزود Z.AI / GLM                             | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| استخدام أنماط تكامل LangGraph                     | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| تشغيل بيئة التنفيذ (دليل العمليات اليومية)        | [operations-runbook.md](ops/operations-runbook.md)                                         |
| استكشاف مشاكل التثبيت/التشغيل/القنوات وإصلاحها    | [troubleshooting.md](ops/troubleshooting.md)                                               |
| تشغيل إعداد وتشخيص غرف Matrix المشفرة             | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| تصفح التوثيق حسب الفئة                            | [SUMMARY.md](SUMMARY.md)                                                                   |
| عرض لقطة توثيق طلبات السحب/المشاكل                | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## شجرة القرار السريعة (10 ثوانٍ)

- تحتاج إلى الإعداد أو التثبيت الأولي؟ ← [setup-guides/README.md](setup-guides/README.md)
- تحتاج مفاتيح CLI/الإعدادات بالتحديد؟ ← [reference/README.md](reference/README.md)
- تحتاج عمليات الإنتاج/الخدمة؟ ← [ops/README.md](ops/README.md)
- ترى أعطالاً أو تراجعات؟ ← [troubleshooting.md](ops/troubleshooting.md)
- تعمل على تقوية الأمان أو خارطة الطريق؟ ← [security/README.md](security/README.md)
- تعمل مع لوحات/أجهزة طرفية؟ ← [hardware/README.md](hardware/README.md)
- المساهمة/المراجعة/سير عمل CI؟ ← [contributing/README.md](contributing/README.md)
- تريد الخريطة الكاملة؟ ← [SUMMARY.md](SUMMARY.md)

## المجموعات (موصى بها)

- البدء: [setup-guides/README.md](setup-guides/README.md)
- كتالوجات المراجع: [reference/README.md](reference/README.md)
- العمليات والنشر: [ops/README.md](ops/README.md)
- توثيق الأمان: [security/README.md](security/README.md)
- العتاد/الأجهزة الطرفية: [hardware/README.md](hardware/README.md)
- المساهمة/CI: [contributing/README.md](contributing/README.md)
- لقطات المشروع: [maintainers/README.md](maintainers/README.md)

## حسب الجمهور

### المستخدمون / المشغّلون

- [commands-reference.md](reference/cli/commands-reference.md) — البحث عن أوامر حسب سير العمل
- [providers-reference.md](reference/api/providers-reference.md) — معرّفات المزودين، الأسماء المستعارة، متغيرات بيئة بيانات الاعتماد
- [channels-reference.md](reference/api/channels-reference.md) — قدرات القنوات ومسارات الإعداد
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — إعداد غرف Matrix المشفرة (E2EE) وتشخيص عدم الاستجابة
- [config-reference.md](reference/api/config-reference.md) — مفاتيح الإعدادات عالية الأهمية والقيم الافتراضية الآمنة
- [custom-providers.md](contributing/custom-providers.md) — أنماط تكامل المزود المخصص/عنوان URL الأساسي
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — إعداد Z.AI/GLM ومصفوفة نقاط الوصول
- [langgraph-integration.md](contributing/langgraph-integration.md) — تكامل احتياطي لحالات حدود النموذج/استدعاء الأدوات
- [operations-runbook.md](ops/operations-runbook.md) — عمليات التشغيل اليومية وتدفقات التراجع
- [troubleshooting.md](ops/troubleshooting.md) — بصمات الأعطال الشائعة وخطوات الاسترداد

### المساهمون / المشرفون

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### الأمان / الموثوقية

> ملاحظة: يتضمن هذا القسم مستندات مقترحات/خارطة طريق. للسلوك الحالي، ابدأ بـ [config-reference.md](reference/api/config-reference.md) و[operations-runbook.md](ops/operations-runbook.md) و[troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## التنقل في النظام والحوكمة

- جدول المحتويات الموحد: [SUMMARY.md](SUMMARY.md)
- خريطة هيكل التوثيق (اللغة/القسم/الوظيفة): [structure/README.md](maintainers/structure-README.md)
- جرد/تصنيف التوثيق: [docs-inventory.md](maintainers/docs-inventory.md)
- لقطة فرز المشروع: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## لغات أخرى

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
