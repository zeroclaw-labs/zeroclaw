# Plan: Crear Repo Privado + Implementar Memory OS en JhedaiClaw

## Contexto

JhedaiClaw (fork de ZeroClaw) ya está compilado y funcionando en `c:/Users/Lenovo/.gemini/antigravity/scratch/zeroclaw/`. Edison quiere: (1) subir el fork a un repo privado en GitHub, y (2) implementar Memory OS — un backend de memoria basado en grafos de conocimiento con CozoDB que funciona como investigador autónomo cognitivo.

---

## Parte 1: Crear Repo Privado en GitHub

1. `gh repo create edisonvasquezd/jhedaiclaw --private --source=. --push`
2. Verificar con `gh repo view`

---

## Parte 2: Implementar Memory OS

### Interfaces existentes (verificadas en el código)

**Memory trait** (`src/memory/traits.rs`):

```rust
trait Memory: Send + Sync {
    fn name(&self) -> &str;
    async fn store(&self, key: &str, content: &str, category: MemoryCategory, session_id: Option<&str>) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize, session_id: Option<&str>) -> Result<Vec<MemoryEntry>>;
    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>>;
    async fn list(&self, category: Option<&MemoryCategory>, session_id: Option<&str>) -> Result<Vec<MemoryEntry>>;
    async fn forget(&self, key: &str) -> Result<bool>;
    async fn count(&self) -> Result<usize>;
    async fn health_check(&self) -> bool;
}
```

**MemoryEntry**: `{ id, key, content, category: MemoryCategory, timestamp, session_id, score }`
**MemoryCategory**: `Core | Daily | Conversation | Custom(String)`

**Tool trait** (`src/tools/traits.rs`):

```rust
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult>;
}
```

**ToolResult**: `{ success: bool, output: String, error: Option<String> }`

**Tool registration**: Push `Arc<dyn Tool>` en `all_tools_with_runtime()` en `src/tools/mod.rs`
**Backend factory**: Match en `create_memory_with_builders()` en `src/memory/mod.rs`
**Gateway routes**: Chain `.route()` en `Router::new()` en `src/gateway/mod.rs` antes de `.with_state(state)`
**Daemon components**: `spawn_component_supervisor()` en `src/daemon/mod.rs`
**Cost tracking**: `AppState.cost_tracker.get_summary().daily_cost_usd` / `.total_tokens`

### Archivos a crear (18 nuevos)

```
src/memory/graph/
├── mod.rs              # GraphMemoryBackend impl Memory
├── config.rs           # GraphConfig struct
├── schema.rs           # CozoDB Datalog schema (8 nodos, 15 relaciones, 3 HNSW indexes)
├── retriever.rs        # Smart Recall: entity extraction + 1 Datalog query
├── extractor.rs        # Heurístico (graph lookup) + regex para entidades nuevas
├── heat.rs             # Lazy decay: heat * e^(-lambda * days)
├── emotion.rs          # VAD emocional por regex español
├── synthesizer.rs      # Event-driven: profundo (0 tok) + REM (budget-gated)
├── researcher.rs       # Research cascade (budget-gated, depth-limited)
└── budget.rs           # Token budget controller (sync con CostTracker)

src/tools/
├── graph_query.rs      # Tool: ejecutar Datalog queries
├── graph_add_concept.rs # Tool: crear concepto en el grafo
├── graph_hypothesis.rs  # Tool: crear hipótesis
├── graph_validate.rs    # Tool: validar/refutar hipótesis
├── graph_connect.rs     # Tool: crear relación epistémica
├── graph_hot_nodes.rs   # Tool: listar nodos calientes
└── graph_search.rs      # Tool: búsqueda semántica HNSW

src/gateway/
└── api_graph.rs        # REST endpoints /api/graph/*
```

### Archivos existentes a modificar (5)

| Archivo                | Cambio                                                                              |
| ---------------------- | ----------------------------------------------------------------------------------- |
| `Cargo.toml`           | Agregar `cozo = { version = "0.7", features = ["storage-sqlite", "graph-algo"] }`   |
| `src/config/schema.rs` | Agregar `GraphConfig` struct + campo `graph: Option<GraphConfig>` en `MemoryConfig` |
| `src/memory/mod.rs`    | Agregar `pub mod graph;` + match arm `"graph"` en factory con fallback a sqlite     |
| `src/tools/mod.rs`     | Agregar 7 `pub mod graph_*;` + push de tools cuando `backend == "graph"`            |
| `src/gateway/mod.rs`   | Agregar rutas `/api/graph/*` al Router                                              |

### Orden de implementación (5 fases)

**Fase 1 — Core (compila y funciona como Memory backend básico)**

1. `Cargo.toml` — agregar dependencia `cozo`
2. `src/memory/graph/config.rs` — GraphConfig struct con defaults
3. `src/config/schema.rs` — agregar `graph: Option<GraphConfig>` a MemoryConfig
4. `src/memory/graph/schema.rs` — schema CozoDB completo (8 nodos, 15 relaciones, 3 HNSW)
5. `src/memory/graph/heat.rs` — lazy decay + reactivation
6. `src/memory/graph/emotion.rs` — VAD regex español
7. `src/memory/graph/extractor.rs` — heurístico (graph lookup + regex)
8. `src/memory/graph/retriever.rs` — smart_recall con 1 Datalog query + hot nodes
9. `src/memory/graph/budget.rs` — BudgetController
10. `src/memory/graph/mod.rs` — GraphMemoryBackend implementando los 8 métodos del Memory trait
11. `src/memory/mod.rs` — registrar backend "graph" con fallback automático

**Checkpoint**: `cargo build --release` + cambiar config a `backend = "graph"` + `jhedaiclaw agent -m "test"`

**Fase 2 — Graph Tools (7 tools para que el LLM manipule el grafo)** 12. `src/tools/graph_query.rs` 13. `src/tools/graph_add_concept.rs` 14. `src/tools/graph_hypothesis.rs` 15. `src/tools/graph_validate.rs` 16. `src/tools/graph_connect.rs` 17. `src/tools/graph_hot_nodes.rs` 18. `src/tools/graph_search.rs` 19. `src/tools/mod.rs` — registrar los 7 tools

**Checkpoint**: compilar + verificar que `jhedaiclaw agent` muestra los graph tools disponibles

**Fase 3 — Synthesizer (investigador autónomo)** 20. `src/memory/graph/synthesizer.rs` — profundo (HNSW + Louvain, 0 tokens) + REM (LLM, budget-gated) 21. `src/memory/graph/researcher.rs` — cascada de investigación (depth-limited) 22. `src/daemon/mod.rs` — registrar synthesizer como componente supervisado

**Checkpoint**: activar daemon + verificar que synthesizer corre en heartbeat

**Fase 4 — Gateway API (endpoints para UI)** 23. `src/gateway/api_graph.rs` — 10 endpoints REST 24. `src/gateway/mod.rs` — montar rutas

**Checkpoint**: `jhedaiclaw gateway` + curl endpoints

**Fase 5 — Verificación final** 25. Compilar release completo 26. Test E2E: `jhedaiclaw agent` con backend graph, store/recall/forget 27. Test tools: verificar que el LLM puede usar graph_add_concept, graph_query, etc. 28. Test fallback: borrar knowledge.db, verificar que cae a sqlite 29. Grep residual por "zeroclaw" (por si acaso)

### Notas técnicas clave

- **CozoDB usa Datalog**, no SQL. Sintaxis: `?[campo] := *tabla{campo, filtro}` para queries, `:put tabla { key => campos }` para inserts
- **store() es bifásico**: fase 1 síncrona (guardar conversación raw), fase 2 async spawn (extraer entidades, crear relaciones, calcular heat, análisis emocional)
- **recall() es 1 query Datalog**: extraer entidades del query heurísticamente → navegar grafo 2-hop desde ellas → combinar con hot nodes → devolver como Vec<MemoryEntry>
- **Fallback automático**: si `GraphMemoryBackend::new()` falla, crear `SqliteMemory` y log warning
- **Tools reciben estado via constructor**: `Arc<dyn Memory>` + `Arc<SecurityPolicy>` — para tools que necesiten acceso directo a CozoDB, el `GraphMemoryBackend` expone `pub fn db(&self) -> &Arc<DbInstance>`
- **Synthesizer como daemon component**: registrar con `spawn_component_supervisor("synthesizer", ...)` gated en `config.memory.graph.synthesis_enabled`
- **Budget controller** sincroniza con `CostTracker` existente via `get_summary().daily_cost_usd`
- **Embedding dimension**: usar 384 para CozoDB HNSW (compatible con modelos small), configurable en GraphConfig
