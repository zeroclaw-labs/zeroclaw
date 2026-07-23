cli-about = El asistente de IA más rápido y pequeño.
cli-no-command-provided = No se proporcionó ningún comando.
cli-try-quickstart = Prueba `zeroclaw quickstart` para crear tu primer agente.
cli-quickstart-about = Crea tu primer agente de principio a fin
cli-agent-about = Inicia el bucle del agente de IA
cli-gateway-about = Gestiona el servidor gateway (webhooks, websockets)
cli-acp-about = Inicia el servidor ACP (JSON-RPC 2.0 sobre stdio)
cli-daemon-about = Inicia el daemon autónomo de larga ejecución
cli-service-about = Gestiona el ciclo de vida del servicio del SO (servicio de usuario launchd/systemd)
cli-doctor-about = Ejecuta diagnósticos para la actualidad de daemon/programador/canal
cli-status-about = Muestra el estado del sistema (detalles completos)
cli-estop-about = Activa, inspecciona y reanuda los estados de parada de emergencia
cli-cron-about = Configura y gestiona tareas programadas
cli-models-about = Gestiona los catálogos de modelos del proveedor
cli-providers-about = Lista los proveedores de IA compatibles
cli-channel-about = Gestiona los canales de comunicación
cli-integrations-about = Explora más de 50 integraciones
cli-skills-about = Gestiona habilidades (capacidades definidas por el usuario)
cli-sop-about = Gestiona los procedimientos operativos estándar (SOP)
cli-migrate-about = Migra datos desde otros entornos de ejecución de agentes
cli-auth-about = Gestiona los perfiles de autenticación de suscripción del proveedor
cli-hardware-about = Descubre e inspecciona hardware USB
cli-peripheral-about = Gestiona los periféricos de hardware
cli-memory-about = Gestiona las entradas de memoria del agente
cli-config-about = Gestiona la configuración de ZeroClaw
cli-update-about = Comprueba y aplica las actualizaciones de ZeroClaw
cli-self-test-about = Ejecuta autopruebas de diagnóstico
cli-completions-about = Genera scripts de autocompletado del shell
cli-desktop-about = Inicia la aplicación de escritorio complementaria de ZeroClaw
cli-config-schema-about = Vuelca el esquema JSON de configuración completo en stdout
cli-config-list-about = Lista todas las propiedades de configuración con los valores actuales
cli-config-get-about = Obtiene el valor de una propiedad de configuración
cli-config-set-about = Establece una propiedad de configuración (los campos secretos solicitan automáticamente entrada enmascarada)
cli-config-init-about = Inicializa las secciones no configuradas con valores predeterminados (enabled=false)
cli-config-migrate-about = Migra config.toml a la versión de esquema actual en disco (conserva los comentarios)
cli-service-install-about = Instala la unidad de servicio del daemon para el inicio automático y el reinicio
cli-service-start-about = Inicia el servicio del daemon
cli-service-stop-about = Detiene el servicio del daemon
cli-service-restart-about = Reinicia el servicio del daemon para aplicar la configuración más reciente
cli-service-status-about = Comprueba el estado del servicio del daemon
cli-service-uninstall-about = Desinstala la unidad de servicio del daemon
cli-service-logs-about = Muestra los registros del servicio del daemon
cli-channel-list-about = Lista todos los canales configurados
cli-channel-start-about = Inicia todos los canales configurados
cli-channel-doctor-about = Ejecuta comprobaciones de estado para los canales configurados
cli-channel-add-about = Añade una nueva configuración de canal
cli-channel-remove-about = Elimina una configuración de canal
cli-channel-send-about = Envía un mensaje único a un canal configurado
cli-wechat-pairing-required = 🔐 Se requiere emparejamiento de WeChat. Código de vinculación único: {$code}
cli-wechat-send-bind-command = Envía `{$command} <code>` desde tu WeChat.
cli-wechat-qr-login = 📱 Inicio de sesión QR de WeChat ({$attempt}/{$max})
cli-wechat-scan-to-connect = Escanea con WeChat para conectar.
cli-wechat-qr-url = URL del QR: {$url}
cli-wechat-qr-expired-giving-up = El código QR de WeChat caducó {$max} veces, abandonando.
cli-wechat-qr-fetch-failed = Error al obtener el código QR de WeChat.
cli-wechat-qr-fetch-status-failed = Error al obtener el código QR de WeChat ({$status}): {$body}
cli-wechat-missing-response-field = Falta {$field} en la respuesta de WeChat.
cli-wechat-scanned-confirm = 👀 ¡Escaneado! Confirma en tu teléfono...
cli-wechat-qr-expired-refreshing = ⏳ Código QR caducado, actualizando...
cli-wechat-login-confirmed-missing-field = Inicio de sesión confirmado pero falta {$field}.
cli-wechat-connected = ✅ ¡WeChat conectado!
cli-wechat-bound-success = ✅ Cuenta de WeChat vinculada correctamente. Ya puedes hablar con ZeroClaw.
cli-wechat-invalid-bind-code = ❌ Código de vinculación no válido. Inténtalo de nuevo.
cli-skills-list-about = Listar todas las skills instaladas
cli-skills-audit-about = Auditar un directorio de origen de skill o el nombre de una skill instalada
cli-skills-audit-failed = La auditoría de skills falló.
cli-skills-install-about = Instalar una nueva skill desde una URL o ruta local
cli-skills-remove-about = Eliminar una skill instalada
cli-skills-test-about = Ejecutar la validación TEST.sh para una skill (o todas las skills)
cli-skills-review-summary = { "  " }💾 Revisión de habilidades: {$summary}
cli-skills-install-start = Instalando skill desde: {$source}
cli-skills-install-resolving-registry = { "  " }Resolviendo '{$source}' desde el registro de skills...
cli-skills-install-resolving-extra-registry = { "  " }Resolviendo '{$source}' desde el registro '{$registry}'...
cli-skills-install-git-failed = no se pudo instalar la fuente de skill de git: {$source}
cli-skills-install-registry-failed = no se pudo instalar la skill desde el registro: {$source}
cli-skills-install-extra-registry-failed = no se pudo instalar la skill desde el registro adicional: {$source}
cli-skills-install-local-failed = no se pudo instalar la fuente de skill local: {$source}
cli-skills-install-installed-audited = { "  " }{$status} Skill instalada y auditada: {$path} ({$files} archivos escaneados)
cli-skills-install-security-audit-completed = { "  " }Auditoría de seguridad completada con éxito.
cli-skills-install-into-bundle = { "  " }Instalado en el paquete '{$alias}'. Los agentes que listen este paquete en skill_bundles lo cargarán.
cli-skills-install-global-note = { "  " }Nota: se instaló en el directorio global de skills, que ningún agente carga automáticamente. Vuelve a ejecutar con --bundle <alias>, o asigna un paquete a un agente, para que sea cargable.
cli-skills-removed-archived = { "  " }{$status} La skill '{$name}' se eliminó del paquete '{$bundle}' (archivada en shared/skills/_deleted/).
cli-skills-removed-global = { "  " }{$status} La skill '{$name}' se eliminó del directorio global de skills.
cli-skills-install-tier-official = Instalando {$name} v{$version} — Oficial (mantenida por zeroclaw-labs)
cli-skills-install-tier-community =
    Instalando {$name} v{$version} — Envío de la comunidad
    Esta skill no está auditada por ZeroClaw. Revisa el contenido de la skill
    y ejecuta `zeroclaw skills audit {$name}` antes de otorgar cualquier
    permiso o ejecutarla en producción.
cli-skills-add-scaffolded = Skill {$target} estructurada en {$dir}
cli-skills-bundle-add-prompt =
    Para crear el skill-bundle '{$alias}' con el directorio '{$dir}', ejecuta:
    zeroclaw config map-key skill-bundles {$alias}
    zeroclaw config set skill-bundles.{$alias}.directory {$dir}

    (La creación directa de paquetes mediante `zeroclaw skills bundle add` duplicaría la superficie de mutación de configuración.)
cli-skills-bundle-remove-prompt =
    Para eliminar el skill-bundle '{$alias}', ejecuta:
    zeroclaw config map-key-delete skill-bundles {$alias}

    (Elimina la entrada de configuración; el directorio del paquete en disco se mantiene.)
cli-skills-bundle-list-empty =
    No hay paquetes de skills configurados.
    Crea uno: zeroclaw config set skill-bundles.default.directory shared/skills/default
cli-skills-bundle-list-header = Paquetes de skills ({$count}):
cli-skills-bundle-entry = {$alias} -> {$dir}
cli-skills-bundle-include = incluir: {$values}
cli-skills-bundle-exclude = excluir: {$values}
cli-skills-bundle-show-no-skills = (no hay skills instaladas)
cli-skills-bundle-show-skills-header = skills ({$count}):
cli-skills-bundle-show-skill = {$name}: {$description}
cli-cron-list-about = Listar todas las tareas programadas
cli-cron-add-about = Agregar una nueva tarea programada recurrente
cli-cron-add-at-about = Agregar una tarea de ejecución única que se activa en una marca de tiempo UTC específica
cli-cron-add-every-about = Agregar una tarea que se repite a un intervalo fijo
cli-cron-once-about = Agregar una tarea de ejecución única que se activa tras un retraso desde ahora
cli-cron-remove-about = Eliminar una tarea programada
cli-cron-update-about = Actualizar uno o más campos de una tarea programada existente
cli-cron-pause-about = Pausar una tarea programada
cli-cron-resume-about = Reanudar una tarea pausada
cli-auth-login-about = Iniciar sesión con OAuth (OpenAI Codex, Gemini o xAI)
cli-auth-refresh-about = Actualizar el token de acceso OAuth usando el token de actualización
cli-auth-logout-about = Eliminar perfil de autenticación
cli-auth-use-about = Establecer el perfil activo para un proveedor
cli-auth-list-about = Listar perfiles de autenticación
cli-auth-status-about = Mostrar el estado de autenticación con el perfil activo e información de caducidad del token
cli-memory-list-about = Lista entradas de memoria con filtros opcionales
cli-memory-get-about = Obtiene una entrada de memoria específica por clave
cli-memory-stats-about = Muestra estadísticas y estado del backend de memoria
cli-memory-clear-about = Borra memorias por categoría, por clave, o borra todas
cli-memory-clear-unsupported-backend = memory clear no es compatible con el backend de solo anexado '{$backend}'; cambia a un backend con capacidad de eliminación (sqlite, lucid o postgres)
cli-estop-status-about = Imprimir el estado actual de estop
cli-estop-resume-about = Reanudar desde un nivel de estop activado
cli-models-refresh-about = Actualiza y almacena en caché los modelos del proveedor
cli-models-list-about = Lista los modelos en caché para un proveedor
cli-models-set-about = Establece el modelo predeterminado en la configuración
cli-models-status-about = Muestra la configuración actual del modelo y el estado de la caché
cli-doctor-models-about = Sondea catálogos de modelos en todos los proveedores e informa sobre la disponibilidad
cli-doctor-traces-about = Consulta eventos de traza en tiempo de ejecución (diagnósticos de herramientas y respuestas de modelos)
cli-doctor-update-context-windows-about = Actualiza context_window en config.toml desde los endpoints /models del proveedor
cli-hardware-discover-about = Enumera dispositivos USB y muestra placas conocidas
cli-hardware-introspect-about = Inspecciona un dispositivo por su número de serie o ruta de dispositivo
cli-hardware-info-about = Obtiene información del chip vía USB usando probe-rs sobre ST-Link
cli-peripheral-list-about = Lista los periféricos configurados
cli-peripheral-add-about = Agrega un periférico por tipo de placa y ruta de transporte
cli-peripheral-flash-about = Flashea el firmware de ZeroClaw a una placa Arduino
cli-sop-list-about = Lista los SOP cargados
cli-sop-validate-about = Valida las definiciones de SOP
cli-sop-show-about = Muestra los detalles de un SOP
cli-migrate-openclaw-about = Importa memoria de un espacio de trabajo OpenClaw a este espacio de trabajo ZeroClaw
cli-agent-long-about =
    Inicia el bucle del agente de IA.

    Lanza una sesión de chat interactiva con el proveedor de IA configurado. Usa --message para consultas de una sola vez sin entrar en modo interactivo.

    Ejemplos:
    zeroclaw agent                              # sesión interactiva
    zeroclaw agent -m "Summarize today's logs"  # mensaje único
    zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
    zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
cli-gateway-long-about =
    Gestiona el servidor de gateway (webhooks, websockets).

    Inicia, reinicia o inspecciona el gateway HTTP/WebSocket que acepta eventos de webhook entrantes y conexiones WebSocket.

    Ejemplos:
    zeroclaw gateway start              # iniciar gateway
    zeroclaw gateway restart            # reiniciar gateway
    zeroclaw gateway get-paircode       # mostrar código de emparejamiento
cli-acp-long-about =
    Inicia el servidor ACP (JSON-RPC 2.0 sobre stdio).

    Lanza un servidor JSON-RPC 2.0 en stdin/stdout para la integración con IDE y herramientas. Admite la gestión de sesiones y la transmisión de respuestas del agente como notificaciones.

    Métodos: initialize, session/new, session/prompt, session/stop.

    Ejemplos:
    zeroclaw acp                        # iniciar servidor ACP
    zeroclaw acp --max-sessions 5       # limitar sesiones concurrentes
cli-daemon-long-about =
    Inicia el daemon autónomo de larga duración.

    Lanza el entorno de ejecución completo de ZeroClaw: servidor de gateway, todos los canales configurados (Telegram, Discord, Slack, etc.), monitor de heartbeat y el programador cron. Esta es la forma recomendada de ejecutar ZeroClaw en producción o como un asistente siempre activo.

    Usa 'zeroclaw service install' para registrar el daemon como un servicio del SO (systemd/launchd) para que se inicie automáticamente al arrancar.

    Ejemplos:
    zeroclaw daemon                   # usar valores predeterminados de config
    zeroclaw daemon -p 9090           # gateway en el puerto 9090
    zeroclaw daemon --host 127.0.0.1  # solo localhost
cli-cron-long-about =
    Configura y gestiona tareas programadas.

    Programación de tareas recurrentes, de una sola vez o basadas en intervalos usando expresiones cron, marcas de tiempo RFC 3339, duraciones o intervalos fijos.

    Las expresiones cron usan el formato estándar de 5 campos: 'min hora día mes díasemana'. Las zonas horarias predeterminadas son UTC; anúlalas con --tz y un nombre de zona horaria IANA.

    Ejemplos:
    zeroclaw cron list
    zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York --agent
    zeroclaw cron add '*/30 * * * *' 'Check system health' --agent
    zeroclaw cron add '*/5 * * * *' 'echo ok'
    zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder' --agent
    zeroclaw cron add-every 60000 'Ping heartbeat'
    zeroclaw cron once 30m 'Run backup in 30 minutes' --agent
    zeroclaw cron pause TASK_ID
    zeroclaw cron update TASK_ID --expression '0 8 * * *' --tz Europe/London
cli-channel-long-about =
    Gestiona los canales de comunicación.

    Agrega, elimina, lista, envía y verifica el estado de los canales que conectan ZeroClaw con plataformas de mensajería. Tipos de canal admitidos: telegram, discord, slack, whatsapp, matrix, imessage, email.

    Ejemplos:
    zeroclaw channel list
    zeroclaw channel doctor
    zeroclaw channel add telegram '{ "{" }"bot_token":"...","name":"my-bot"{ "}" }'
    zeroclaw channel remove my-bot
    zeroclaw channel bind-telegram zeroclaw_user
    zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789
cli-hardware-long-about =
    Descubre e inspecciona hardware USB.

    Enumera dispositivos USB conectados, identifica placas de desarrollo conocidas (STM32 Nucleo, Arduino, ESP32) y recupera información del chip mediante probe-rs / ST-Link.

    Ejemplos:
    zeroclaw hardware discover
    zeroclaw hardware introspect /dev/ttyACM0
    zeroclaw hardware info --chip STM32F401RETx
cli-peripheral-long-about =
    Gestiona los periféricos de hardware.

    Agrega, lista, flashea y configura placas de hardware que exponen herramientas al agente (GPIO, sensores, actuadores). Placas admitidas: nucleo-f401re, rpi-gpio, esp32, arduino-uno.

    Ejemplos:
    zeroclaw peripheral list
    zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
    zeroclaw peripheral add rpi-gpio native
    zeroclaw peripheral flash --port /dev/cu.usbmodem12345
    zeroclaw peripheral flash-nucleo
cli-memory-long-about =
    Gestiona las entradas de memoria del agente.

    Lista, inspecciona y borra entradas de memoria almacenadas por el agente. Admite filtrado por categoría y sesión, paginación y borrado por lotes con confirmación.

    Ejemplos:
    zeroclaw memory stats
    zeroclaw memory list
    zeroclaw memory list --category core --limit 10
    zeroclaw memory get KEY
    zeroclaw memory clear --category conversation --yes
cli-config-long-about =
    Gestiona la configuración de ZeroClaw.

    Visualiza, establece o inicializa propiedades de configuración mediante una ruta con puntos. Usa 'schema' para volcar el esquema JSON completo del archivo de configuración.

    Las propiedades se direccionan mediante una ruta con puntos (p. ej. channels.matrix.mention-only).
    Los campos secretos (claves API, tokens) usan automáticamente entrada enmascarada.
    Los campos enum ofrecen selección interactiva cuando se omite el valor.

    Ejemplos:
    zeroclaw config list                                  # listar todas las propiedades
    zeroclaw config list --secrets                        # listar solo secretos
    zeroclaw config list --filter channels.matrix         # filtrar por prefijo
    zeroclaw config get channels.matrix.mention-only      # obtener un valor
    zeroclaw config set channels.matrix.mention-only true # establecer un valor
    zeroclaw config set channels.matrix.access-token      # secreto: entrada enmascarada
    zeroclaw config set channels.matrix.stream-mode       # enum: selección interactiva
    zeroclaw config init channels.matrix                  # iniciar sección con valores predeterminados
    zeroclaw config schema                                # imprimir esquema JSON en stdout
    zeroclaw config schema > schema.json

    El autocompletado de la ruta de propiedades se incluye automáticamente en `zeroclaw completions <shell>`.
cli-update-long-about =
    Comprueba y aplica actualizaciones de ZeroClaw.

    De forma predeterminada, descarga e instala la última versión con un pipeline de 6 fases: verificación previa, descarga, copia de seguridad, validación, intercambio y prueba de humo. Reversión automática en caso de fallo.

    Usa --check para solo comprobar actualizaciones sin instalar.
    Usa --force para omitir el aviso de confirmación.
    Usa --version para apuntar a una versión específica en lugar de la última.

    Ejemplos:
    zeroclaw update                      # descargar e instalar la última
    zeroclaw update --check              # solo comprobar, no instalar
    zeroclaw update --force              # instalar sin confirmación
    zeroclaw update --version 0.6.0      # instalar versión específica
cli-self-test-long-about =
    Ejecuta autodiagnósticos para verificar la instalación de ZeroClaw.

    De forma predeterminada, ejecuta la suite de pruebas completa, incluidas las comprobaciones de red (estado del gateway, ida y vuelta de memoria). Usa --quick para omitir las comprobaciones de red y validar más rápido sin conexión.

    Ejemplos:
    zeroclaw self-test             # suite completa
    zeroclaw self-test --quick     # solo comprobaciones rápidas (sin red)
cli-skills-install-suggestion =
    Parece que esta solicitud necesita la habilidad `{$name}`, pero no está instalada.

    Capacidad coincidente: {$matched}
    Siguiente: Ejecuta `{$install_command}` para instalarla.
cli-plugin-install-suggestion =
    Parece que esta solicitud necesita el plugin `{$name}`, pero no está instalado.

    Capacidad coincidente: {$matched}
    Siguiente: Ejecuta `{$install_command}` para instalarlo.
cli-completions-long-about =
    Genera scripts de autocompletado de shell para `zeroclaw`.

    El script se imprime en stdout para que pueda obtenerse directamente:

    Ejemplos:
    source <(zeroclaw completions bash)
    zeroclaw completions zsh > ~/.zfunc/_zeroclaw
    zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish
cli-desktop-long-about =
    Lanza la aplicación de escritorio complementaria de ZeroClaw.

    La aplicación complementaria es una aplicación ligera de barra de menú / bandeja del sistema que se conecta al mismo gateway que la CLI. Proporciona acceso rápido al panel, monitoreo de estado y emparejamiento de dispositivos.

    Usa --install para descargar la aplicación complementaria precompilada para tu plataforma.

    Ejemplos:
    zeroclaw desktop              # lanzar la aplicación complementaria
    zeroclaw desktop --install    # descargarla e instalarla
channel-needs-quickstart-reply = Este agente aún no está completamente configurado. El operador debe ejecutar Quickstart antes de que pueda responder.
channel-whatsapp-web-feature-missing-warning = ⚠ WhatsApp Web está configurado pero la característica 'whatsapp-web' no está compilada.
channel-whatsapp-web-feature-missing-build = Compila/ejecuta con: cargo build --features whatsapp-web
channel-whatsapp-web-feature-missing-install = Si está instalado en PATH, reinstala con: cargo install --path . --force --locked --features whatsapp-web
channel-whatsapp-web-feature-missing-error = El canal WhatsApp Web requiere la característica 'whatsapp-web'. Actívala con: cargo build --features whatsapp-web (o, si está instalado en PATH: cargo install --path . --force --locked --features whatsapp-web)
channel-wecom-ws-stream-bootstrap = Trabajando en ello, por favor espera.
channel-wecom-ws-stop-ack = Se detuvo el mensaje actual.
channel-wecom-ws-voice-unavailable = No puedo procesar mensajes de voz en este momento {$emoji}
channel-wecom-ws-unsupported-message = Este tipo de mensaje aún no es compatible.
channel-wecom-ws-welcome = Hola, bienvenido a chatear conmigo {$emoji}
channel-wecom-ws-supplemental-message =
    {"["}Mensaje complementario]
    {$extra}
channel-wecom-ws-group-allowlist-missing =
    La lista de permitidos de WeCom no está configurada, por lo que este bot no acepta mensajes de grupo.

    Group chatid: {$chatid}
    Sender userid: {$userid}

    Agrega una entrada permitida a {$allowed_groups_path} o {$allowed_users_path}. También puedes configurarla temporalmente como ["*"] para pruebas.
channel-wecom-ws-group-access-denied =
    Este grupo no tiene permiso para usar este bot.

    Group chatid: {$chatid}
    Sender userid: {$userid}

    Pide a un administrador que añada este grupo a {$allowed_groups_path}, o añade tu userid a {$allowed_users_path}.
channel-wecom-ws-dm-allowlist-missing =
    La lista de permitidos de WeCom no está configurada, por lo que este bot no acepta mensajes.

    Tu userid: {$userid}

    Añade una entrada permitida a {$allowed_users_path}. También puedes establecerlo temporalmente en ["*"] para realizar pruebas.
channel-wecom-ws-dm-access-denied =
    No tienes permiso para usar este bot.

    Tu userid: {$userid}

    Pide a un administrador que añada tu userid a {$allowed_users_path}.
channel-discord-interaction-unauthorized = No tienes permiso para usar este comando aquí.
channel-discord-interaction-malformed = Comando desconocido o mal formado.
channel-discord-interaction-unavailable = Ese comando ya no está disponible o su entrada estaba vacía.
channel-discord-component-expired = Este botón o menú ha expirado o ya fue utilizado.
channel-discord-approval-recorded = Tu decisión ha sido registrada.
channel-discord-delivery-failure-note-one = (nota: no pude entregar {$count} archivo.)
channel-discord-delivery-failure-note-many = (nota: no pude entregar {$count} archivos.)
channel-whatsapp-web-delivery-failure-note-one = (nota: no pude entregar {$count} archivo multimedia de WhatsApp.)
channel-whatsapp-web-delivery-failure-note-many = (nota: no pude entregar {$count} archivos multimedia de WhatsApp.)
channel-line-bind-success = ✅ ¡Emparejado! Ya puedes chatear.
channel-line-bind-invalid-code = ❌ Código no válido. Inténtalo de nuevo.
channel-line-bind-rate-limited = ⏳ Demasiados intentos. Reintenta en { $secs }s.
onboard-openai-auth-note =
    Autenticación de OpenAI:
    • Clave de API — acceso estándar a la API mediante platform.openai.com (sk-...)
    • Suscripción de Codex — usa tu cuenta de ChatGPT Plus/Pro (no se necesita clave de API)
onboard-openai-auth-prompt = Autenticación
onboard-openai-auth-api-key = Clave de API
onboard-openai-auth-codex = Suscripción de Codex
onboard-openai-codex-followup =
    La autenticación con la suscripción de Codex usa tu cuenta de ChatGPT.
    Ejecuta `zeroclaw auth login --provider openai-codex` para autenticarte antes de iniciar tu agente.
cli-web-dist-dir-reason-tilde = comienza con `~`, que no se expande
cli-web-dist-dir-reason-dollar = contiene `$`, que no se expande
cli-doctor-web-dist-dir-expansion-warning = gateway.web_dist_dir = "{$path}" — {$reason}; gateway.web_dist_dir se lee literalmente, así que expande el valor tú mismo (p. ej., una ruta absoluta)
cli-doctor-codex-auth-profile-no-slot = Las credenciales de OpenAI Codex tienen sesión iniciada, pero ningún slot de proveedor de modelo las usa. Establece `requires_openai_auth = true` en un slot de proveedor OpenAI y apunta el `model_provider` de un agente a él, o ejecuta `zeroclaw quickstart`.
cli-doctor-codex-auth-slot-no-profile = Los slots OpenAI {$slots} tienen `requires_openai_auth = true`, pero no hay credenciales de OpenAI Codex con sesión iniciada. Ejecuta `zeroclaw auth login --provider openai-codex`.
cli-doctor-codex-auth-ok = Las credenciales de OpenAI Codex tienen sesión iniciada y están referenciadas por un slot de proveedor de modelo.
cli-doctor-systemd-linger-enabled = la permanencia de usuario de systemd está habilitada
cli-doctor-systemd-linger-disabled = la permanencia de usuario de systemd está deshabilitada; el servicio de usuario puede detenerse tras cerrar sesión. Habilítala con: loginctl enable-linger {$user}
cli-doctor-systemd-linger-unknown = no se pudo comprobar la permanencia de usuario de systemd con loginctl
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = no establecido (usando detección automática)
cli-self-test-web-dist-dir-pass-literal = {$path} (ruta literal)
cli-self-test-web-dist-dir-fail-expansion = ADVERTENCIA: {$path} — {$reason}; gateway.web_dist_dir se lee literalmente, así que expande el valor tú mismo (p. ej., una ruta absoluta)
cli-service-systemd-linger-disabled-warning = la permanencia de usuario de systemd está deshabilitada. El servicio de usuario de ZeroClaw puede detenerse tras cerrar sesión. Habilítala con: loginctl enable-linger {$user}
cli-peripherals-none = No hay periféricos configurados.
cli-peripherals-add-hint = Agregue uno con: zeroclaw peripheral add <board> <path>
cli-peripherals-add-example = {"  "}Ejemplo: zeroclaw peripheral add nucleo-f401re <serial-path>
cli-peripherals-config-hint = O agregue a config.toml:
cli-peripherals-configured = Periféricos configurados:
cli-peripherals-already-configured = La placa {$board} en {$path} ya está configurada.
cli-peripherals-added = Se agregó {$board} en {$path}. Reinicie el daemon para aplicar.
cli-peripherals-flash-needs-hardware = El flasheo de Arduino requiere la característica 'hardware'.
cli-peripherals-unoq-needs-hardware = La configuración de Uno Q requiere la característica 'hardware'.
cli-peripherals-nucleo-needs-hardware = El flasheo de Nucleo requiere la característica 'hardware'.
cli-skills-none-installed = No hay skills instaladas.
cli-skills-create-hint = {"  "}Cree uno: mkdir -p ~/.zeroclaw/workspace/skills/my-skill
cli-skills-install-hint = {"  "}O instale: zeroclaw skills install <source>
cli-skills-installed-header = Skills instaladas ({$count}):
cli-skills-list-group-bundle = paquete: {$alias}
cli-skills-list-group-agent = cargada por el agente '{$alias}'
cli-skills-list-group-global = global / open-skills / plugins (no de un paquete)
cli-skills-agent-not-configured = el agente '{$alias}' no está configurado
cli-skills-agent-multiple-bundles = el agente '{$alias}' tiene múltiples paquetes de skills ({$bundles}); pasa --bundle para elegir uno
cli-skills-multiple-locations-bundle = la skill '{$name}' existe en múltiples ubicaciones ({$locations}); pasa --bundle para elegir una
cli-skills-multiple-locations-path = la skill '{$name}' existe en múltiples ubicaciones ({$locations}); pasa una ruta explícita para desambiguar
cli-skills-tags = Etiquetas:  {$tags}
cli-skills-skipped-header = Omitidas ({$count}):
cli-skills-skipped-reason = {"    "}Motivo: {$reason}
cli-skills-skipped-scripts-hint = {"    "}Establece `skills.allow_scripts = true` en tu configuración de zeroclaw para habilitarla.
cli-sop-none = No se encontraron SOP.
cli-sop-pending-none = No hay ejecuciones de SOP esperando aprobación.
cli-sop-pending-header = Ejecuciones de SOP esperando aprobación:
cli-sop-pending-row = {"  "}{$run_id} [{$sop_name}] paso {$step}/{$total}
cli-sop-ws-invalid-approval = sop approval_response requiere run_id y una decisión approve o deny
cli-sop-ws-resolve-failed = error al resolver SOP: {$error}
cli-sop-ws-engine-lock-poisoned = bloqueo del motor SOP envenenado
cli-sop-ws-subsystem-disabled = el subsistema SOP no está habilitado
cli-sop-create-hint = {"  "}Cree uno: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}luego agregue SOP.toml y SOP.md
cli-sop-loaded-header = SOP cargados ({$count}):
cli-sop-none-to-validate = No se encontraron SOP para validar.
cli-sop-valid = ✅ {$name} — válido
cli-sop-deleted = SOP eliminado: {$name}
cli-sop-warnings = ⚠️  {$name} — {$count} advertencia(s):
cli-sop-all-passed = Todos los SOP pasaron la validación.
cli-sop-priority = {"  "}Prioridad:       {$value}
cli-sop-execution-mode = {"  "}Modo de ejecución: {$value}
cli-sop-deterministic = {"  "}Determinista:  {$value}
cli-sop-cooldown = {"  "}Tiempo de espera: {$value}s
cli-sop-max-concurrent = {"  "}Máx. concurrentes: {$value}
cli-sop-admission-policy = {"  "}Admisión:        {$value}
cli-sop-max-pending-approvals = {"  "}Máx. pendientes: {$value}
cli-sop-location = {"  "}Ubicación:       {$value}
cli-sop-triggers = {"  "}Disparadores:
cli-sop-steps = {"  "}Pasos:
cli-sop-step-tools = Herramientas: {$tools}
cli-memory-reindexing = Reindexando el backend de memoria...
cli-memory-none = No se encontraron entradas de memoria.
cli-memory-none-at-offset = No hay entradas en el desplazamiento {$offset} (total: {$total}).
cli-memory-next-page = Use --offset {$offset} para ver la página siguiente.
cli-memory-key-not-found = No se encontró ninguna entrada de memoria para la clave: {$key}
cli-memory-prefix-matched = El prefijo '{$key}' coincidió con {$n} entradas:
cli-memory-narrow-prefix = Especifique un prefijo más largo para acotar la coincidencia.
cli-memory-key = Clave:       {$value}
cli-memory-category = Categoría:  {$value}
cli-memory-timestamp = Marca de tiempo: {$value}
cli-memory-session = Sesión:   {$value}
cli-memory-stats-header = Estadísticas de memoria:
cli-memory-backend = {"  "}Backend:  {$value}
cli-memory-total = {"  "}Total:    {$value}
cli-memory-by-category = {"  "}Por categoría:
cli-memory-none-to-clear = No hay entradas para borrar.
cli-memory-found-in-scope = Se encontraron {$count} entradas en '{$scope}'.
cli-memory-aborted = Abortado.
cli-memory-deleted-key = Clave eliminada: {$key}
cli-cron-none = Aún no hay tareas programadas.
cli-cron-usage = Uso:
cli-cron-jobs-header = 🕒 Tareas programadas ({$count}):
cli-cron-list-cmd = {"    "}cmd: {$cmd}
cli-cron-list-prompt = {"    "}prompt: {$prompt}
cli-cron-added-agent = ✅ Tarea cron de agente agregada {$id}
cli-cron-added = ✅ Tarea cron agregada {$id}
cli-cron-added-oneshot-agent = ✅ Tarea cron de agente de una sola vez agregada {$id}
cli-cron-added-oneshot = ✅ Tarea cron de una sola vez agregada {$id}
cli-cron-added-interval-agent = ✅ Tarea cron de agente por intervalo agregada {$id}
cli-cron-added-interval = ✅ Tarea cron de intervalo agregada {$id}
cli-cron-updated = ✅ Tarea cron actualizada {$id}
cli-cron-update-no-field = Se debe proporcionar al menos uno de --expression, --tz, --command, --name, --allowed-tool, o --uses-memory
cli-cron-removed = ✅ Tarea cron eliminada {$id}
cli-cron-paused = ⏸️  Tarea cron pausada {$id}
cli-cron-resumed = ▶️  Tarea cron reanudada {$id}
cli-cron-expr = {"  "}Expr  : {$v}
cli-cron-expr2 = {"  "}Expr: {$v}
cli-cron-next = {"  "}Siguiente  : {$v}
cli-cron-next2 = {"  "}Siguiente: {$v}
cli-cron-next3 = {"  "}Siguiente     : {$v}
cli-cron-prompt = {"  "}Prompt: {$v}
cli-cron-prompt3 = {"  "}Prompt   : {$v}
cli-cron-cmd = {"  "}Cmd : {$v}
cli-cron-cmd3 = {"  "}Cmd      : {$v}
cli-cron-at = {"  "}En    : {$v}
cli-cron-at2 = {"  "}En  : {$v}
cli-cron-every = {"  "}Cada(ms): {$v}
cli-no-command = No se proporcionó ningún comando.
cli-press-enter = Presiona Enter para salir...
cli-quickstart-title = Quickstart — crea un agente funcional de principio a fin.
cli-quickstart-needs-tty = Quickstart es interactivo y necesita una terminal en stdin y stderr. Ejecútalo desde una shell interactiva, o usa `zeroclaw config set <path> <value>` para configuración sin interfaz.
cli-quickstart-cancelled = Quickstart cancelado. No se escribió ninguna configuración.
cli-quickstart-incomplete = {"  "}Aún no se han completado todos los selectores.
cli-quickstart-create-agent = ── Crear agente
cli-quickstart-create-agent-locked = ── Crear agente (bloqueado — completa todos los selectores primero)
cli-quickstart-open-selector-prompt = Abre un selector (Enter) o elige Crear. Esc para salir.
cli-quickstart-use-existing = Usar existente
cli-quickstart-create-new = Crear nuevo
cli-quickstart-model-provider-prompt = Proveedor de modelo
cli-quickstart-pick-configured-provider = Elige un proveedor configurado
cli-quickstart-row-model-provider = {$glyph} Proveedor de modelo — {$summary}
cli-quickstart-row-risk-profile = {$glyph} Perfil de riesgo   — {$summary}
cli-quickstart-row-memory = {$glyph} Memoria            — {$summary}
cli-quickstart-row-channels = {$glyph} Canales (0..N)    — {$summary}
cli-quickstart-row-peer-groups = {$glyph} Grupos de pares   — {$summary}
cli-quickstart-row-agent-identity = {$glyph} Identidad agente — {$summary}
cli-quickstart-summary-not-yet-chosen = aún no elegido
cli-quickstart-summary-not-yet-visited = aún no visitado
cli-quickstart-summary-not-yet-named = aún sin nombre
cli-quickstart-summary-provider-fresh = {$name} (alias: {$alias}, modelo: {$model})
cli-quickstart-summary-use-existing = usar existente {$reference}
cli-quickstart-summary-preset-fresh = preset: {$name}
cli-quickstart-summary-channels-none = ninguno (chatea solo con `zeroclaw agent`)
cli-quickstart-summary-agent = alias: {$alias}, prompt del sistema: {$chars} caracteres, {$files} archivo(s) de personalidad
cli-quickstart-summary-peer-groups-none = ninguno — los canales no aceptan pares
cli-quickstart-channel-remove-row = {"  "}{$reference} (quitar)
cli-quickstart-peer-group-row = {$channel} → {$name} ({$count} pares)
cli-quickstart-provider-local-label = {$name} (local)
cli-quickstart-provider-type-prompt = Tipo de proveedor
cli-quickstart-alias-for = Alias para {$name}
cli-quickstart-openai-auth-mode-label = Autenticación
cli-quickstart-openai-auth-mode-help = Elige `codex` para usar un perfil de autenticación de suscripción de ChatGPT/Codex. Si ya iniciaste sesión con el CLI de Codex, ejecuta `zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json`; de lo contrario ejecuta `zeroclaw auth login --model-provider openai-codex`.
cli-quickstart-anthropic-auth-mode-label = Autenticación
cli-quickstart-anthropic-auth-mode-help = Elige `api_key` para una clave de Anthropic Console, o `setup_token` si vas a ejecutar `claude setup-token` para Claude Max y pegar el token generado.
cli-quickstart-anthropic-api-key-help = Pega una clave API de Anthropic Console o el token generado por `claude setup-token`.
cli-quickstart-auth-codex-prompt = ¿Iniciar sesión en OpenAI Codex con tu cuenta de ChatGPT ahora?
cli-quickstart-auth-codex-import-prompt = Se encontró un inicio de sesión existente de Codex (~/.codex/auth.json) — ¿importarlo ahora?
cli-quickstart-auth-codex-skip-hint = {"  "}Termina más tarde con: zeroclaw auth login --model-provider openai-codex
cli-quickstart-auth-anthropic-prompt = ¿Ejecutar `claude setup-token` para el proveedor de Anthropic `{$alias}` ahora?
cli-quickstart-auth-anthropic-token-prompt = Pega el token de `claude setup-token`
cli-quickstart-auth-anthropic-saved = {"  "}Token de configuración de Claude guardado para anthropic.{$alias}
cli-quickstart-auth-anthropic-skip-hint = {"  "}Termina más tarde con: claude setup-token, luego zeroclaw config set providers.models.anthropic.{$alias}.api_key <token>
cli-quickstart-auth-failed = {"  "}La configuración de autenticación no se completó: {$error}
cli-quickstart-model-field-missing-warning = ADVERTENCIA: el esquema no produjo un campo `model` para `{$provider}` — se usará entrada manual. Informa de esto.
cli-quickstart-model-id-for = ID de modelo para {$name}
cli-quickstart-risk-profile-prompt = Perfil de riesgo
cli-quickstart-memory-backend-prompt = Backend de memoria
cli-quickstart-add-channel = + Agregar canal
cli-quickstart-channels-done = Listo (el selector de canales cuenta como visitado)
cli-quickstart-channels-prompt = Canales (opcional, 0..N)
cli-quickstart-channel-source-prompt = Origen del canal
cli-quickstart-all-channels-bound = {"  "}Todos los canales configurados ya están vinculados a un agente. Libera uno con `zeroclaw config set agents.<alias>.channels ...` antes de reutilizarlo aquí.
cli-quickstart-pick-configured-channel = Elegir un canal configurado
cli-quickstart-channel-type-prompt = Tipo de canal
cli-quickstart-add-peer-group = + Agregar grupo de pares
cli-quickstart-done = Listo
cli-quickstart-peer-groups-prompt = Grupos de pares (Enter en una fila para quitar, + Agregar para crear)
cli-quickstart-channel-to-authorize-prompt = Canal para autorizar
cli-quickstart-external-peers-prompt = Pares externos (separados por comas o saltos de línea; vacío para ninguno)
cli-quickstart-agent-alias-prompt = Alias del agente
cli-quickstart-edit-system-prompt = ¿Editar el prompt del sistema en $EDITOR? (vacío para omitir)
cli-quickstart-personality-start-template = Empezar con plantilla (abrir en $EDITOR)
cli-quickstart-personality-start-current = Empezar desde el contenido actual (abrir en $EDITOR)
cli-quickstart-personality-start-scratch = Empezar desde cero (abrir en $EDITOR)
cli-quickstart-personality-skip = Omitir
cli-quickstart-esc-go-back = {" "}(Esc para volver)
cli-quickstart-esc-return-checklist = {" "}(Esc para volver a la lista)
cli-quickstart-personality-file-prompt = {$filename}{$position} — ¿qué sigue?{$back_hint}
cli-quickstart-next-agent-command = {"  "}zeroclaw agent -a {$alias}  # chatea con este agente en tu terminal
cli-quickstart-fix-and-rerun = Tu configuración existente no se modificó. Corrige lo siguiente y vuelve a ejecutar quickstart:
cli-quickstart-could-not-finish = quickstart no pudo terminar: {$count} problema(s) por corregir
cli-quickstart-pick-preset = Elegir un preset
cli-quickstart-pick-existing-prompt = Elegir un {$prompt} existente
cli-quickstart-pick-preset-prompt = Elegir un preset de {$prompt}
cli-quickstart-step-model-provider = Proveedor de modelo
cli-quickstart-step-risk-profile = Perfil de riesgo
cli-quickstart-step-runtime-profile = Perfil de runtime
cli-quickstart-step-memory = Memoria
cli-quickstart-step-channels = Canales
cli-quickstart-step-peer-groups = Grupos de pares
cli-quickstart-step-agent = Agente
cli-quickstart-error-internal-no-result = error interno: apply_into no devolvió resultado aunque no hubo errores de validación
cli-quickstart-error-completion-flag = no se pudo cambiar quickstart-completed: {$err}
cli-quickstart-error-persist-config = no se pudo persistir la configuración: {$err}
cli-quickstart-error-not-type-alias-ref = `{$reference}` no es una referencia `<type>.<alias>`
cli-quickstart-error-no-configured-path = no hay `{$path}` configurado
cli-quickstart-error-provider-required = se requieren tipo de proveedor, alias y modelo
cli-quickstart-error-unknown-provider-type = tipo de proveedor de modelo desconocido `{$provider}` — elige uno de la lista de proveedores
cli-quickstart-error-unknown-openai-auth-mode = modo de autenticación de OpenAI desconocido `{$mode}` — elige `api_key` o `codex`
cli-quickstart-error-unknown-anthropic-auth-mode = modo de autenticación de Anthropic desconocido `{$mode}` — elige `api_key` o `setup_token`
cli-quickstart-error-alias-exists = el alias `{$alias}` ya existe
cli-quickstart-error-no-profile = no hay perfil `{$alias}` configurado
cli-quickstart-error-unknown-risk-preset = preset de riesgo desconocido `{$preset}`
cli-quickstart-error-unknown-runtime-preset = preset de runtime desconocido `{$preset}`
cli-quickstart-error-channel-bound = el canal `{$reference}` ya está vinculado al agente `{$owner}`
cli-quickstart-error-channel-required = se requieren tipo de canal y alias
cli-quickstart-error-channel-field-not-advertised = el campo de canal `{$field}` no está disponible en Quickstart
cli-quickstart-error-channel-token-required = se requiere el token del bot de Telegram
cli-quickstart-error-peer-group-name-required = se requiere el nombre del grupo de pares
cli-quickstart-error-peer-group-channel-required = se requiere la referencia de canal del grupo de pares
cli-quickstart-error-peer-group-unknown-channel = el grupo de pares `{$name}` referencia un canal desconocido `{$channel}`
cli-quickstart-error-peer-group-exists = el grupo de pares `{$name}` ya existe
cli-quickstart-error-personality-workspace = no se pudo crear el workspace del agente: {$err}
cli-quickstart-error-personality-filename-required = se requiere el nombre de archivo
cli-quickstart-error-personality-not-editable = `{$filename}` no es un archivo de personalidad editable
cli-quickstart-error-personality-too-large = el contenido supera el límite de {$limit} caracteres
cli-quickstart-error-personality-stage-failed = preparar {$filename} falló: {$err}
cli-quickstart-error-personality-write-failed = escribir {$path} falló: {$err}
cli-quickstart-error-agent-name-required = se requiere el nombre del agente
cli-quickstart-error-agent-exists = el agente `{$name}` ya existe
cli-no-channels-compiled = {"  "}No hay tipos de canal compilados en este binario.
cli-quickstart-complete = Quickstart completado. Se creó el agente `{$alias}`.
cli-next-steps = Siguientes pasos:
cli-agent-not-created = Tu agente no fue creado — y no se cambió nada en el disco.
cli-onboard-deprecated = `zeroclaw onboard` está obsoleto — usa `zeroclaw quickstart`.
cli-otp-initialized = Secreto OTP inicializado para ZeroClaw.
cli-otp-enrollment-uri = URI de inscripción: {$uri}
cli-otp-received = {"  "}✓ OTP recibido
cli-secret-captured = {"  "}● Valor capturado — pulse Enter para guardar
cli-secret-received = {"  "}✓ Secreto recibido
cli-pairing-enabled = 🔐 El emparejamiento del gateway está habilitado.
cli-pairing-use-code = {"  "}Usa este código de un solo uso para emparejar un nuevo dispositivo:
cli-pairing-post = {"    "}POST /pair con encabezado X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}Reinicia el gateway para generar un nuevo código de emparejamiento.
cli-pairing-disabled = ⚠️  El emparejamiento del gateway está deshabilitado en la configuración.
cli-gateway-running-q = {"   "}¿Está el gateway en ejecución? Inícialo con:
cli-status-title = 🦀 Estado de ZeroClaw
cli-security-status-title = Estado de seguridad de ZeroClaw
cli-security-status-source = Origen:      {$v}
cli-security-status-agent = Agente:       {$v}
cli-security-status-agent-enabled = Agente habilitado: {$enabled}
cli-security-status-risk-profile = Perfil de riesgo: {$v}
cli-security-status-autonomy = Autonomía:   {$v}
cli-security-status-approvals = Aprobaciones:  aprobación requerida para riesgo medio: {$medium}, comandos de alto riesgo bloqueados: {$high}
cli-security-status-sandbox = Sandbox:    solicitado {$requested}, activo {$active} ({$description})
cli-security-status-workspace = Espacio de trabajo:  {$dir}; solo espacio de trabajo: {$workspace_only}; raíces rw: {$read_write_roots}; raíces de solo lectura: {$read_only_roots}; raíces de solo escritura: {$write_only_roots}; paso de entorno: {$env_passthrough}
cli-security-status-credentials = Credenciales: cifrado: {$encryption}; secretos definidos: {$secrets_set}/{$secrets_total}; campos clasificados: {$classified_total}; clases: {$classification_summary}
cli-security-status-credentials-classes-none = ninguna
cli-security-status-gateway = Gateway:    {$host}:{$port}; emparejamiento requerido: {$pairing}; enlace público: {$public_bind}; TLS: {$tls}
cli-security-status-warnings = Advertencias:   {$v}
cli-security-status-warnings-none = Advertencias:   ninguna
cli-security-status-warning-agent-disabled = el agente está deshabilitado
cli-security-status-warning-sandbox-disabled = el sandbox está deshabilitado para este perfil de riesgo del agente
cli-security-status-warning-sandbox-none = el sandbox activo es solo de capa de aplicación
cli-security-status-warning-sandbox-fallback = el backend de sandbox solicitado `{$requested}` recurrió a `{$active}`
cli-security-status-warning-workspace-not-restricted = la política de sistema de archivos solo del espacio de trabajo está deshabilitada
cli-security-status-warning-shell-env-passthrough = {$count} variable(s) de entorno del shell se pasan directamente
cli-security-status-warning-secrets-unencrypted = el cifrado de secretos de configuración está deshabilitado
cli-security-status-warning-credential-follow-up = algunas superficies de configuración con forma de credencial aún requieren seguimiento
cli-security-status-warning-pairing-disabled = no se requiere el emparejamiento del gateway
cli-security-status-warning-public-bind-no-tls = el gateway permite enlace público sin TLS habilitado
cli-status-provider-none = 🤖 ModelProvider:      (ninguno configurado)
cli-status-agents-none = 🛡️  Agentes:        (ninguno configurado)
cli-status-service-running = 🟢 Servicio:       en ejecución
cli-status-service-stopped = 🔴 Servicio:       detenido
cli-status-channels = Canales:
cli-status-cli-always = {"  "}CLI:      ✅ siempre
cli-status-peripherals = Periféricos:
cli-desktop-download = Descarga la aplicación complementaria de ZeroClaw:
cli-desktop-homebrew = O instálala con Homebrew (próximamente):
cli-desktop-linux-pkg = {"  "}Descarga el .deb o .AppImage para tu arquitectura.
cli-desktop-launching = Iniciando la aplicación complementaria de ZeroClaw...
cli-status-version = Versión:     {$v}
cli-status-workspace = Espacio de trabajo:   {$v}
cli-status-config = Configuración:      {$v}
cli-status-provider-indent = {"   "}ModelProvider:      {$family}.{$alias}
cli-status-provider = 🤖 ModelProvider:      {$family}.{$alias}
cli-status-model = {"   "}Modelo:         {$model}
cli-status-observability = 📊 Observabilidad:  {$v}
cli-status-trace-storage = 🧾 Almacenamiento de trazas:  {$mode} ({$path})
cli-status-agents = 🛡️  Agentes:        {$v}
cli-status-runtime = ⚙️  Entorno de ejecución:       {$v}
cli-status-heartbeat = 💓 Latido:      {$v}
cli-status-heartbeat-every-minutes = cada {$minutes}min
cli-status-memory = 🧠 Memoria:         {$backend} (autoguardado: {$auto_save})
cli-status-security-noprofile = Seguridad ({$alias}): <sin risk_profile>
cli-status-security = Seguridad ({$alias}):
cli-status-workspace-only = {"  "}Solo espacio de trabajo:    {$v}
cli-status-allowed-roots = {"  "}Raíces permitidas:     {$v}
cli-status-allowed-commands = {"  "}Comandos permitidos:  {$v}
cli-status-max-actions = {"  "}Máx. acciones/hora:  {$v}
cli-status-cost-tracking = {"  "}Seguimiento de costos:     {$v}
cli-status-max-cost-day = {"  "}Costo máx./día:      ${$v}
cli-status-max-cost-month = {"  "}Costo máx./mes:    ${$v}
cli-status-spent-today = {"  "}Gastado hoy:       ${$spent} / ${$limit}
cli-status-spent-month = {"  "}Gastado este mes:  ${$spent} / ${$limit}
cli-status-otp = {"  "}OTP habilitado:       {$v}
cli-status-estop = {"  "}Parada de emergencia activada:    {$v}
cli-status-peripherals-enabled = {"  "}Habilitado:   {$v}
cli-status-boards = {"  "}Tableros:    {$v}
cli-status-word-enabled = habilitado
cli-status-word-disabled = deshabilitado
cli-status-word-yes = sí
cli-status-word-no = no
cli-status-word-on = activado
cli-status-word-off = desactivado
cli-status-word-none = (ninguno)
cli-status-word-configured = configurado
cli-status-word-not-configured = no configurado
cli-status-channel-not-compiled = 🚫 configurado, no compilado
cli-desktop-not-installed = La aplicación complementaria de ZeroClaw no está instalada.
cli-desktop-blurb1 = La aplicación complementaria es una ligera app de la barra de menú que
cli-desktop-blurb2 = se conecta a la misma puerta de enlace que la CLI.
cli-config-all-configured = Todas las secciones ya están configuradas.
cli-config-schema-current = La configuración ya está en la versión actual del esquema.
cli-config-applied-ops = Se aplicaron {$count} operación(es):
cli-plugins-none = No hay complementos instalados.
cli-plugins-installed = Complementos instalados:
cli-plugin-search-none = No hay complementos que coincidan con '{$query}'.
cli-plugin-search-results = Complementos que coinciden con '{$query}' ({$count}):
cli-plugin-search-result = {$name} v{$version} — {$description}
cli-plugin-no-description = (sin descripción)
cli-plugin-install-resolving = Resolviendo '{$source}' desde el registro de complementos...
cli-plugin-installed-from = Complemento instalado desde {$source}
cli-plugin-installed-name-version = Complemento instalado {$name} v{$version}
cli-plugin-config-entry-seeded = Se creó [[plugins.entries]] para '{$name}'. Establece los valores de configuración del plugin con `zeroclaw config set plugins.entries.{$name}.config.<key>`.
cli-plugin-config-entry-seed-skipped = advertencia: se omitió crear la entrada de configuración para '{$name}': la sección [plugins] en disco está mal formada. Repárala, agrega un bloque [[plugins.entries]] con `name = "{$name}"`, y luego establece valores con `zeroclaw config set plugins.entries.{$name}.config.<key>`.
cli-plugin-config-entry-seed-unaddressable = advertencia: se omitió crear la entrada de configuración para '{$name}': los nombres de plugin que contienen '.' no se pueden direccionar mediante rutas de configuración con puntos (`config set` divide por '.'). Agrega a mano un bloque [[plugins.entries]] con `name = "{$name}"` al archivo de configuración.
cli-config-section-degraded = advertencia: la sección de configuración `{$section}` en {$path} está mal formada y se restableció a los valores predeterminados para esta ejecución. Los valores de esa sección NO están en efecto. Ejecuta `zeroclaw config migrate` para ver el error de análisis y luego repara el archivo.
cli-plugin-removed = Complemento '{$name}' eliminado.
cli-plugin-not-found = No se encontró el complemento '{$name}'.
cli-plugin-legacy-detected = Nota: los complementos en una ubicación heredada ({$path}) no se cargan en el agente. Ejecuta `zeroclaw plugin migrate` para moverlos a {$target}.
cli-plugin-migrated = Se movieron {$count} complemento(s) de {$path} a {$target}.
cli-plugin-migrate-none = No hay nada que migrar.
cli-estop-resume-done = Reanudación de la parada de emergencia completada.
cli-estop-engaged = Parada de emergencia activada.
cli-estop-status = Estado de la parada de emergencia:
cli-auth-none = No hay perfiles de autenticación configurados.
cli-auth-active = Perfiles activos:
cli-warn-crypto-provider = Advertencia: No se pudo instalar el proveedor de cifrado predeterminado: {$err}
cli-error-label = {"   "}Error: {$err}
cli-warn-cost-usage = {"  "}⚠ No se pudo cargar el uso de costos: {$err}
cli-warn-cost-tracker = {"  "}⚠ No se pudo inicializar el rastreador de costos: {$err}
cli-desktop-download-at = {"  "}Descárgala en: {$url}
cli-config-legend = Leyenda: 💉 anulado por entorno  🔒 secreto
cli-config-secret-set = {$path} está establecido (secreto cifrado — valor no mostrado)
cli-config-secret-unset = {$path} no está establecido (secreto cifrado)
cli-config-updated = {$path} actualizado.
cli-config-review-hint = Ejecuta `zeroclaw config list` para revisar y luego establece los campos requeridos.
cli-config-backed-up = Copia de seguridad en {$path}
cli-plugin-name-version = Plugin: {$name} v{$version}
cli-plugin-description = Descripción: {$desc}
cli-plugin-capabilities = Capacidades: {$v}
cli-plugin-permissions = Permisos: {$v}
cli-plugin-wasm = WASM: {$path}
cli-plugin-wasm-none = WASM: (plugin solo de skill)
cli-estop-domains-none = {"  "}domain_blocks:  (ninguno)
cli-estop-domains = {"  "}domain_blocks:  {$v}
cli-estop-tools-none = {"  "}tool_freeze:    (ninguno)
cli-estop-tools = {"  "}tool_freeze:    {$v}
cli-estop-updated-at = {"  "}updated_at:     {$v}
cli-auth-saved = Perfil guardado {$profile}
cli-auth-active-for = Perfil activo para {$provider}: {$profile}
cli-auth-refresh-ok = ✓ Actualización de token correcta (perfil {$profile})
cli-auth-removed = Perfil de autenticación eliminado {$provider}:{$profile}
cli-auth-not-found = Perfil de autenticación no encontrado: {$provider}:{$profile}
cli-auth-xai-imported = Perfil de autenticación de xAI importado desde {$path}
cli-auth-xai-device-code-started = Inicio de sesión con código de dispositivo de xAI iniciado.
cli-auth-oauth-visit = Visita: {$uri}
cli-auth-oauth-code = Código:  {$code}
cli-auth-oauth-fast-link = Enlace rápido: {$uri}
cli-auth-xai-open-oauth-url = Abre esta URL OAuth de xAI en tu navegador y autoriza el acceso:
cli-auth-callback-capture-failed = No se pudo capturar la devolución de llamada: {$error}
cli-auth-run-paste-redirect = Ejecuta `zeroclaw auth paste-redirect --model-provider {$provider} --profile {$profile}`
cli-auth-xai-no-pending-login = No se encontró un inicio de sesión pendiente de xAI. Ejecuta `zeroclaw auth login --model-provider xai` primero.
cli-auth-paste-redirect-requires-input = paste-redirect requiere la URL de redirección o el código OAuth
cli-locales-fetched = {"  "}descargado {$name} -> {$path}
cli-locales-skipped = {"  "}omitido {$name}: no está en upstream ({$path}; se intentó {$refs})
cli-locales-installed = Se instalaron {$count} catálogo(s) para '{$locale}' en {$dir}
cli-browse-header = {$path} ({$count} entradas)
cli-browse-empty = (vacío)
cli-browse-file-bytes = {$name} ({$bytes} bytes)
cli-hardware-feature-required = El descubrimiento de hardware requiere la característica 'hardware'.
cli-hardware-feature-build = Compila con: cargo build --features hardware
cli-hardware-unsupported-platform = El descubrimiento de USB por hardware no es compatible con esta plataforma.
cli-hardware-supported-platforms = Plataformas compatibles: Linux, macOS, Windows.
cli-update-already-current = Ya está actualizado (v{$version}).
cli-update-success = ¡Actualizado correctamente a v{$version}!
cli-update-prebuilt-channel-note = Las actualizaciones precompiladas usan la distribución estándar ligera. Compila desde el código fuente con `./install.sh --source --preset full`, `--features channels-full` o una característica `channel-*` específica para Slack y otros canales no incluidos en esa distribución.
cli-update-available = Actualización disponible: v{$current} -> v{$latest}
cli-update-forcing-reinstall = Forzando la reinstalación: v{$current} -> v{$latest}
cli-update-not-writable = el directorio de instalación {$dir} no admite escritura ({$error}); vuelve a ejecutar `zeroclaw update` con privilegios elevados (sudo en macOS/Linux, una consola de administrador en Windows)
cli-selftest-all-passed = Las {$total} comprobaciones pasaron.
cli-selftest-some-failed = {$failed}/{$total} comprobaciones fallaron.
cli-selftest-channel-config-uncompiled = {$compiled} tipos de canal compilados, {$configured} compilados/configurados; configurados pero no compilados: {$names}. Compila desde el código fuente con `./install.sh --source --preset full`, `--features channels-full` o la característica `channel-*` específica.
cli-channels-header = Canales:
cli-channels-cli-always = {"  "}✅ CLI (siempre disponible)
cli-channels-notion = {"  "}{$status} Notion
cli-channels-not-compiled-header = {"  "}Configurados pero no compilados en este binario:
cli-channels-not-compiled-entry = {"  "}🚫 {$name} (configurado, no compilado)
cli-channels-build-hint = {"  "}Compila desde el código fuente con `./install.sh --source --preset full`, `--features channels-full` o la característica `channel-*` específica.
cli-channels-start-hint = Para iniciar canales: zeroclaw channel start
cli-channels-doctor-hint = Para comprobar el estado:    zeroclaw channel doctor
cli-channels-configure-hint = Para configurar:      zeroclaw config set channels.<name>.<field>=<value>
cli-models-set-ok = Modelo predeterminado establecido en "{ $model }" en { $provider }.
cli-models-status-current = Modelo predeterminado: { $model } (proveedor: { $provider })
cli-models-status-none = No hay ningún modelo predeterminado configurado.
turn-interrupted-by-user = [interrumpido por el usuario]
turn-cancelled-client-rpc = [turno cancelado mediante el cliente]
turn-stream-interrupted = [transmisión interrumpida]
turn-model-fallback-notice = ⚡ { $requested_model } ({ $requested_provider }) no estaba disponible; esta respuesta fue generada por { $actual_model } ({ $actual_provider }).
history-trim-breadcrumb = [earlier turns omitted to fit the context window]
history-trim-reason-budget = context token budget exceeded
history-trim-reason-message-cap = límite de mensajes del historial superado
history-trim-floor-exceeds-budget = system prompt and tool definitions ({$floor} tokens) alone meet or exceed the context budget ({$budget} tokens); raise [runtime_profiles.<name>] max_context_tokens or reduce the tool surface by disabling unused integrations
turn-ingress-dropped = Esta solicitud no se procesó: { $reason }
turn-tool-interrupted-before-result = [interrumpido por el usuario antes de que esta herramienta produjera un resultado]
channel-runtime-malformed-tool-output = Generé un error de formato interno en la llamada de herramienta y no pude completar esta solicitud. Inténtalo de nuevo.
channel-runtime-new-session = Historial de conversación borrado. Empezando de nuevo.
channel-runtime-stop-sent = Señal de detención enviada.
channel-runtime-stop-no-task = No hay una tarea en curso para este ámbito de remitente.
channel-runtime-model-empty = El ID del modelo no puede estar vacío. Usa `/model <model-id>`.
channel-runtime-model-switched = Modelo cambiado a `{ $model }` (model_provider: `{ $provider }`). Contexto conservado.
channel-runtime-agent-scope-rejected = El remitente `{ $sender }` no está autorizado para `/model --agent` en el agente `{ $agent }`. Usa `/model --user { $model }` para una anulación solo de la sesión, o pide a un administrador que marque un grupo de pares con `admin_for_agent_scope = true` contigo como miembro.
channel-runtime-request-timeout = ⚠️ La solicitud agotó el tiempo de espera mientras se esperaba al modelo. Inténtalo de nuevo.
channel-runtime-current-model-status =
    model_provider actual: `{ $provider }`
    Modelo actual: `{ $model }`
channel-runtime-model-switch-hint = Cambia el modelo con `/model <model-id>` o `/model <hint>`.
channel-runtime-provider-switch-hint = Cambia el model_provider con `/models <model_provider>`.
channel-runtime-available-providers-header = model_providers disponibles:
channel-runtime-configured-routes-header = Rutas de modelo configuradas:
channel-runtime-no-cached-models = No se encontró una lista de modelos en caché para `{ $provider }`. Pide al operador que ejecute `zeroclaw models refresh --model-provider { $provider }`.
channel-runtime-cached-model-ids-header = IDs de modelo en caché (primeros { $count }):
channel-runtime-config-switch-hints =
    Usa `/models <model_provider>` para cambiar el model_provider.
    Usa `/model <model-id>` para cambiar el modelo.
channel-runtime-config-block-title =
    { "*" }Configuración del modelo{ "*" }
    Actual: `{ $provider }` / `{ $model }`
channel-runtime-config-select-provider-placeholder = Seleccionar model_provider
channel-runtime-config-select-model-placeholder = Seleccionar modelo
channel-runtime-config-provider-label = *ModelProvider*
channel-runtime-config-model-label = *Modelo*
channel-runtime-scope-user = usuario
channel-runtime-scope-agent = agente
channel-runtime-scope-overrides-summary =
    { "**" }Sobrescrituras de modelo{ "**" } (solo sesión; precedencia user > agent > session > default):
    • user: { $user }
    • agent: { $agent }
    • session (este chat): { $session }
    • default (config): { $default }
    Define un ámbito con `/model --user|--agent <model-id>`; bórralo configurándolo de nuevo al valor predeterminado.
channel-runtime-set-provider-switched =
    ModelProvider cambiado a `{ $provider }` para esta sesión del remitente. El modelo actual es `{ $model }`.
    Usa `/model <model-id>` para definir un modelo compatible con el provider.
channel-runtime-set-provider-init-failed =
    Error al inicializar model_provider `{ $provider }`. La ruta no cambió.
    Detalles: { $error }
channel-runtime-provider-ambiguous = ModelProvider `{ $family }` tiene varios alias configurados. Especifica cuál con `/models { $family }.<alias>`: { $list }
channel-runtime-provider-no-alias = No hay una entrada de provider configurada para `{ $provider }`. Agrega `[providers.models.{ $provider }]` (con api_key/uri) o selecciona un provider configurado; `/models` lista los válidos.
channel-runtime-provider-unknown = model_provider desconocido `{ $provider }`. Usa `/models` para listar model_providers válidos.
channel-runtime-scoped-model-empty = El ID del modelo no puede estar vacío. Usa `/model --user|--agent <model-id>`.
channel-runtime-scoped-model-switched = Modelo definido en `{ $model }` (model_provider: `{ $provider }`) para el ámbito **{ $scope }**. Solo para la sesión; se restablece al reiniciar.
channel-runtime-shadow-note = ⚠️ Hay una sobrescritura de mayor precedencia activa, por lo que los mensajes usarán `{ $model }` (`{ $provider }`) en su lugar; consulta `/model`.
channel-runtime-thinking-set =
    Thinking definido en `{ $level }` para esta sesión del remitente.
    Usa `/thinking reset` para volver al valor predeterminado del agente.
channel-runtime-thinking-cleared = Sobrescritura de thinking borrada. Usando el valor predeterminado del agente `{ $default }` para esta sesión del remitente.
channel-runtime-thinking-default =
    Thinking ya está usando el valor predeterminado del agente `{ $default }` para esta sesión del remitente.
    Usa `/thinking high`, `/thinking max` o `/thinking off` para sobrescribirlo.
channel-runtime-thinking-invalid = Nivel de thinking desconocido `{ $raw }`. Usa `/thinking off|minimal|low|medium|high|max`, `/thinking on` o `/thinking reset`.
channel-runtime-provider-turn-init-failed =
    ⚠️ Error al inicializar model_provider `{ $provider }`. Ejecuta `/models` para elegir otro model_provider.
    Detalles: { $error }
channel-runtime-fallback-footer =
    ⚡ `{ $requested }` no está disponible — respuesta de **{ $actual }** (`{ $model }`)
    Cambiar modelo: /models
cli-alias-list-empty = (sin entradas en {$section})
cli-alias-created = creado {$section}.{$alias}
cli-alias-exists = {$section}.{$alias} ya existe (sin cambios)
cli-alias-impact-scrub-header = la eliminación de {$section}.{$alias} depuraría {$count} referencia(s):
cli-alias-impact-blocked-header = la eliminación de {$section}.{$alias} está BLOQUEADA por {$count} referencia(s) fuerte(s):
cli-alias-impact-blocker = ✗ {$path} (referencia fuerte)
cli-alias-impact-scrub = • {$path} (se depuraría)
cli-alias-no-changes = No se realizaron cambios. Vuelva a ejecutar con --yes para aplicar (o --dry-run para previsualizar).
cli-alias-warn-workspace-archive = advertencia: falló el archivado del workspace: {$error}
cli-alias-owned-cascaded = estado propio en cascada: memoria {$memory} · cron {$cron} · acp {$acp} · sesiones {$sessions} → {$archive}
cli-alias-owned-repointed = estado propio reapuntado: memoria {$memory} · cron {$cron} · acp {$acp} · sesiones {$sessions}
cli-alias-warn-workspace-move = advertencia: falló el movimiento del workspace: {$error}
cli-alias-warn = advertencia: {$warning}
cli-alias-deleted = eliminado {$section}.{$alias} (depuradas {$count} referencia(s))
cli-alias-delete-refused-header = rechazado: {$count} referencia(s) fuerte(s) bloquean la eliminación:
cli-alias-delete-refused-hint = eliminación rechazada — resuelva primero las referencias fuertes
cli-alias-not-configured = {$path} no está configurado
cli-alias-delete-failed = error al eliminar: {$error}
cli-alias-delete-reserved-default = el agente `default` está reservado y no se puede eliminar
cli-alias-create-reserved-default = el agente `default` está reservado y no se puede crear
cli-alias-renamed = renombrado {$section}.{$from} → {$section}.{$to} (se reescribieron {$count} ruta(s) de referencia)
cli-alias-rename-invalid = nuevo alias no válido: {$message}
cli-alias-rename-reserved = el alias `{$alias}` está reservado y no se puede renombrar
cli-alias-rename-postcondition = falló la poscondición de la cascada de renombrado: {$message}
cli-alias-unknown-provider-category = categoría de proveedor desconocida `{$category}` (se esperaba models | tts | transcription)
cli-alias-no-such-section = no existe la sección de configuración: {$section}
cli-alias-live-acp-sessions = {$count} sesión(es) ACP activa(s) para `{$alias}` — finalícelas primero
cli-alias-owned-state-unavailable = nota: las referencias de configuración se actualizaron, pero el estado propio del agente (filas de memoria, directorio de workspace, filas de cron/acp/sesión) AÚN NO fue propagado en cascada por esta CLI — use la API del gateway para la cascada completa del estado propio.
cli-bundle-not-configured = el skill bundle '{$alias}' no está configurado
cli-bundle-rename-failed = error al renombrar: {$error}
cli-bundle-exists = el skill bundle '{$alias}' ya existe (sin cambios)
cli-bundle-created = creado skill_bundles.{$alias} (dir: {$dir})
cli-bundle-created-warn = creado skill_bundles.{$alias} (advertencia: falló la resolución del dir: {$error})
cli-bundle-impact-header = la eliminación de skill_bundles.{$alias} lo quitaría de {$count} referencia(s) de agente:
cli-bundle-no-changes = No se realizaron cambios. Vuelva a ejecutar con --yes para aplicar.
cli-bundle-archived = directorio del bundle archivado → {$path}
cli-bundle-warn-archive = advertencia: falló el archivado del directorio del bundle: {$error}
cli-bundle-deleted = eliminado skill_bundles.{$alias} (eliminado de {$count} agente(s))
cli-bundle-warn-move = advertencia: falló el movimiento del directorio del bundle: {$error}
cli-bundle-renamed = renombrado skill_bundles.{$from} → skill_bundles.{$to}

# ── Sugerencias de reinicio del panel web — RestartInfo.hint mostrado tras una actualización en la app (PR #8173) ──
# Las primeras cuatro son plantillas de comando de shell que se muestran tal cual; no se traducen.
cli-gateway-restart-hint-kubernetes = kubectl rollout restart deployment/zeroclaw
cli-gateway-restart-hint-container = docker compose restart
cli-gateway-restart-hint-systemd = systemctl restart zeroclaw
cli-gateway-restart-hint-launchd = launchctl kickstart -k <your-zeroclaw-label>
cli-gateway-restart-hint-process = reinicie el proceso `zeroclaw daemon`

cli-daemon-gateway-already-running = Ya hay un gateway de ZeroClaw ejecutándose en {$host}:{$port}. El daemon supervisa su propio gateway y no iniciará un segundo en la misma dirección. Detén ese gateway (o apunta el daemon a un puerto libre con `zeroclaw config set gateway.port <port>`) y luego vuelve a ejecutar el daemon.
cli-daemon-gateway-port-occupied = La dirección del gateway {$host}:{$port} ya está en uso por otro proceso. Libera el puerto o apunta el daemon a un puerto libre (`zeroclaw config set gateway.port <port>`) y luego vuelve a ejecutar el daemon.
cli-agent-context-bar = ctx: {$used} / {$max}  {$bar}  {$pct}%
cli-agent-context-bar-unknown = ctx: desconocido / {$max}
cli-doctor-ctxwin-already-set = {$provider_ref}: ya tiene context_window = {$ctx}
cli-doctor-ctxwin-no-model = {$provider_ref}: no hay modelo configurado, omitiendo
cli-doctor-ctxwin-would-set = {$provider_ref}: establecería context_window = {$ctx} (simulación)
cli-doctor-ctxwin-set = {$provider_ref}: context_window establecido = {$ctx}
cli-doctor-ctxwin-not-found = {$provider_ref}: no se encontró la entrada para actualizar
cli-doctor-ctxwin-fetch-failed = {$provider_ref}: el proveedor no expone ventana de contexto o falló la obtención
cli-doctor-ctxwin-saved = Guardados {$updated} cambios en config.toml
cli-doctor-ctxwin-dry-run = Simulación completa — sin cambios. Ejecute sin --dry-run para aplicar.
cli-doctor-ctxwin-none = No se necesitan actualizaciones.
cli-doctor-ctxwin-write-failed = {$provider_ref}: error al escribir context_window: {$error}

# ── Degraded config sections (doctor diagnose, #8835) ──
cli-doctor-degraded-security = Sección de configuración CRÍTICA PARA LA SEGURIDAD `{$path}` no es válida y se restableció a sus valores predeterminados para que el daemon pueda arrancar; la postura en ejecución puede ser MÁS DÉBIL de lo previsto. Ejecute `zeroclaw config migrate` para ver el error de análisis y luego repare el archivo.
cli-doctor-degraded-section = La sección de configuración `{$path}` está mal formada y se restableció a sus valores predeterminados; los valores de esa sección NO están en efecto. Ejecute `zeroclaw config migrate` para ver el error de análisis y luego repare el archivo.
sop-approval-deferred-at-capacity = No se pudo reanudar la ejecución {$run_id}: los cupos de ejecución están llenos. La aprobación sigue en espera; inténtalo de nuevo cuando se libere un cupo.
sop-approval-policy-unavailable = La aprobación falló porque el paso de SOP en espera no está disponible: {$reason}. La ejecución sigue en espera.
sop-rpc-decision-invalid-state = La ejecución {$run_id} no se puede resolver en su estado actual.
sop-rpc-decision-unauthorized = La identidad RPC no está autorizada para resolver este paso de SOP.
sop-rpc-policy-missing = La política de aprobación de SOP '{$name}' no está configurada.
sop-rpc-policy-unavailable = La política del SOP en espera no está disponible: {$reason}.
