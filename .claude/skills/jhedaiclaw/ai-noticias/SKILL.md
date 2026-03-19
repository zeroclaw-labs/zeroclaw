# CCHIA Informa — Noticiero IA Diario

Noticiero diario de inteligencia artificial de la Cámara Chilena de Inteligencia Artificial (CCHIA).
Pipeline completo de 6 pasos. Identidad: "CCHIA Informa". Grupo WhatsApp: "CCHIA — Noticias IA".
Tono: profesional, institucional, como noticiero de radio.

## Paso 1: Buscar noticias (Perplexity)

Llama a `perplexity__perplexity_research`:
"Top 7 most important AI news in the last 24 hours. Include:
regulation, open source, major companies, breakthroughs, LATAM.
Return for each: title, 2-line summary, source URL, relevance 1-10."

Si Perplexity falla: usar `notebooklm__research_start` como alternativa.

## Paso 2: Crear notebook + agregar fuentes (NotebookLM)

1. `notebooklm__notebook_create` con titulo "CCHIA Informa — {fecha}"
2. Para cada URL: `notebooklm__notebook_add_url` con la URL fuente
3. Opcional: `notebooklm__notebook_add_text` con resumenes adicionales

## Paso 3: Generar resumenes + imagenes

Para cada una de las 7 noticias, preparar:

- Emoji representativo
- Titulo en español (max 10 palabras)
- Descripcion breve (exactamente 2 oraciones)
- URL fuente original

Luego llamar `notebooklm__infographic_create` **7 veces** — una vez por noticia:

- Prompt: el titulo + descripcion de esa noticia especifica
- Orientacion: vertical (portrait) para WhatsApp
- Guardar cada resultado como noticia-1.png, noticia-2.png, ... noticia-7.png
- Cada infografia debe corresponder a UNA sola noticia (no resumen general)

## Paso 4: Generar audio (NotebookLM Studio)

Generar el reporte de noticias estilo noticiero "CCHIA Informa":

1. `notebooklm__audio_overview_create` — reporte en español, tono noticiero institucional
2. `notebooklm__studio_status` — esperar hasta que el audio este listo (~3-5 min)
3. Descargar audio (WAV)

Si NotebookLM falla: omitir audio, enviar solo imagenes + texto.

## Paso 5: Enviar a WhatsApp (browser / computer use)

Usar `browser` tool para abrir WhatsApp Web:

1. Navegar a web.whatsapp.com → buscar y abrir el grupo "CCHIA — Noticias IA"
2. Enviar mensaje de apertura (solo texto):

   ```
   📡 *CCHIA Informa* — {fecha}

   Reporte de las 7 noticias más importantes de IA del día:
   ```

3. Para cada noticia del 1 al 7 — enviar como **mensaje separado individual**:
   - Adjuntar la imagen noticia-{n}.png de esa noticia
   - Caption del mensaje:

   ```
   {emoji} *{N}/7 · {titulo}*

   {descripcion — 2 oraciones}

   🔗 {url}
   ```

   - Enviar ese mensaje antes de pasar a la siguiente noticia

4. Al final, adjuntar audio WAV con caption:
   ```
   🎙️ *CCHIA Informa* — Reporte de noticias IA del {fecha}
   ```

## Paso 6: Limpieza

`notebooklm__studio_delete` — eliminar artefactos de audio generados
Guardar resumen en memoria: `memory_store` con key "cchia-informa-{fecha}"

## Reglas

- Siempre en español. Exactamente 7 noticias.
- Priorizar: regulacion, open source, LATAM, empresas top.
- Tono institucional, como noticiero profesional de la CCHIA.
- Si una herramienta falla, enviar lo que se pudo generar.
- No inventar noticias. Solo reportar lo encontrado.
