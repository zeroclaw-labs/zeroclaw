# Flujo de incorporación — cadenas independientes de la interfaz.
#
# Se sirven a todas las interfaces (CLI, RPC, web). El flujo transporta los
# identificadores de mensaje y los argumentos como datos; la superficie que lo
# usa los resuelve según la configuración regional activa.

# Selector de idioma — el primer paso del flujo.
onboard-flow-locale-prompt = Elige un idioma
onboard-flow-locale-confirmed = Idioma establecido en {$label}.

# Resultados del recorrido de la sección.
onboard-flow-completed = {$items} configurado.
onboard-flow-cancelled = Incorporación cancelada. No se cambió nada.
onboard-flow-failed = No se pudo configurar {$layer}:{$instance}: {$reason}

# Errores.
onboard-flow-no-fields = La sección {$section} no tiene campos configurables.
