# JhedaiClaw दस्तावेज़ीकरण केंद्र

यह पृष्ठ दस्तावेज़ीकरण प्रणाली का प्राथमिक प्रवेश बिंदु है।

अंतिम अपडेट: **20 फरवरी 2026**।

स्थानीयकृत केंद्र: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## यहाँ से शुरू करें

| मैं चाहता/चाहती हूँ…                                       | यह पढ़ें                                                                                   |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| JhedaiClaw को जल्दी से इंस्टॉल और चलाना                    | [README.md (त्वरित प्रारंभ)](../README.md#quick-start)                                     |
| एक कमांड में बूटस्ट्रैप                                    | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                              |
| कार्य के अनुसार कमांड खोजना                                | [commands-reference.md](reference/cli/commands-reference.md)                               |
| कॉन्फ़िग कुंजियों और डिफ़ॉल्ट मानों को जल्दी जाँचना        | [config-reference.md](reference/api/config-reference.md)                                   |
| कस्टम प्रदाता/एंडपॉइंट कॉन्फ़िगर करना                      | [custom-providers.md](contributing/custom-providers.md)                                    |
| Z.AI / GLM प्रदाता कॉन्फ़िगर करना                          | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                                          |
| LangGraph एकीकरण पैटर्न का उपयोग करना                      | [langgraph-integration.md](contributing/langgraph-integration.md)                          |
| रनटाइम संचालित करना (दिन-2 रनबुक)                          | [operations-runbook.md](ops/operations-runbook.md)                                         |
| इंस्टॉलेशन/रनटाइम/चैनल समस्याओं का निवारण                  | [troubleshooting.md](ops/troubleshooting.md)                                               |
| Matrix एन्क्रिप्टेड कमरों का सेटअप और डायग्नोस्टिक्स चलाना | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                                      |
| श्रेणी के अनुसार दस्तावेज़ ब्राउज़ करना                    | [SUMMARY.md](SUMMARY.md)                                                                   |
| प्रोजेक्ट PR/issues दस्तावेज़ स्नैपशॉट देखना               | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## त्वरित निर्णय वृक्ष (10 सेकंड)

- प्रारंभिक सेटअप या इंस्टॉलेशन चाहिए? → [setup-guides/README.md](setup-guides/README.md)
- सटीक CLI/कॉन्फ़िग कुंजियाँ चाहिए? → [reference/README.md](reference/README.md)
- प्रोडक्शन/सर्विस ऑपरेशन चाहिए? → [ops/README.md](ops/README.md)
- विफलताएँ या रिग्रेशन दिख रहे हैं? → [troubleshooting.md](ops/troubleshooting.md)
- सुरक्षा सख्ती या रोडमैप पर काम कर रहे हैं? → [security/README.md](security/README.md)
- बोर्ड/पेरिफेरल्स के साथ काम कर रहे हैं? → [hardware/README.md](hardware/README.md)
- योगदान/समीक्षा/CI वर्कफ़्लो? → [contributing/README.md](contributing/README.md)
- पूरा नक्शा चाहिए? → [SUMMARY.md](SUMMARY.md)

## संग्रह (अनुशंसित)

- प्रारंभ: [setup-guides/README.md](setup-guides/README.md)
- संदर्भ सूचियाँ: [reference/README.md](reference/README.md)
- संचालन और तैनाती: [ops/README.md](ops/README.md)
- सुरक्षा दस्तावेज़: [security/README.md](security/README.md)
- हार्डवेयर/पेरिफेरल्स: [hardware/README.md](hardware/README.md)
- योगदान/CI: [contributing/README.md](contributing/README.md)
- प्रोजेक्ट स्नैपशॉट: [maintainers/README.md](maintainers/README.md)

## दर्शक वर्ग के अनुसार

### उपयोगकर्ता / ऑपरेटर

- [commands-reference.md](reference/cli/commands-reference.md) — वर्कफ़्लो के अनुसार कमांड खोज
- [providers-reference.md](reference/api/providers-reference.md) — प्रदाता ID, उपनाम, क्रेडेंशियल पर्यावरण चर
- [channels-reference.md](reference/api/channels-reference.md) — चैनल क्षमताएँ और कॉन्फ़िगरेशन पथ
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix एन्क्रिप्टेड कमरा (E2EE) सेटअप और गैर-प्रतिक्रिया डायग्नोस्टिक्स
- [config-reference.md](reference/api/config-reference.md) — उच्च-संकेत कॉन्फ़िग कुंजियाँ और सुरक्षित डिफ़ॉल्ट
- [custom-providers.md](contributing/custom-providers.md) — कस्टम प्रदाता/बेस URL एकीकरण पैटर्न
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM सेटअप और एंडपॉइंट मैट्रिक्स
- [langgraph-integration.md](contributing/langgraph-integration.md) — मॉडल/टूल-कॉल एज केस के लिए फ़ॉलबैक एकीकरण
- [operations-runbook.md](ops/operations-runbook.md) — रनटाइम दिन-2 ऑपरेशन और रोलबैक फ़्लो
- [troubleshooting.md](ops/troubleshooting.md) — सामान्य विफलता हस्ताक्षर और पुनर्प्राप्ति चरण

### योगदानकर्ता / अनुरक्षक

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### सुरक्षा / विश्वसनीयता

> नोट: इस क्षेत्र में प्रस्ताव/रोडमैप दस्तावेज़ शामिल हैं। वर्तमान व्यवहार के लिए, [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), और [troubleshooting.md](ops/troubleshooting.md) से शुरू करें।

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## सिस्टम नेविगेशन और शासन

- एकीकृत विषय सूची: [SUMMARY.md](SUMMARY.md)
- दस्तावेज़ संरचना नक्शा (भाषा/भाग/कार्य): [structure/README.md](maintainers/structure-README.md)
- दस्तावेज़ीकरण सूची/वर्गीकरण: [docs-inventory.md](maintainers/docs-inventory.md)
- प्रोजेक्ट ट्राइएज स्नैपशॉट: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## अन्य भाषाएँ

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
