# ศูนย์กลางเอกสาร ZeroClaw

หน้านี้เป็นจุดเริ่มต้นหลักของระบบเอกสาร

อัปเดตล่าสุด: **21 กุมภาพันธ์ 2026**

ศูนย์กลางภาษาต่าง ๆ: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## เริ่มต้นที่นี่

| ฉันต้องการ…                                                          | อ่านสิ่งนี้                                                                    |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| ติดตั้งและรัน ZeroClaw อย่างรวดเร็ว                                    | [README.md (เริ่มต้นอย่างรวดเร็ว)](../README.md#quick-start)                    |
| ติดตั้งด้วยคำสั่งเดียว                                                | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| ค้นหาคำสั่งตามงาน                                                    | [commands-reference.md](reference/cli/commands-reference.md)                   |
| ตรวจสอบคีย์และค่าเริ่มต้นของการตั้งค่าอย่างรวดเร็ว                     | [config-reference.md](reference/api/config-reference.md)                       |
| ตั้งค่าผู้ให้บริการ/endpoint แบบกำหนดเอง                               | [custom-providers.md](contributing/custom-providers.md)                         |
| ตั้งค่าผู้ให้บริการ Z.AI / GLM                                        | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| ใช้รูปแบบการรวม LangGraph                                            | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| ดำเนินงาน runtime (คู่มือปฏิบัติการวันที่ 2)                          | [operations-runbook.md](ops/operations-runbook.md)                             |
| แก้ไขปัญหาการติดตั้ง/runtime/ช่องทาง                                  | [troubleshooting.md](ops/troubleshooting.md)                                   |
| รันการตั้งค่าและวินิจฉัยห้อง Matrix แบบเข้ารหัส                        | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| เรียกดูเอกสารตามหมวดหมู่                                              | [SUMMARY.md](SUMMARY.md)                                                       |
| ดูสแนปช็อตเอกสาร PR/issue ของโปรเจกต์                                 | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## แผนผังการตัดสินใจอย่างรวดเร็ว (10 วินาที)

- ต้องการการตั้งค่าหรือการติดตั้งเบื้องต้น? → [setup-guides/README.md](setup-guides/README.md)
- ต้องการคีย์ CLI/config ที่แน่นอน? → [reference/README.md](reference/README.md)
- ต้องการการดำเนินงานระดับโปรดักชัน/เซอร์วิส? → [ops/README.md](ops/README.md)
- พบความล้มเหลวหรือการถดถอย? → [troubleshooting.md](ops/troubleshooting.md)
- ทำงานเกี่ยวกับการเสริมความปลอดภัยหรือแผนงาน? → [security/README.md](security/README.md)
- ทำงานกับบอร์ด/อุปกรณ์ต่อพ่วง? → [hardware/README.md](hardware/README.md)
- การมีส่วนร่วม/รีวิว/เวิร์กโฟลว์ CI? → [contributing/README.md](contributing/README.md)
- ต้องการแผนที่ทั้งหมด? → [SUMMARY.md](SUMMARY.md)

## คอลเลกชัน (แนะนำ)

- เริ่มต้น: [setup-guides/README.md](setup-guides/README.md)
- แคตตาล็อกอ้างอิง: [reference/README.md](reference/README.md)
- การดำเนินงานและการปรับใช้: [ops/README.md](ops/README.md)
- เอกสารความปลอดภัย: [security/README.md](security/README.md)
- ฮาร์ดแวร์/อุปกรณ์ต่อพ่วง: [hardware/README.md](hardware/README.md)
- การมีส่วนร่วม/CI: [contributing/README.md](contributing/README.md)
- สแนปช็อตโปรเจกต์: [maintainers/README.md](maintainers/README.md)

## ตามกลุ่มผู้ใช้

### ผู้ใช้ / ผู้ดำเนินงาน

- [commands-reference.md](reference/cli/commands-reference.md) — ค้นหาคำสั่งตามเวิร์กโฟลว์
- [providers-reference.md](reference/api/providers-reference.md) — ID ผู้ให้บริการ, นามแฝง, ตัวแปรสภาพแวดล้อมข้อมูลรับรอง
- [channels-reference.md](reference/api/channels-reference.md) — ความสามารถของช่องทางและเส้นทางการตั้งค่า
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — การตั้งค่าห้อง Matrix แบบเข้ารหัส (E2EE) และการวินิจฉัยการไม่ตอบสนอง
- [config-reference.md](reference/api/config-reference.md) — คีย์การตั้งค่าที่สำคัญและค่าเริ่มต้นที่ปลอดภัย
- [custom-providers.md](contributing/custom-providers.md) — รูปแบบการรวมผู้ให้บริการแบบกำหนดเอง/URL ฐาน
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — การตั้งค่า Z.AI/GLM และเมทริกซ์ endpoint
- [langgraph-integration.md](contributing/langgraph-integration.md) — การรวมแบบ fallback สำหรับกรณีพิเศษของโมเดล/การเรียกเครื่องมือ
- [operations-runbook.md](ops/operations-runbook.md) — การดำเนินงาน runtime วันที่ 2 และโฟลว์การย้อนกลับ
- [troubleshooting.md](ops/troubleshooting.md) — ลายเซ็นความล้มเหลวทั่วไปและขั้นตอนการกู้คืน

### ผู้มีส่วนร่วม / ผู้ดูแล

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### ความปลอดภัย / ความน่าเชื่อถือ

> หมายเหตุ: ส่วนนี้รวมเอกสารข้อเสนอ/แผนงาน สำหรับพฤติกรรมปัจจุบัน เริ่มต้นที่ [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) และ [troubleshooting.md](ops/troubleshooting.md)

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## การนำทางระบบและการกำกับดูแล

- สารบัญรวม: [SUMMARY.md](SUMMARY.md)
- แผนที่โครงสร้างเอกสาร (ภาษา/ส่วน/ฟังก์ชัน): [structure/README.md](maintainers/structure-README.md)
- รายการ/การจำแนกเอกสาร: [docs-inventory.md](maintainers/docs-inventory.md)
- สแนปช็อตการคัดกรองโปรเจกต์: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## ภาษาอื่น ๆ

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
