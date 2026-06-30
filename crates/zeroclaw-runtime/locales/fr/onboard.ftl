# Flux d'intégration — chaînes indépendantes de l'interface.
#
# Servies à toutes les interfaces (CLI, RPC, web). Le flux transporte les
# identifiants de message et les arguments comme données ; la surface qui
# l'utilise les résout selon la locale active.

# Sélecteur de langue — la première étape du flux.
onboard-flow-locale-prompt = Choisissez une langue
onboard-flow-locale-confirmed = Langue définie sur {$label}.

# Résultats du parcours de section.
onboard-flow-completed = {$items} configuré.
onboard-flow-cancelled = Intégration annulée. Rien n'a été modifié.
onboard-flow-failed = Impossible de configurer {$layer}:{$instance} : {$reason}

# Erreurs.
onboard-flow-no-fields = La section {$section} n'a aucun champ configurable.
