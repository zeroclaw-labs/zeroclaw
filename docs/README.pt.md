# Centro de Documentação ZeroClaw

Esta página é o ponto de entrada principal do sistema de documentação.

Última atualização: **20 de fevereiro de 2026**.

Centros localizados: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Comece Aqui

| Eu quero…                                                           | Leia isto                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Instalar e executar o ZeroClaw rapidamente                          | [README.md (Início Rápido)](../README.md#quick-start)                          |
| Bootstrap com um único comando                                      | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                   |
| Encontrar comandos por tarefa                                       | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Verificar rapidamente chaves de configuração e valores padrão       | [config-reference.md](reference/api/config-reference.md)                       |
| Configurar provedores/endpoints personalizados                      | [custom-providers.md](contributing/custom-providers.md)                         |
| Configurar o provedor Z.AI / GLM                                    | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                              |
| Usar padrões de integração LangGraph                                | [langgraph-integration.md](contributing/langgraph-integration.md)               |
| Operar o runtime (runbook dia-2)                                    | [operations-runbook.md](ops/operations-runbook.md)                             |
| Resolver problemas de instalação/runtime/canal                      | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Configurar e diagnosticar salas criptografadas Matrix               | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                           |
| Navegar na documentação por categoria                               | [SUMMARY.md](SUMMARY.md)                                                       |
| Ver instantâneo de docs de PRs/issues do projeto                    | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Árvore de Decisão Rápida (10 segundos)

- Precisa de instalação ou configuração inicial? → [setup-guides/README.md](setup-guides/README.md)
- Precisa de chaves CLI/configuração exatas? → [reference/README.md](reference/README.md)
- Precisa de operações de produção/serviço? → [ops/README.md](ops/README.md)
- Vê falhas ou regressões? → [troubleshooting.md](ops/troubleshooting.md)
- Trabalhando em endurecimento de segurança ou roadmap? → [security/README.md](security/README.md)
- Trabalhando com placas/periféricos? → [hardware/README.md](hardware/README.md)
- Contribuição/revisão/workflow CI? → [contributing/README.md](contributing/README.md)
- Quer o mapa completo? → [SUMMARY.md](SUMMARY.md)

## Coleções (Recomendadas)

- Primeiros passos: [setup-guides/README.md](setup-guides/README.md)
- Catálogos de referência: [reference/README.md](reference/README.md)
- Operações e implantação: [ops/README.md](ops/README.md)
- Documentação de segurança: [security/README.md](security/README.md)
- Hardware/periféricos: [hardware/README.md](hardware/README.md)
- Contribuição/CI: [contributing/README.md](contributing/README.md)
- Instantâneos do projeto: [maintainers/README.md](maintainers/README.md)

## Por Público

### Usuários / Operadores

- [commands-reference.md](reference/cli/commands-reference.md) — busca de comandos por workflow
- [providers-reference.md](reference/api/providers-reference.md) — IDs de provedores, aliases, variáveis de ambiente de credenciais
- [channels-reference.md](reference/api/channels-reference.md) — capacidades dos canais e caminhos de configuração
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — configuração de salas criptografadas Matrix (E2EE) e diagnóstico de não resposta
- [config-reference.md](reference/api/config-reference.md) — chaves de configuração de alto sinal e valores padrão seguros
- [custom-providers.md](contributing/custom-providers.md) — padrões de integração de provedor personalizado/URL base
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — configuração Z.AI/GLM e matriz de endpoints
- [langgraph-integration.md](contributing/langgraph-integration.md) — integração de fallback para casos extremos de modelo/chamada de ferramenta
- [operations-runbook.md](ops/operations-runbook.md) — operações runtime dia-2 e fluxos de rollback
- [troubleshooting.md](ops/troubleshooting.md) — assinaturas de falha comuns e etapas de recuperação

### Contribuidores / Mantenedores

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Segurança / Confiabilidade

> Nota: esta seção inclui documentos de proposta/roadmap. Para o comportamento atual, comece com [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md) e [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navegação do Sistema e Governança

- Índice unificado: [SUMMARY.md](SUMMARY.md)
- Mapa da estrutura de docs (idioma/parte/função): [structure/README.md](maintainers/structure-README.md)
- Inventário/classificação da documentação: [docs-inventory.md](maintainers/docs-inventory.md)
- Instantâneo de triagem do projeto: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Outros idiomas

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
