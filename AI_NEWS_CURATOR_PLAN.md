# Plan: AI News Curator — 7 Noticias IA Diarias con Multimedia

## Contexto

JhedaiClaw (fork de ZeroClaw) automatiza un pipeline diario de curación de noticias IA para el grupo CCHIA de WhatsApp. El pipeline busca noticias, genera imágenes personalizadas, crea un podcast en audio y video, y entrega todo via browser automation (computer use).

**Entregables diarios:**

- **7 mensajes individuales** en WhatsApp, cada uno con imagen personalizada + descripción breve
- **1 audio podcast** con las 7 noticias (NotebookLM, español)
- **1 video resumen** estilo documental (NotebookLM)

**Costo estimado: ~$0.01/dia (LLM) — pipeline 100% gratuito** (L-V)

---

## Pipeline (6 pasos)

### Opcion A: Perplexity + NotebookLM (recomendada)

| Paso           | Accion                                                                | Herramienta                                                   | Tiempo est. |
| -------------- | --------------------------------------------------------------------- | ------------------------------------------------------------- | ----------- |
| 1. Buscar      | Perplexity deep research: top 7 noticias IA                           | MCP `perplexity__perplexity_research`                         | ~30s        |
| 2. Notebook    | Crear notebook + agregar las 7 URLs como fuentes                      | MCP `notebooklm__notebook_create` + `notebook_add_url`        | ~20s        |
| 3. Resumir     | LLM genera 7 resumenes en espanol + consulta notebook para enriquecer | Claude + `notebooklm__notebook_query`                         | ~15s        |
| 4. Imagenes    | Generar 7 infografias personalizadas por noticia                      | MCP `notebooklm__infographic_create` (PNG, gratis)            | ~60s        |
| 5. Audio+Video | NotebookLM genera podcast + video del dia                             | `notebooklm__audio_overview_create` + `video_overview_create` | ~3-5min     |
| 6. Enviar      | Browser: 7 msgs (img+texto) + audio + video al grupo WA               | Browser tool (computer use)                                   | ~2-3min     |

**Tiempo total: ~7-10 minutos**

### Opcion B: Solo NotebookLM (todo gratis, menos control)

| Paso        | Accion                                      | Herramienta                                                          |
| ----------- | ------------------------------------------- | -------------------------------------------------------------------- |
| 1. Research | NotebookLM investiga noticias IA via Google | `notebooklm__research_start` + `research_status` + `research_import` |
| 2. Query    | Preguntar al notebook por top 7 noticias    | `notebooklm__notebook_query`                                         |
| 3. Imagenes | Generar 7 infografias por noticia           | `notebooklm__infographic_create` (PNG, gratis)                       |
| 4. Media    | Generar audio podcast + video explainer     | `audio_overview_create` + `video_overview_create`                    |
| 5. Enviar   | Browser -> WhatsApp grupo                   | Browser tool                                                         |

---

## Formato de Entrega

Cada noticia se envia como mensaje individual:

```
[Imagen personalizada adjunta]

🔬 1/7 · Google lanza Gemini 3.0 con razonamiento multimodal

Google presento Gemini 3.0, su modelo mas avanzado con capacidad
de razonamiento sobre video, codigo y datos simultaneamente.

🔗 techcrunch.com/2026/03/16/google-gemini-3
```

Despues de los 7 mensajes:

```
[Audio podcast adjunto]
🎧 Resumen en audio — 7 Noticias IA del 16 de marzo 2026

[Video adjunto]
🎬 Video resumen IA del dia
```

---

## Configuracion (config.toml)

```toml
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.4

[agent]
max_tool_iterations = 30    # Pipeline largo: busqueda + 7 imagenes + audio + 8 envios browser

[memory]
backend = "sqlite"
auto_save = true

[gateway]
port = 3000
host = "127.0.0.1"

[autonomy]
level = "supervised"
workspace_only = true
allowed_commands = ["git", "npm", "npx", "curl", "python3", "ls", "cat"]

# ── MCP Servers ──
[mcp]
enabled = true
deferred_loading = true

# Perplexity Web API (GRATIS — usa sesion de tu cuenta)
[[mcp.servers]]
name = "perplexity"
transport = "stdio"
command = "npx"
args = ["-y", "perplexity-web-api-mcp"]
tool_timeout_secs = 180

# NotebookLM (32 tools: audio, video, research, infographics)
[[mcp.servers]]
name = "notebooklm"
transport = "stdio"
command = "npx"
args = ["-y", "@m4ykeldev/notebooklm-mcp", "serve"]
tool_timeout_secs = 300

# ── WhatsApp Web (grupos) ──
[channels_config.whatsapp]
session_path = "~/.jhedaiclaw/state/whatsapp-web/session.db"
pair_phone = "56912345678"
allowed_numbers = ["*"]

# ── Browser (computer use para envio de media a WhatsApp) ──
[browser]
enabled = true

# ── Web search fallback ──
[web_search]
enabled = true

# ── Cron ──
[cron]
enabled = true
```

### Variables de Entorno (en shell, NO en TOML)

```bash
export OPENROUTER_API_KEY="sk-or-xxx"        # LLM orquestador

# Perplexity Web API (gratis — extraer cookies de perplexity.ai):
# 1. Login en perplexity.ai en Chrome
# 2. DevTools → Application → Cookies → perplexity.ai
# 3. Copiar valores de: __Secure-next-auth.session-token y next-auth.csrf-token
export PERPLEXITY_SESSION_TOKEN="tu-session-token"
export PERPLEXITY_CSRF_TOKEN="tu-csrf-token"

# NotebookLM: auth via Chrome (una sola vez)
# npx @m4ykeldev/notebooklm-mcp auth
```

### Costos por Ejecucion Diaria

| Componente       | Costo                          |
| ---------------- | ------------------------------ |
| Perplexity       | $0 (sesion web)                |
| NotebookLM       | $0 (servicio Google gratuito)  |
| Imagenes (infog) | $0 (NotebookLM infographic)    |
| Audio + Video    | $0 (NotebookLM Studio)         |
| Claude LLM       | ~$0.01 (OpenRouter)            |
| **Total**        | **~$0.01/dia — 100% gratis\*** |

\*El unico costo es el LLM orquestador. Con Ollama (local) o modelo gratuito = $0 total.

---

## MCPs Detalle

### NotebookLM MCP (`@m4ykeldev/notebooklm-mcp`)

**32 tools** — Paquete recomendado. MIT, gratis.

| Tool                    | Descripcion                               | Uso en pipeline          |
| ----------------------- | ----------------------------------------- | ------------------------ |
| `notebook_create`       | Crear notebook nuevo                      | Notebook diario          |
| `notebook_add_url`      | Agregar URL como fuente (incluye YouTube) | Agregar 7 URLs           |
| `notebook_add_text`     | Agregar texto como fuente                 | Contexto adicional       |
| `research_start`        | Investigacion autonoma (Google Search)    | Alternativa a Perplexity |
| `research_status`       | Verificar progreso de investigacion       | Monitorear busqueda      |
| `research_import`       | Importar hallazgos como fuentes           | Alimentar notebook       |
| `notebook_query`        | Preguntas con respuestas citadas          | Generar resumenes        |
| `audio_overview_create` | Podcast en 80+ idiomas                    | Audio en espanol         |
| `video_overview_create` | Video explainer (6 estilos visuales)      | Video de noticias        |
| `infographic_create`    | Infografia (horizontal/vertical)          | **Imagenes por noticia** |
| `slide_deck_create`     | Presentacion                              | Alternativa visual       |
| `studio_status`         | Estado de generacion                      | Esperar audio/video      |
| `studio_delete`         | Limpiar artefactos                        | Cleanup post-envio       |

**Descargas soportadas:** Audio (WAV), Video (MP4), Infografia (PNG)

**Estilos de video:** classroom, documentary, animated, corporate, cinematic, minimalist

**Paquetes alternativos:**

| Paquete                            | Tools | Video          | Audio | Research                |
| ---------------------------------- | ----- | -------------- | ----- | ----------------------- |
| `@m4ykeldev/notebooklm-mcp`        | 32    | Si             | Si    | Si (Deep Research)      |
| `notebooklm-mcp` (PleasePrompto)   | ~20   | No             | Si    | Si                      |
| `@roomi-fields/notebooklm-mcp`     | ~25   | Si (6 estilos) | Si    | Si                      |
| `notebooklm-mcp-secure` (Pantheon) | ~20   | No             | Si    | Si (14 capas seguridad) |

### Perplexity MCP (opciones gratuitas)

| Paquete                     | Metodo            | Cuenta          | Tools                          |
| --------------------------- | ----------------- | --------------- | ------------------------------ |
| `perplexity-web-api-mcp`    | Session cookies   | Free o Pro      | search, ask, reason, research  |
| `perplexity-mcp-zerver`     | Puppeteer browser | Free o Pro      | search, docs, APIs, chat       |
| `@perplexity-ai/mcp-server` | API oficial       | API key ($5/1K) | search, chat, research, reason |

**Recomendado:** `perplexity-web-api-mcp` — gratis, usa cookies de sesion.

---

## Warnings NotebookLM

- ⚠️ Sin API oficial. Usa browser automation interno. Puede romperse si Google cambia la UI.
- ⚠️ Usar cuenta Google dedicada (riesgo bloqueo por automation).
- ⚠️ Auth inicial: `npx @m4ykeldev/notebooklm-mcp auth` abre Chrome para login Google (una sola vez).
- ⚠️ Credenciales guardadas en `~/.notebooklm-mcp/auth.json`.
- ⚠️ 32 tools = muchos tokens en el prompt. Usar `deferred_loading = true`.
- ⚠️ Fallback audio: Edge TTS nativo (`es-CL-CatalinaNeural`, gratis).
- ⚠️ Fallback video: omitir (no hay alternativa gratuita equivalente).

**Confirmado:** el MCP de NotebookLM funciona con Claude Code. Busqueda (Google Search), audio y video estan operativos.

---

## WhatsApp — Envio de Media

### Limitaciones del canal nativo

| Modo          | Texto | Grupos               | Media (img/audio/video) |
| ------------- | ----- | -------------------- | ----------------------- |
| Cloud API     | Si    | No (solo individual) | No                      |
| Web (`wa-rs`) | Si    | Si (JIDs de grupo)   | No                      |

### Solucion: Browser Tool (Computer Use)

El browser tool de JhedaiClaw soporta: Open, Click, Fill, Type, Screenshot, Wait, Press, Hover, Scroll.

Para enviar media a WhatsApp Web:

1. `browser.open("web.whatsapp.com")` → abrir grupo
2. Click en adjuntar → seleccionar imagen/audio/video
3. Agregar caption → enviar

**Campos de config reales de WhatsApp** (verificados en `src/config/schema.rs`):

- `session_path` — ruta a session.db
- `pair_phone` — numero para QR pairing
- `allowed_numbers` — lista de numeros permitidos

**NO existen:** `mode`, `target_group`, `allowed_users` (estos eran errores del plan original)

---

## Cron — Ejecucion Programada

```bash
# Agregar tarea cron (L-V 8:00 AM Santiago)
jhedaiclaw cron add "0 8 * * 1-5" \
  --tz "America/Santiago" \
  --type agent \
  --prompt "Ejecutar AI News Curator: buscar 7 noticias IA, generar imagenes, audio podcast y video, enviar todo al grupo de WhatsApp"
```

**Nota:** `DeliveryConfig` de cron NO incluye WhatsApp. El envio se hace dentro del agent loop via browser tool, no via delivery config.

**En Windows:** No existe `service install` (es systemd/Linux). Usar Task Scheduler para iniciar el daemon.

---

## SKILL.md

Ubicacion: `~/.jhedaiclaw/workspace/skills/ai-noticias/SKILL.md`

El SKILL.md se inyecta como `<instruction>` en el system prompt del agente. No soporta YAML frontmatter. El nombre se toma del directorio.

Ver archivo completo en: `.claude/skills/jhedaiclaw/ai-noticias/SKILL.md`

---

## Componentes del Sistema

| Componente      | Tecnologia                         | Equivalente OpenClaw   | Estado                   |
| --------------- | ---------------------------------- | ---------------------- | ------------------------ |
| Runtime         | JhedaiClaw Daemon + Cron           | Heartbeat task         | OK                       |
| Busqueda        | Perplexity MCP                     | web-search skill       | OK                       |
| LLM             | Claude Sonnet 4.6 (OpenRouter)     | Claude via Anthropic   | OK                       |
| Imagenes        | NotebookLM `infographic_create`    | openai-image-gen skill | Gratis (PNG)             |
| Audio podcast   | NotebookLM `audio_overview_create` | N/A                    | Confirmado               |
| Video explainer | NotebookLM `video_overview_create` | N/A                    | Confirmado               |
| Research        | NotebookLM `research_start`        | web-search skill       | Alternativa a Perplexity |
| Infografias     | NotebookLM `infographic_create`    | N/A                    | Alternativa a DALL-E     |
| Distribucion    | Browser tool -> WhatsApp Web       | WhatsApp canal nativo  | Computer use             |
| Skill format    | SKILL.md (raw injection)           | SKILL.md (frontmatter) | Compatible               |

---

## Lecciones de OpenClaw Replicadas

### Modelo OpenClaw

OpenClaw usa **"Heartbeat tasks"** para news digests:

1. **Tier 1 (deterministico):** fetch RSS + APIs → dedup → quality scoring
2. **Tier 2 (LLM):** resumir + formatear
3. **Delivery:** envio directo via canales nativos

Sus skills usan SKILL.md con YAML frontmatter, 30-50 lineas max, instrucciones deterministicas.

### Lo que replicamos en ZeroClaw

- Skill como SKILL.md con instrucciones paso-a-paso (inyectado como `<instruction>`)
- MCP servers para Perplexity (busqueda) y NotebookLM (imagenes, audio, video)
- Cron scheduler con timezone (`job_type: agent`)
- Browser tool para WhatsApp delivery (computer use)
- Sin YAML frontmatter (ZeroClaw inyecta el .md completo como texto)
- Sin "Heartbeat" (cron con `job_type: agent` es equivalente funcional)

---

## Ecosistema ZeroClaw

### Skills y Marketplaces

| Fuente                 | Skills     | Tipo                  | Relevancia                          |
| ---------------------- | ---------- | --------------------- | ----------------------------------- |
| Open Skills (besoeasy) | 39         | SKILL.md              | Basicos: crypto, web scraping, PDFs |
| ZeroMarket             | ~5         | WASM sandboxed        | Nuevo, pocos skills                 |
| LobeHub                | 1          | Marketplace externo   | Skill operacional                   |
| Skills locales         | Ilimitados | SKILL.md en workspace | **Nuestro AI News Curator**         |

### Comparacion ZeroClaw vs OpenClaw

| Aspecto        | ZeroClaw                             | OpenClaw             |
| -------------- | ------------------------------------ | -------------------- |
| Stars          | 27K                                  | 68K                  |
| Skills propios | ~45                                  | 13,700+              |
| MCP soporte    | Completo (3 transportes)             | Completo             |
| News Curator   | **No existe — seremos los primeros** | Varios existentes    |
| Runtime        | Rust, <5MB RAM                       | TypeScript, >1GB RAM |

**Oportunidad:** Nuestro AI News Curator seria una de las primeras implementaciones completas de automatizacion multimedia para ZeroClaw.

---

## Verificacion

1. Validar config TOML contra `src/config/schema.rs`
2. Probar NotebookLM MCP: `npx @m4ykeldev/notebooklm-mcp auth`
3. Compilar: `cargo build --release --features whatsapp-web`
4. Probar browser tool: `jhedaiclaw agent -m "abre web.whatsapp.com"`
5. Probar cron: `jhedaiclaw cron add "*/5 * * * *" --tz "America/Santiago" --type agent --prompt "di hola"`
6. Test manual: `jhedaiclaw agent -m "Ejecutar AI News Curator ahora"`

---

## Fuentes

**ZeroClaw:**

- [ZeroClaw GitHub](https://github.com/zeroclaw-labs/zeroclaw) — 27K stars, Rust runtime
- [ZeroClaw MCP Integration (DeepWiki)](https://deepwiki.com/zeroclaw-labs/zeroclaw/11.7-mcp-integration)
- [Open Skills](https://github.com/besoeasy/open-skills) — 39 skills compatibles
- [ZeroMarket](https://zeromarket.vercel.app) — WASM skills marketplace

**OpenClaw (referencia):**

- [OpenClaw Skills Docs](https://docs.openclaw.ai/tools/skills)
- [OpenClaw Tech News Digest](https://openclawconsult.com/lab/openclaw-tech-news-digest)
- [OpenClaw WhatsApp Briefing](https://diamantai.substack.com/p/openclaw-tutorial-build-an-ai-agent)
- [Awesome OpenClaw Skills](https://github.com/VoltAgent/awesome-openclaw-skills)

**MCPs:**

- [NotebookLM MCP (m4ykeldev)](https://github.com/m4yk3ldev/notebooklm-mcp) — 32 tools
- [Perplexity Web API MCP](https://github.com/mishamyrt/perplexity-web-api-mcp) — gratis
- [Perplexity MCP Zerver](https://github.com/wysh3/perplexity-mcp-zerver) — gratis
- [Awesome MCP Servers](https://github.com/punkpeye/awesome-mcp-servers)
