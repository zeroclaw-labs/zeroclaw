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
cli-skills-install-about = Instalar una nueva skill desde una URL o ruta local
cli-skills-remove-about = Eliminar una skill instalada
cli-skills-test-about = Ejecutar la validación TEST.sh para una skill (o todas las skills)
cli-skills-install-start = Instalando skill desde: {$source}
cli-skills-install-resolving-registry = { "  " }Resolviendo '{$source}' desde el registro de skills...
cli-skills-install-installed-audited = { "  " }{$status} Skill instalada y auditada: {$path} ({$files} archivos escaneados)
cli-skills-install-security-audit-completed = { "  " }Auditoría de seguridad completada con éxito.
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
cli-auth-login-about = Iniciar sesión con OAuth (OpenAI Codex o Gemini)
cli-auth-refresh-about = Actualizar el token de acceso de OpenAI Codex usando el token de actualización
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
channel-discord-delivery-failure-note-one = (nota: no pude entregar {$count} archivo.)
channel-discord-delivery-failure-note-many = (nota: no pude entregar {$count} archivos.)
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
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = no establecido (usando detección automática)
cli-self-test-web-dist-dir-pass-literal = {$path} (ruta literal)
cli-self-test-web-dist-dir-fail-expansion = ADVERTENCIA: {$path} — {$reason}; gateway.web_dist_dir se lee literalmente, así que expande el valor tú mismo (p. ej., una ruta absoluta)
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
cli-skills-tags = Etiquetas:  {$tags}
cli-sop-none = No se encontraron SOP.
cli-sop-create-hint = {"  "}Cree uno: mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}luego agregue SOP.toml y SOP.md
cli-sop-loaded-header = SOP cargados ({$count}):
cli-sop-none-to-validate = No se encontraron SOP para validar.
cli-sop-valid = ✅ {$name} — válido
cli-sop-warnings = ⚠️  {$name} — {$count} advertencia(s):
cli-sop-all-passed = Todos los SOP pasaron la validación.
cli-sop-priority = {"  "}Prioridad:       {$value}
cli-sop-execution-mode = {"  "}Modo de ejecución: {$value}
cli-sop-deterministic = {"  "}Determinista:  {$value}
cli-sop-cooldown = {"  "}Tiempo de espera: {$value}s
cli-sop-max-concurrent = {"  "}Máx. concurrentes: {$value}
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
cli-quickstart-cancelled = Quickstart cancelado. No se escribió ninguna configuración.
cli-quickstart-incomplete = {"  "}Aún no se han completado todos los selectores.
cli-no-channels-compiled = {"  "}No hay tipos de canal compilados en este binario.
cli-quickstart-complete = Quickstart completado. Se creó el agente `{$alias}`.
cli-next-steps = Siguientes pasos:
cli-agent-not-created = Tu agente no fue creado — y no se cambió nada en el disco.
cli-onboard-deprecated = `zeroclaw onboard` está obsoleto — usa `zeroclaw quickstart`.
cli-otp-initialized = Secreto OTP inicializado para ZeroClaw.
cli-otp-enrollment-uri = URI de inscripción: {$uri}
cli-pairing-enabled = 🔐 El emparejamiento del gateway está habilitado.
cli-pairing-use-code = {"  "}Usa este código de un solo uso para emparejar un nuevo dispositivo:
cli-pairing-post = {"    "}POST /pair con encabezado X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}Reinicia el gateway para generar un nuevo código de emparejamiento.
cli-pairing-disabled = ⚠️  El emparejamiento del gateway está deshabilitado en la configuración.
cli-gateway-running-q = {"   "}¿Está el gateway en ejecución? Inícialo con:
cli-status-title = 🦀 Estado de ZeroClaw
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
cli-status-agents = 🛡️  Agentes:        {$v}
cli-status-runtime = ⚙️  Entorno de ejecución:       {$v}
cli-status-security-noprofile = Seguridad ({$alias}): <sin risk_profile>
cli-status-security = Seguridad ({$alias}):
cli-status-workspace-only = {"  "}Solo espacio de trabajo:    {$v}
cli-status-max-actions = {"  "}Máx. acciones/hora:  {$v}
cli-status-max-cost-day = {"  "}Costo máx./día:      ${$v}
cli-status-max-cost-month = {"  "}Costo máx./mes:    ${$v}
cli-status-otp = {"  "}OTP habilitado:       {$v}
cli-status-estop = {"  "}Parada de emergencia activada:    {$v}
cli-status-boards = {"  "}Tableros:    {$v}
cli-desktop-not-installed = La aplicación complementaria de ZeroClaw no está instalada.
cli-desktop-blurb1 = La aplicación complementaria es una ligera app de la barra de menú que
cli-desktop-blurb2 = se conecta a la misma puerta de enlace que la CLI.
cli-config-all-configured = Todas las secciones ya están configuradas.
cli-config-schema-current = La configuración ya está en la versión actual del esquema.
cli-config-applied-ops = Se aplicaron {$count} operación(es):
cli-plugins-none = No hay complementos instalados.
cli-plugins-installed = Complementos instalados:
cli-plugin-installed-from = Complemento instalado desde {$source}
cli-plugin-removed = Complemento '{$name}' eliminado.
cli-plugin-not-found = No se encontró el complemento '{$name}'.
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
cli-selftest-all-passed = Las {$total} comprobaciones pasaron.
cli-selftest-some-failed = {$failed}/{$total} comprobaciones fallaron.
cli-channels-header = Canales:
cli-channels-cli-always = {"  "}✅ CLI (siempre disponible)
cli-channels-notion = {"  "}{$status} Notion
cli-channels-start-hint = Para iniciar canales: zeroclaw channel start
cli-channels-doctor-hint = Para comprobar el estado:    zeroclaw channel doctor
cli-channels-configure-hint = Para configurar:      zeroclaw config set channels.<name>.<field>=<value>
