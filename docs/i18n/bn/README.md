# ZeroClaw ডকুমেন্টেশন হাব

এই পৃষ্ঠাটি ডকুমেন্টেশন সিস্টেমের প্রধান প্রবেশ বিন্দু।

সর্বশেষ আপডেট: **২০ ফেব্রুয়ারি ২০২৬**।

স্থানীয়কৃত হাব: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## এখান থেকে শুরু করুন

| আমি চাই…                                                            | এটি পড়ুন                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| দ্রুত ZeroClaw ইনস্টল ও চালু করতে                                   | [README.md (দ্রুত শুরু)](../README.md#quick-start)                             |
| এক-ক্লিকে বুটস্ট্র্যাপ করতে                                        | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| কাজ অনুযায়ী কমান্ড খুঁজতে                                          | [commands-reference.md](reference/cli/commands-reference.md)                   |
| দ্রুত কনফিগ কী ও ডিফল্ট মান যাচাই করতে                             | [config-reference.md](reference/api/config-reference.md)                       |
| কাস্টম প্রোভাইডার/এন্ডপয়েন্ট সেটআপ করতে                           | [custom-providers.md](contributing/custom-providers.md)                         |
| Z.AI / GLM প্রোভাইডার সেটআপ করতে                                    | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| LangGraph ইন্টিগ্রেশন প্যাটার্ন ব্যবহার করতে                       | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| রানটাইম পরিচালনা করতে (দৈনন্দিন অপারেশন গাইড)                      | [operations-runbook.md](ops/operations-runbook.md)                             |
| ইনস্টলেশন/রানটাইম/চ্যানেল সমস্যা সমাধান করতে                       | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Matrix এনক্রিপ্টেড রুম সেটআপ ও ডায়াগনস্টিক চালাতে                 | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| বিভাগ অনুযায়ী ডকুমেন্টেশন ব্রাউজ করতে                              | [SUMMARY.md](SUMMARY.md)                                                       |
| প্রকল্পের PR/ইস্যু ডক স্ন্যাপশট দেখতে                              | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## দ্রুত সিদ্ধান্ত গাছ (১০ সেকেন্ড)

- সেটআপ বা প্রাথমিক ইনস্টলেশন দরকার? → [setup-guides/README.md](setup-guides/README.md)
- সুনির্দিষ্ট CLI/কনফিগ কী দরকার? → [reference/README.md](reference/README.md)
- প্রোডাকশন/সার্ভিস অপারেশন দরকার? → [ops/README.md](ops/README.md)
- ব্যর্থতা বা রিগ্রেশন দেখছেন? → [troubleshooting.md](ops/troubleshooting.md)
- নিরাপত্তা শক্তিশালীকরণ বা রোডম্যাপে কাজ করছেন? → [security/README.md](security/README.md)
- বোর্ড/পেরিফেরাল নিয়ে কাজ করছেন? → [hardware/README.md](hardware/README.md)
- অবদান/রিভিউ/CI ওয়ার্কফ্লো? → [contributing/README.md](contributing/README.md)
- সম্পূর্ণ মানচিত্র চান? → [SUMMARY.md](SUMMARY.md)

## সংগ্রহ (প্রস্তাবিত)

- শুরু করুন: [setup-guides/README.md](setup-guides/README.md)
- রেফারেন্স ক্যাটালগ: [reference/README.md](reference/README.md)
- অপারেশন ও ডিপ্লয়মেন্ট: [ops/README.md](ops/README.md)
- নিরাপত্তা ডকুমেন্টেশন: [security/README.md](security/README.md)
- হার্ডওয়্যার/পেরিফেরাল: [hardware/README.md](hardware/README.md)
- অবদান/CI: [contributing/README.md](contributing/README.md)
- প্রকল্প স্ন্যাপশট: [maintainers/README.md](maintainers/README.md)

## দর্শক অনুযায়ী

### ব্যবহারকারী / অপারেটর

- [commands-reference.md](reference/cli/commands-reference.md) — ওয়ার্কফ্লো অনুযায়ী কমান্ড খোঁজা
- [providers-reference.md](reference/api/providers-reference.md) — প্রোভাইডার আইডি, উপনাম, ক্রেডেনশিয়াল এনভায়রনমেন্ট ভেরিয়েবল
- [channels-reference.md](reference/api/channels-reference.md) — চ্যানেল সক্ষমতা ও কনফিগারেশন পাথ
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix এনক্রিপ্টেড রুম (E2EE) সেটআপ ও সাড়া না দেওয়ার ডায়াগনস্টিক
- [config-reference.md](reference/api/config-reference.md) — উচ্চ-গুরুত্বপূর্ণ কনফিগ কী ও নিরাপদ ডিফল্ট
- [custom-providers.md](contributing/custom-providers.md) — কাস্টম প্রোভাইডার/বেস URL ইন্টিগ্রেশন প্যাটার্ন
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM সেটআপ ও এন্ডপয়েন্ট ম্যাট্রিক্স
- [langgraph-integration.md](contributing/langgraph-integration.md) — মডেল/টুল-কল এজ কেসের জন্য ফলব্যাক ইন্টিগ্রেশন
- [operations-runbook.md](ops/operations-runbook.md) — দৈনন্দিন রানটাইম অপারেশন ও রোলব্যাক ফ্লো
- [troubleshooting.md](ops/troubleshooting.md) — সাধারণ ব্যর্থতার স্বাক্ষর ও পুনরুদ্ধার পদক্ষেপ

### অবদানকারী / রক্ষণাবেক্ষণকারী

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### নিরাপত্তা / নির্ভরযোগ্যতা

> দ্রষ্টব্য: এই বিভাগে প্রস্তাবনা/রোডম্যাপ ডকুমেন্ট রয়েছে। বর্তমান আচরণের জন্য [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), এবং [troubleshooting.md](ops/troubleshooting.md) দিয়ে শুরু করুন।

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## সিস্টেম নেভিগেশন ও গভর্ন্যান্স

- একীভূত সূচিপত্র: [SUMMARY.md](SUMMARY.md)
- ডক কাঠামো মানচিত্র (ভাষা/অংশ/ফাংশন): [structure/README.md](maintainers/structure-README.md)
- ডকুমেন্টেশন তালিকা/শ্রেণীবিভাগ: [docs-inventory.md](maintainers/docs-inventory.md)
- প্রকল্প ট্রায়াজ স্ন্যাপশট: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## অন্যান্য ভাষা

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
