# Centro de Documentación ZeroClaw

Esta página es el punto de entrada principal del sistema de documentación.

Última actualización: **20 de febrero de 2026**.

Centros localizados: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## Comience Aquí

| Quiero…                                                             | Leer esto                                                                      |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Instalar y ejecutar ZeroClaw rápidamente                            | [README.md (Inicio Rápido)](../README.md#quick-start)                          |
| Arranque con un solo comando                                        | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| Encontrar comandos por tarea                                        | [commands-reference.md](reference/cli/commands-reference.md)                   |
| Verificar rápidamente claves y valores predeterminados de config    | [config-reference.md](reference/api/config-reference.md)                       |
| Configurar proveedores/endpoints personalizados                     | [custom-providers.md](contributing/custom-providers.md)                        |
| Configurar el proveedor Z.AI / GLM                                  | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| Usar los patrones de integración LangGraph                          | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| Operar el runtime (runbook día-2)                                   | [operations-runbook.md](ops/operations-runbook.md)                             |
| Solucionar problemas de instalación/runtime/canal                   | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Ejecutar configuración y diagnósticos de salas cifradas Matrix      | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| Navegar la documentación por categoría                              | [SUMMARY.md](SUMMARY.md)                                                      |
| Ver la instantánea de docs de PR/issues del proyecto                | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Árbol de Decisión Rápida (10 segundos)

- ¿Necesita configuración o instalación inicial? → [setup-guides/README.md](setup-guides/README.md)
- ¿Necesita claves exactas de CLI/configuración? → [reference/README.md](reference/README.md)
- ¿Necesita operaciones de producción/servicio? → [ops/README.md](ops/README.md)
- ¿Ve fallos o regresiones? → [troubleshooting.md](ops/troubleshooting.md)
- ¿Trabaja en endurecimiento de seguridad o hoja de ruta? → [security/README.md](security/README.md)
- ¿Trabaja con placas/periféricos? → [hardware/README.md](hardware/README.md)
- ¿Contribución/revisión/flujo de trabajo CI? → [contributing/README.md](contributing/README.md)
- ¿Quiere el mapa completo? → [SUMMARY.md](SUMMARY.md)

## Colecciones (Recomendadas)

- Inicio: [setup-guides/README.md](setup-guides/README.md)
- Catálogos de referencia: [reference/README.md](reference/README.md)
- Operaciones y despliegue: [ops/README.md](ops/README.md)
- Documentación de seguridad: [security/README.md](security/README.md)
- Hardware/periféricos: [hardware/README.md](hardware/README.md)
- Contribución/CI: [contributing/README.md](contributing/README.md)
- Instantáneas del proyecto: [maintainers/README.md](maintainers/README.md)

## Por Audiencia

### Usuarios / Operadores

- [commands-reference.md](reference/cli/commands-reference.md) — búsqueda de comandos por flujo de trabajo
- [providers-reference.md](reference/api/providers-reference.md) — IDs de proveedores, alias, variables de entorno de credenciales
- [channels-reference.md](reference/api/channels-reference.md) — capacidades de canales y rutas de configuración
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — configuración de salas cifradas Matrix (E2EE) y diagnósticos de no-respuesta
- [config-reference.md](reference/api/config-reference.md) — claves de configuración de alta señalización y valores predeterminados seguros
- [custom-providers.md](contributing/custom-providers.md) — patrones de integración de proveedor personalizado/URL base
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — configuración Z.AI/GLM y matriz de endpoints
- [langgraph-integration.md](contributing/langgraph-integration.md) — integración de respaldo para casos límite de modelo/llamada de herramienta
- [operations-runbook.md](ops/operations-runbook.md) — operaciones runtime día-2 y flujos de rollback
- [troubleshooting.md](ops/troubleshooting.md) — firmas de fallo comunes y pasos de recuperación

### Contribuidores / Mantenedores

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Seguridad / Fiabilidad

> Nota: esta zona incluye documentos de propuesta/hoja de ruta. Para el comportamiento actual, comience por [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), y [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## Navegación del Sistema y Gobernanza

- Tabla de contenidos unificada: [SUMMARY.md](SUMMARY.md)
- Mapa de estructura de docs (idioma/sección/función): [structure/README.md](maintainers/structure-README.md)
- Inventario/clasificación de la documentación: [docs-inventory.md](maintainers/docs-inventory.md)
- Instantánea de triaje del proyecto: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## Otros idiomas

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
