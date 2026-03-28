# מרכז התיעוד של ZeroClaw

דף זה הוא נקודת הכניסה הראשית למערכת התיעוד.

עדכון אחרון: **20 בפברואר 2026**.

מרכזים מתורגמים: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## התחילו כאן

| אני רוצה…                                                          | קראו זאת                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| להתקין ולהריץ את ZeroClaw במהירות                                   | [README.md (התחלה מהירה)](../README.md#quick-start)                            |
| אתחול בפקודה אחת                                                   | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| למצוא פקודות לפי משימה                                              | [commands-reference.md](reference/cli/commands-reference.md)                   |
| לבדוק במהירות מפתחות ובררות מחדל של הגדרות                         | [config-reference.md](reference/api/config-reference.md)                       |
| להגדיר ספקים/נקודות קצה מותאמים אישית                               | [custom-providers.md](contributing/custom-providers.md)                        |
| להגדיר את ספק Z.AI / GLM                                           | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| להשתמש בתבניות שילוב LangGraph                                     | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| להפעיל את סביבת הריצה (runbook יום-2)                               | [operations-runbook.md](ops/operations-runbook.md)                             |
| לפתור בעיות התקנה/סביבת ריצה/ערוץ                                   | [troubleshooting.md](ops/troubleshooting.md)                                   |
| להריץ הגדרה ואבחון של חדרים מוצפנים ב-Matrix                       | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| לדפדף בתיעוד לפי קטגוריה                                           | [SUMMARY.md](SUMMARY.md)                                                      |
| לראות תמונת מצב של PR/issues של הפרויקט                            | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## עץ החלטה מהיר (10 שניות)

- צריכים הגדרה או התקנה ראשונית? → [setup-guides/README.md](setup-guides/README.md)
- צריכים מפתחות CLI/הגדרות מדויקים? → [reference/README.md](reference/README.md)
- צריכים פעולות ייצור/שירות? → [ops/README.md](ops/README.md)
- רואים כשלים או רגרסיות? → [troubleshooting.md](ops/troubleshooting.md)
- עובדים על הקשחת אבטחה או מפת דרכים? → [security/README.md](security/README.md)
- עובדים עם לוחות/ציוד היקפי? → [hardware/README.md](hardware/README.md)
- תרומה/סקירה/זרימת עבודה CI? → [contributing/README.md](contributing/README.md)
- רוצים את המפה המלאה? → [SUMMARY.md](SUMMARY.md)

## אוספים (מומלצים)

- התחלה: [setup-guides/README.md](setup-guides/README.md)
- קטלוגי עיון: [reference/README.md](reference/README.md)
- תפעול ופריסה: [ops/README.md](ops/README.md)
- תיעוד אבטחה: [security/README.md](security/README.md)
- חומרה/ציוד היקפי: [hardware/README.md](hardware/README.md)
- תרומה/CI: [contributing/README.md](contributing/README.md)
- תמונות מצב של הפרויקט: [maintainers/README.md](maintainers/README.md)

## לפי קהל יעד

### משתמשים / מפעילים

- [commands-reference.md](reference/cli/commands-reference.md) — חיפוש פקודות לפי זרימת עבודה
- [providers-reference.md](reference/api/providers-reference.md) — מזהי ספקים, כינויים, משתני סביבה של אישורים
- [channels-reference.md](reference/api/channels-reference.md) — יכולות ערוצים ונתיבי הגדרה
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — הגדרת חדרים מוצפנים ב-Matrix (E2EE) ואבחון אי-תגובה
- [config-reference.md](reference/api/config-reference.md) — מפתחות הגדרה בעלי אות חזק ובררות מחדל בטוחות
- [custom-providers.md](contributing/custom-providers.md) — תבניות שילוב ספק מותאם אישית/URL בסיס
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — הגדרת Z.AI/GLM ומטריצת נקודות קצה
- [langgraph-integration.md](contributing/langgraph-integration.md) — שילוב חלופי למקרי קצה של מודל/קריאת כלי
- [operations-runbook.md](ops/operations-runbook.md) — פעולות סביבת ריצה יום-2 וזרימות שחזור
- [troubleshooting.md](ops/troubleshooting.md) — חתימות כשל נפוצות וצעדי שחזור

### תורמים / מתחזקים

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### אבטחה / אמינות

> הערה: אזור זה כולל מסמכי הצעה/מפת דרכים. להתנהגות הנוכחית, התחילו מ-[config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), ו-[troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## ניווט במערכת וממשל

- תוכן עניינים מאוחד: [SUMMARY.md](SUMMARY.md)
- מפת מבנה תיעוד (שפה/חלק/פונקציה): [structure/README.md](maintainers/structure-README.md)
- מלאי/סיווג תיעוד: [docs-inventory.md](maintainers/docs-inventory.md)
- תמונת מצב של מיון הפרויקט: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## שפות אחרות

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
