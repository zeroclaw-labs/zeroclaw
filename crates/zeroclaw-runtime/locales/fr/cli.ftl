cli-about = L'assistant IA le plus rapide et le plus léger.
cli-no-command-provided = Aucune commande fournie.
cli-try-quickstart = Essayez `zeroclaw quickstart` pour créer votre premier agent.
cli-quickstart-about = Créez votre premier agent de bout en bout
cli-agent-about = Démarrer la boucle de l'agent IA
cli-gateway-about = Gérer le serveur de passerelle (webhooks, websockets)
cli-acp-about = Démarrer le serveur ACP (JSON-RPC 2.0 sur stdio)
cli-daemon-about = Démarrer le daemon autonome à exécution longue
cli-service-about = Gérer le cycle de vie du service OS (service utilisateur launchd/systemd)
cli-doctor-about = Exécuter des diagnostics sur le daemon, le planificateur et l'actualisation des canaux
cli-status-about = Afficher l'état du système (détails complets)
cli-estop-about = Activer, inspecter et reprendre les états d'arrêt d'urgence
cli-cron-about = Configurer et gérer les tâches planifiées
cli-models-about = Gérer les catalogues de modèles des fournisseurs
cli-providers-about = Lister les fournisseurs d'IA pris en charge
cli-channel-about = Gérer les canaux de communication
cli-integrations-about = Parcourir plus de 50 intégrations
cli-skills-about = Gérer les compétences (capacités définies par l'utilisateur)
cli-sop-about = Gérer les procédures opérationnelles standard (SOP)
cli-migrate-about = Migrer les données depuis d'autres runtimes d'agents
cli-auth-about = Gérer les profils d'authentification des abonnements fournisseur
cli-hardware-about = Découvrir et analyser le matériel USB
cli-peripheral-about = Gérer les périphériques matériels
cli-memory-about = Gérer les entrées de mémoire de l'agent
cli-config-about = Gérer la configuration de ZeroClaw
cli-update-about = Vérifier et appliquer les mises à jour de ZeroClaw
cli-self-test-about = Exécuter les tests d'autodiagnostic
cli-completions-about = Générer des scripts d'achèvement de shell
cli-desktop-about = Lancer l'application de bureau companion ZeroClaw
cli-config-schema-about = Afficher le schéma JSON complet de la configuration sur stdout
cli-config-list-about = Lister toutes les propriétés de configuration avec leurs valeurs actuelles
cli-config-get-about = Obtenir la valeur d'une propriété de configuration
cli-config-set-about = Définir une propriété de configuration (les champs secrets demandent automatiquement une entrée masquée)
cli-config-init-about = Initialiser les sections non configurées avec les valeurs par défaut (enabled=false)
cli-config-migrate-about = Migrer config.toml vers la version actuelle du schéma sur le disque (conserve les commentaires)
cli-service-install-about = Installer l'unité de service daemon pour le démarrage automatique et la redémarrage
cli-service-start-about = Démarrer le service daemon
cli-service-stop-about = Arrêter le service daemon
cli-service-restart-about = Redémarrer le service daemon pour appliquer la dernière configuration
cli-service-status-about = Vérifier l'état du service daemon
cli-service-uninstall-about = Désinstaller l'unité de service daemon
cli-service-logs-about = Suivre les日志 du service daemon
cli-channel-list-about = Lister tous les canaux configurés
cli-channel-start-about = Démarrer tous les canaux configurés
cli-channel-doctor-about = Exécuter des vérifications de santé pour les canaux configurés
cli-channel-add-about = Ajouter une nouvelle configuration de canal
cli-channel-remove-about = Supprimer une configuration de canal
cli-channel-send-about = Envoyer un message ponctuel à un canal configuré
cli-wechat-pairing-required = 🔐 Appairage WeChat requis. Code de liaison à usage unique : {$code}
cli-wechat-send-bind-command = Envoyez `{$command} <code>` depuis votre WeChat.
cli-wechat-qr-login = 📱 Connexion QR WeChat ({$attempt}/{$max})
cli-wechat-scan-to-connect = Scannez avec WeChat pour vous connecter.
cli-wechat-qr-url = URL du QR : {$url}
cli-wechat-qr-expired-giving-up = Le code QR WeChat a expiré {$max} fois, abandon.
cli-wechat-qr-fetch-failed = Échec de la récupération du code QR WeChat.
cli-wechat-qr-fetch-status-failed = Échec de la récupération du code QR WeChat ({$status}) : {$body}
cli-wechat-missing-response-field = {$field} manquant dans la réponse WeChat.
cli-wechat-scanned-confirm = 👀 Scanné ! Confirmez sur votre téléphone...
cli-wechat-qr-expired-refreshing = ⏳ Code QR expiré, actualisation...
cli-wechat-login-confirmed-missing-field = Connexion confirmée mais {$field} manquant.
cli-wechat-connected = ✅ WeChat connecté !
cli-wechat-bound-success = ✅ Compte WeChat lié avec succès. Vous pouvez maintenant parler à ZeroClaw.
cli-wechat-invalid-bind-code = ❌ Code de liaison invalide. Veuillez réessayer.
cli-skills-list-about = Lister toutes les compétences installées
cli-skills-audit-about = Auditer un répertoire source de compétence ou une compétence installée
cli-skills-audit-failed = L'audit des compétences a échoué.
cli-skills-install-about = Installer une nouvelle compétence à partir d'une URL ou d'un chemin local
cli-skills-remove-about = Supprimer une compétence installée
cli-skills-test-about = Exécuter la validation TEST.sh pour une compétence (ou toutes les compétences)
cli-skills-review-summary = { "  " }💾 Revue de compétence : {$summary}
cli-skills-install-start = Installation du skill depuis : {$source}
cli-skills-install-resolving-registry = { "  " }Résolution de '{$source}' depuis le registre de skills...
cli-skills-install-resolving-extra-registry = { "  " }Résolution de '{$source}' depuis le registre '{$registry}'...
cli-skills-install-git-failed = échec de l'installation de la source de compétence git : {$source}
cli-skills-install-registry-failed = échec de l'installation de la compétence depuis le registre : {$source}
cli-skills-install-extra-registry-failed = échec de l'installation de la compétence depuis le registre supplémentaire : {$source}
cli-skills-install-local-failed = échec de l'installation de la source de compétence locale : {$source}
cli-skills-install-installed-audited = { "  " }{$status} Skill installé et audité : {$path} ({$files} fichiers analysés)
cli-skills-install-security-audit-completed = { "  " }Audit de sécurité terminé avec succès.
cli-skills-install-into-bundle = { "  " }Installé dans le lot '{$alias}'. Les agents qui répertorient ce lot dans skill_bundles le chargeront.
cli-skills-install-global-note = { "  " }Note : installé dans le répertoire global des compétences, qu'aucun agent ne charge automatiquement. Relancez avec --bundle <alias>, ou assignez un lot à un agent, pour le rendre chargeable.
cli-skills-removed-archived = { "  " }{$status} Compétence '{$name}' retirée du lot '{$bundle}' (archivée sous shared/skills/_deleted/).
cli-skills-removed-global = { "  " }{$status} Compétence '{$name}' retirée du répertoire global des compétences.
cli-skills-install-tier-official = Installation de {$name} v{$version} — Officiel (maintenu par zeroclaw-labs)
cli-skills-install-tier-community =
    Installation de {$name} v{$version} — Soumission communautaire
    Ce skill n'est pas audité par ZeroClaw. Examinez le contenu du skill
    et exécutez `zeroclaw skills audit {$name}` avant d'accorder des
    permissions ou de l'exécuter en production.
cli-skills-add-scaffolded = Skill {$target} échafaudé dans {$dir}
cli-skills-bundle-add-prompt =
    Pour créer le skill-bundle '{$alias}' avec le répertoire '{$dir}', exécutez :
    zeroclaw config map-key skill-bundles {$alias}
    zeroclaw config set skill-bundles.{$alias}.directory {$dir}

    (La création directe de bundle via `zeroclaw skills bundle add` dupliquerait la surface de mutation de configuration.)
cli-skills-bundle-remove-prompt =
    Pour supprimer le skill-bundle '{$alias}', exécutez :
    zeroclaw config map-key-delete skill-bundles {$alias}

    (Supprime l'entrée de configuration ; le répertoire du bundle sur le disque reste en place.)
cli-skills-bundle-list-empty =
    Aucun bundle de skills configuré.
    Créez-en un : zeroclaw config set skill-bundles.default.directory shared/skills/default
cli-skills-bundle-list-header = Bundles de skills ({$count}) :
cli-skills-bundle-entry = {$alias} -> {$dir}
cli-skills-bundle-include = inclure : {$values}
cli-skills-bundle-exclude = exclure : {$values}
cli-skills-bundle-show-no-skills = (aucun skill installé)
cli-skills-bundle-show-skills-header = skills ({$count}) :
cli-skills-bundle-show-skill = {$name} : {$description}
cli-cron-list-about = Lister toutes les tâches planifiées
cli-cron-add-about = Ajouter une nouvelle tâche planifiée récurrente
cli-cron-add-at-about = Ajouter une tâche unique qui se déclenche à un moment UTC spécifique
cli-cron-add-every-about = Ajouter une tâche qui se répète à un intervalle fixe
cli-cron-once-about = Ajouter une tâche unique qui se déclenche après un délai à partir de maintenant
cli-cron-remove-about = Supprimer une tâche planifiée
cli-cron-update-about = Mettre à jour un ou plusieurs champs d'une tâche planifiée existante
cli-cron-pause-about = Mettre en pause une tâche planifiée
cli-cron-resume-about = Reprendre une tâche en pause
cli-auth-login-about = Se connecter avec OAuth (OpenAI Codex, Gemini ou xAI)
cli-auth-refresh-about = Actualiser le jeton d'accès OAuth avec le jeton d'actualisation
cli-auth-logout-about = Supprimer le profil d'authentification
cli-auth-use-about = Définir le profil actif pour un fournisseur
cli-auth-list-about = Lister les profils d'authentification
cli-auth-status-about = Afficher le statut d'authentification avec le profil actif et les informations d'expiration du jeton
cli-memory-list-about = Lister les entrées de mémoire avec des filtres optionnels
cli-memory-get-about = Obtenir une entrée de mémoire spécifique par clé
cli-memory-stats-about = Afficher les statistiques et l'état de santé du backend mémoire
cli-memory-clear-about = Effacer les mémoires par catégorie, par clé, ou tout effacer
cli-memory-clear-unsupported-backend = memory clear n'est pas pris en charge pour le backend en ajout seul '{$backend}' ; passez à un backend supprimable (sqlite, lucid ou postgres)
cli-estop-status-about = Imprimer le statut actuel d'arrêt d'urgence
cli-estop-resume-about = Reprendre depuis un niveau d'arrêt d'urgence engagé
cli-models-refresh-about = Actualiser et mettre en cache les modèles du fournisseur
cli-models-list-about = Lister les modèles mis en cache pour un fournisseur
cli-models-set-about = Définir le modèle par défaut dans la configuration
cli-models-status-about = Afficher la configuration actuelle du modèle et l'état du cache
cli-doctor-models-about = Sonder les catalogues de modèles à travers les fournisseurs et signaler la disponibilité
cli-doctor-traces-about = Interroger les événements de trace d'exécution (diagnostics d'outils et réponses de modèle)
cli-doctor-update-context-windows-about = Mettre à jour context_window dans config.toml depuis les endpoints /models du fournisseur
cli-hardware-discover-about = Énumérer les dispositifs USB et afficher les cartes connues
cli-hardware-introspect-about = Inspecter un appareil par son numéro de série ou son chemin de dispositif
cli-hardware-info-about = Obtenir les informations de puce via USB en utilisant probe-rs via ST-Link
cli-peripheral-list-about = Lister les périphériques configurés
cli-peripheral-add-about = Ajouter un périphérique en fonction du type de carte et du chemin de transport
cli-peripheral-flash-about = Flasher le firmware de ZeroClaw sur une carte Arduino
cli-sop-list-about = Lister les SOP (Procédures Opérationnelles Standard) chargées
cli-sop-validate-about = Valider les définitions des SOP
cli-sop-show-about = Afficher les détails d'une SOP
cli-migrate-openclaw-about = Importer la mémoire d'un espace de travail OpenClaw vers cet espace de travail ZeroClaw
cli-agent-long-about =
    Démarrer la boucle de l'agent IA.

    Lance une session de chat interactive avec le fournisseur d'IA configuré. Utilisez --message pour des requêtes ponctuelles sans entrer en mode interactif.

    Exemples :
    zeroclaw agent                              # session interactive
    zeroclaw agent -m "Résumez les logs d'aujourd'hui"  # message unique
    zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
    zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
cli-gateway-long-about =
    Gérer le serveur gateway (webhooks, websockets).

    Démarrer, redémarrer ou inspecter la gateway HTTP/WebSocket qui accepte les événements webhook entrants et les connexions WebSocket.

    Exemples :
    zeroclaw gateway start              # démarrer la gateway
    zeroclaw gateway restart            # redémarrer la gateway
    zeroclaw gateway get-paircode       # afficher le code d'appairage
cli-acp-long-about =
    Démarrer le serveur ACP (JSON-RPC 2.0 sur stdio).

    Lance un serveur JSON-RPC 2.0 sur stdin/stdout pour l'intégration avec des IDE et des outils. Gère la session et diffuse les réponses de l'agent sous forme de notifications.

    Méthodes : initialize, session/new, session/prompt, session/stop.

    Exemples :
    zeroclaw acp                        # démarrer le serveur ACP
    zeroclaw acp --max-sessions 5       # limiter les sessions concurrently
cli-daemon-long-about =
    Démarrer le daemon autonome longue durée.

    Lance l'exécution Runtime complète de ZeroClaw : serveur gateway, tous les canaux configurés (Telegram, Discord, Slack, etc., moniteur de cœur et planificateur cron. C'est la méthode recommandée pour exécuter ZeroClaw en production ou comme assistant toujours actif.

    Utilisez 'zeroclaw service install' pour enregistrer le daemon en tant que service OS (systemd/launchd) pour un démarrage automatique au démarrage.

    Exemples :
    zeroclaw daemon                   # utiliser les défauts de configuration
    zeroclaw daemon -p 9090           # gateway sur le port 9090
    zeroclaw daemon --host 127.0.0.1  # uniquement localhost
cli-cron-long-about =
    Configurer et gérer les tâches planifiées.

    Programmez des tâches récurrentes, uniques ou basées sur des intervalles en utilisant des expressions cron, des horodatages RFC 3339, des durées ou des intervalles fixes.

    Les expressions cron utilisent le format standard à 5 champs : 'min heure jour mois jour_semaine'. Les fuseaux horaires sont par défaut UTC ; modifiez-les avec --tz et un nom de fuseau horaire IANA.

    Exemples :
    zeroclaw cron list
    zeroclaw cron add '0 9 * * 1-5' 'Bonjour' --tz America/New_York --agent
    zeroclaw cron add '*/30 * * * *' 'Vérifier la santé du système' --agent
    zeroclaw cron add '*/5 * * * *' 'echo ok'
    zeroclaw cron add-at 2025-01-15T14:00:00Z 'Envoyer un rappel' --agent
    zeroclaw cron add-every 60000 'Ping de santé'
    zeroclaw cron once 30m 'Lancer une sauvegarde dans 30 minutes' --agent
    zeroclaw cron pause IDENTIFIANT_TACHE
    zeroclaw cron update IDENTIFIANT_TACHE --expression '0 8 * * *' --tz Europe/London
cli-channel-long-about =
    Gérer les canaux de communication.

    Ajouter, supprimer, lister, envoyer et vérifier la santé des canaux qui connectent ZeroClaw aux plateformes de messagerie. Types de canaux pris en charge : telegram, discord, slack, whatsapp, matrix, imessage, email.

    Exemples :
    zeroclaw channel list
    zeroclaw channel doctor
    zeroclaw channel add telegram '{ "{" }"bot_token":"...","name":"my-bot"{ "}" }'
    zeroclaw channel remove my-bot
    zeroclaw channel bind-telegram zeroclaw_user
    zeroclaw channel send 'Alerte !' --channel-id telegram --recipient 123456789
cli-hardware-long-about =
    Découvrir et inspecter le matériel USB.

    Énumérer les dispositifs USB connectés, identifier les cartes de développement connues (STM32 Nucleo, Arduino, ESP32), et récupérer les informations de puce via probe-rs / ST-Link.

    Exemples :
    zeroclaw hardware discover
    zeroclaw hardware introspect /dev/ttyACM0
    zeroclaw hardware info --chip STM32F401RETx
cli-peripheral-long-about =
    Gérer les périphériques matériels.

    Connecter, tester et diagnostiquer les appareils via des périphériques USB (UART, I²C, SPI, etc.). Prend en charge la connexion, la désconnexion, la détection, le diagnostic d'éventail et le débogage de protocoles.

    Exemples :
    zeroclaw peripheral connect nucleo-f401re:/dev/ttyACM0
    zeroclaw peripheral disconnect nucleo-f401re
    zeroclaw peripheral detect nucleo-f401re
    zeroclaw peripheral probe nucleo-f401re
    zeroclaw peripheral trace nucleo-f401re
    zeroclaw peripheral debug nucleo-f401re
    zeroclaw peripheral connect esp32-usb-serial:/dev/ttyUSB0
    zeroclaw peripheral disconnect esp32-usb-serial
cli-memory-long-about =
    Gérer les entrées de mémoire de l'agent.

    Lister, inspecter et effacer les entrées de mémoire stockées en utilisant des stratégies par défaut. La mémoire persiste à travers les sessions et peut être organisée par catégorie, type ou clés arbitraires.

    Exemples :
    zeroclaw memory list
    zeroclaw memory get my_key
    zeroclaw memory clear

    La complétion par tabulation est automatiquement incluse dans les sous-commandes de complétion.
cli-config-long-about =
    Gérer la configuration de ZeroClaw.

    Afficher, définir ou initialiser les propriétés de la configuration par chemin ponctué. Utilisez 'schema' pour.dumping le schéma JSON complet pour le fichier de configuration.

    Les propriétés sont adressées par chemin ponctué (par ex. channels.matrix.mention-only).
    Les champs secrets (clés API, jetons) utilisent automatiquement une entrée masquée.
    Les champs énumérables offrent une sélection interactive lorsque la valeur est omise.

    Exemples :
    zeroclaw config list                                  # lister toutes les propriétés
    zeroclaw config list --secrets                        # lister uniquement les secrets
    zeroclaw config list --filter channels.matrix         # filtrer par préfixe
    zeroclaw config get channels.matrix.mention-only      # obtenir une valeur
    zeroclaw config set channels.matrix.mention-only true # définir une valeur
    zeroclaw config set channels.matrix.access-token      # secret : entrée masquée
    zeroclaw config set channels.matrix.stream-mode       # enum : sélection interactive
    zeroclaw config init channels.matrix                  # initier la section par défaut
    zeroclaw config schema                                # imprimer le schéma JSON vers stdout
    zeroclaw config schema > schema.json

    La complétion par tabulation du chemin de propriété est incluse automatiquement dans `zeroclaw completions <shell>`.
cli-update-long-about =
    Vérifie et applique les mises à jour de ZeroClaw.

    Par défaut, télécharge et installe la dernière version avec un pipeline en 6 phases : pré-validation, téléchargement, sauvegarde, validation, remplacement et test de fumée. Rollback automatique en cas d'échec.

    Utilisez --check pour uniquement vérifier les mises à jour sans installer.
    Utilisez --force pour ignorer l'invite de confirmation.
    Utilisez --version pour cibler une version spécifique au lieu de la dernière.

    Exemples :
    zeroclaw update                      # télécharger et installer la dernière version
    zeroclaw update --check              # vérifier uniquement, ne pas installer
    zeroclaw update --force              # installer sans confirmation
    zeroclaw update --version 0.6.0      # installer une version spécifique
cli-self-test-long-about =
    Exécute les tests d'auto-diagnostic pour vérifier l'installation de ZeroClaw.

    Par défaut, exécutera l'ensemble complet des tests incluant les vérifications réseau (santé du pont, mémoire aller-retour). Utilisez --quick pour ignorer les vérifications réseau afin d'obtenir une validation hors ligne plus rapide.

    Exemples :
    zeroclaw self-test             # ensemble complet de tests
    zeroclaw self-test --quick     # tests rapides uniquement (pas de réseau)
cli-skills-install-suggestion =
    Il semble que cette requête nécessite le skill `{$name}`, mais il n'est pas installé.

    Capacité correspondante : {$matched}
    Étape suivante : Exécutez `{$install_command}` pour l'installer.
cli-plugin-install-suggestion =
    Il semble que cette requête nécessite le plugin `{$name}`, mais il n'est pas installé.

    Capacité correspondante : {$matched}
    Étape suivante : Exécutez `{$install_command}` pour l'installer.
cli-completions-long-about =
    Génère les scripts de complétion de shell pour `zeroclaw`.

    Le script est imprimé dans stdout afin de pouvoir être chargé directement :

    Exemples :
    source <(zeroclaw completions bash)
    zeroclaw completions zsh > ~/.zfunc/_zeroclaw
    zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish
cli-desktop-long-about =
    Lance l'application de bureau compagnon ZeroClaw.

    L'application compagnon est une application légère pour la barre de menu / zone de dénombrement du système qui se connecte au même pont que la CLI. Elle fournit un accès rapide au tableau de bord, à la supervision de l'état et à l'appairage des appareils.

    Utilisez --install pour télécharger l'application compagnon pré-construite pour votre plateforme.

    Exemples :
    zeroclaw desktop              # lancer l'application compagnon
    zeroclaw desktop --install    # télécharger et l'installer
channel-needs-quickstart-reply = Cet agent n'est pas encore entièrement configuré. L'opérateur doit exécuter Quickstart avant que je puisse répondre.
channel-whatsapp-web-feature-missing-warning = ⚠ WhatsApp Web est configuré mais la fonctionnalité 'whatsapp-web' n'est pas compilée.
channel-whatsapp-web-feature-missing-build = Compilez/exécutez avec : cargo build --features whatsapp-web
channel-whatsapp-web-feature-missing-install = Si installé dans le PATH, réinstallez avec : cargo install --path . --force --locked --features whatsapp-web
channel-whatsapp-web-feature-missing-error = Le canal WhatsApp Web nécessite la fonctionnalité 'whatsapp-web'. Activez-la avec : cargo build --features whatsapp-web (ou, si installé dans le PATH : cargo install --path . --force --locked --features whatsapp-web)
channel-wecom-ws-stream-bootstrap = Je m'en occupe, veuillez patienter.
channel-wecom-ws-stop-ack = Message en cours arrêté.
channel-wecom-ws-voice-unavailable = Je ne peux pas traiter les messages vocaux pour le moment {$emoji}
channel-wecom-ws-unsupported-message = Ce type de message n'est pas encore pris en charge.
channel-wecom-ws-welcome = Bonjour, bienvenue dans cette discussion avec moi {$emoji}
channel-wecom-ws-supplemental-message =
    {"["}Message complémentaire]
    {$extra}
channel-wecom-ws-group-allowlist-missing =
    La liste d'autorisation WeCom n'est pas configurée, donc ce bot n'accepte pas les messages de groupe.

    chatid du groupe : {$chatid}
    userid de l'expéditeur : {$userid}

    Ajoutez une entrée autorisée à {$allowed_groups_path} ou {$allowed_users_path}. Vous pouvez aussi temporairement la définir sur ["*"] pour les tests.
channel-wecom-ws-group-access-denied =
    Ce groupe n'est pas autorisé à utiliser ce bot.

    chatid du groupe : {$chatid}
    userid de l'expéditeur : {$userid}

    Demandez à un administrateur d'ajouter ce groupe à {$allowed_groups_path}, ou ajoutez votre userid à {$allowed_users_path}.
channel-wecom-ws-dm-allowlist-missing =
    La liste d'autorisation WeCom n'est pas configurée, donc ce bot n'accepte pas les messages.

    Votre userid : {$userid}

    Ajoutez une entrée autorisée à {$allowed_users_path}. Vous pouvez aussi temporairement la définir sur ["*"] pour les tests.
channel-wecom-ws-dm-access-denied =
    Vous n'êtes pas autorisé à utiliser ce bot.

    Votre userid : {$userid}

    Demandez à un administrateur d'ajouter votre userid à {$allowed_users_path}.
channel-discord-interaction-unauthorized = Vous n'êtes pas autorisé à utiliser cette commande ici.
channel-discord-interaction-malformed = Commande inconnue ou mal formée.
channel-discord-interaction-unavailable = Cette commande n'est plus disponible ou son entrée était vide.
channel-discord-component-expired = Ce bouton ou ce menu a expiré ou a déjà été utilisé.
channel-discord-approval-recorded = Votre décision a été enregistrée.
channel-discord-delivery-failure-note-one = (note : je n'ai pas pu livrer {$count} fichier.)
channel-discord-delivery-failure-note-many = (note : je n'ai pas pu livrer {$count} fichiers.)
channel-whatsapp-web-delivery-failure-note-one = (note : je n'ai pas pu livrer {$count} pièce jointe multimédia WhatsApp.)
channel-whatsapp-web-delivery-failure-note-many = (note : je n'ai pas pu livrer {$count} pièces jointes multimédias WhatsApp.)
channel-line-bind-success = ✅ Appairé ! Vous pouvez maintenant discuter.
channel-line-bind-invalid-code = ❌ Code invalide. Veuillez réessayer.
channel-line-bind-rate-limited = ⏳ Trop de tentatives. Réessayez dans { $secs }s.
onboard-openai-auth-note =
    Authentification OpenAI :
    • Clé API — accès API standard via platform.openai.com (sk-...)
    • Abonnement Codex — utilise votre compte ChatGPT Plus/Pro (aucune clé API requise)
onboard-openai-auth-prompt = Authentification
onboard-openai-auth-api-key = Clé API
onboard-openai-auth-codex = Abonnement Codex
onboard-openai-codex-followup =
    L'authentification par abonnement Codex utilise votre compte ChatGPT.
    Exécutez `zeroclaw auth login --provider openai-codex` pour vous authentifier avant de démarrer votre agent.
cli-web-dist-dir-reason-tilde = commence par `~` qui n'est pas développé
cli-web-dist-dir-reason-dollar = contient `$` qui n'est pas développé
cli-doctor-web-dist-dir-expansion-warning = gateway.web_dist_dir = "{$path}" — {$reason} ; gateway.web_dist_dir est lu tel quel, vous devez donc développer la valeur vous-même (p. ex. un chemin absolu)
cli-doctor-codex-auth-profile-no-slot = Des identifiants OpenAI Codex sont connectés, mais aucun slot de fournisseur de modèle ne les utilise. Définissez `requires_openai_auth = true` sur un slot de fournisseur OpenAI et pointez le `model_provider` d'un agent vers celui-ci, ou exécutez `zeroclaw quickstart`.
cli-doctor-codex-auth-slot-no-profile = Les slots OpenAI {$slots} définissent `requires_openai_auth = true`, mais aucun identifiant OpenAI Codex n'est connecté. Exécutez `zeroclaw auth login --provider openai-codex`.
cli-doctor-codex-auth-ok = Les identifiants OpenAI Codex sont connectés et référencés par un slot de fournisseur de modèle.
cli-doctor-systemd-linger-enabled = persistance utilisateur systemd activée
cli-doctor-systemd-linger-disabled = persistance utilisateur systemd désactivée ; le service utilisateur peut s'arrêter après la déconnexion. Activez-la avec : loginctl enable-linger {$user}
cli-doctor-systemd-linger-unknown = impossible de vérifier la persistance utilisateur systemd avec loginctl
cli-self-test-web-dist-dir-name = web_dist_dir
cli-self-test-web-dist-dir-pass-unset = non défini (détection automatique utilisée)
cli-self-test-web-dist-dir-pass-literal = {$path} (chemin littéral)
cli-self-test-web-dist-dir-fail-expansion = AVERTISSEMENT : {$path} — {$reason} ; gateway.web_dist_dir est lu tel quel, vous devez donc développer la valeur vous-même (p. ex. un chemin absolu)
cli-service-systemd-linger-disabled-warning = la persistance utilisateur systemd est désactivée. Le service utilisateur ZeroClaw peut s'arrêter après la déconnexion. Activez-la avec : loginctl enable-linger {$user}
cli-peripherals-none = Aucun périphérique configuré.
cli-peripherals-add-hint = Ajoutez-en un avec : zeroclaw peripheral add <board> <path>
cli-peripherals-add-example = {"  "}Exemple : zeroclaw peripheral add nucleo-f401re <serial-path>
cli-peripherals-config-hint = Ou ajoutez à config.toml :
cli-peripherals-configured = Périphériques configurés :
cli-peripherals-already-configured = La carte {$board} à {$path} est déjà configurée.
cli-peripherals-added = {$board} ajouté à {$path}. Redémarrez le démon pour appliquer.
cli-peripherals-flash-needs-hardware = Le flash Arduino nécessite la fonctionnalité « hardware ».
cli-peripherals-unoq-needs-hardware = La configuration Uno Q nécessite la fonctionnalité « hardware ».
cli-peripherals-nucleo-needs-hardware = Le flash Nucleo nécessite la fonctionnalité « hardware ».
cli-skills-none-installed = Aucune compétence installée.
cli-skills-create-hint = {"  "}Créez-en une : mkdir -p ~/.zeroclaw/workspace/skills/my-skill
cli-skills-install-hint = {"  "}Ou installez : zeroclaw skills install <source>
cli-skills-installed-header = Compétences installées ({$count}) :
cli-skills-list-group-bundle = lot : {$alias}
cli-skills-list-group-agent = chargé par l'agent '{$alias}'
cli-skills-list-group-global = global / open-skills / plugins (pas depuis un lot)
cli-skills-agent-not-configured = l'agent '{$alias}' n'est pas configuré
cli-skills-agent-multiple-bundles = l'agent '{$alias}' a plusieurs lots de compétences ({$bundles}) ; passez --bundle pour en choisir un
cli-skills-multiple-locations-bundle = la compétence '{$name}' existe à plusieurs emplacements ({$locations}) ; passez --bundle pour en choisir un
cli-skills-multiple-locations-path = la compétence '{$name}' existe à plusieurs emplacements ({$locations}) ; passez un chemin explicite pour lever l'ambiguïté
cli-skills-tags = Étiquettes :  {$tags}
cli-skills-skipped-header = Ignorées ({$count}) :
cli-skills-skipped-reason = {"    "}Raison : {$reason}
cli-skills-skipped-scripts-hint = {"    "}Définissez `skills.allow_scripts = true` dans votre configuration zeroclaw pour l'activer.
cli-sop-none = Aucun SOP trouvé.
cli-sop-pending-none = Aucune exécution SOP en attente d'approbation.
cli-sop-pending-header = Exécutions SOP en attente d'approbation :
cli-sop-pending-row = {"  "}{$run_id} [{$sop_name}] étape {$step}/{$total}
cli-sop-ws-invalid-approval = sop approval_response requiert run_id et une décision approve ou deny
cli-sop-ws-resolve-failed = échec de la résolution SOP : {$error}
cli-sop-ws-engine-lock-poisoned = verrou du moteur SOP empoisonné
cli-sop-ws-subsystem-disabled = sous-système SOP non activé
cli-sop-create-hint = {"  "}Créez-en un : mkdir -p <workspace>/sops/my-sop
cli-sop-create-hint-2 = {"              "}puis ajoutez SOP.toml et SOP.md
cli-sop-loaded-header = SOP chargés ({$count}) :
cli-sop-none-to-validate = Aucun SOP trouvé à valider.
cli-sop-valid = ✅ {$name} — valide
cli-sop-deleted = SOP supprimé : {$name}
cli-sop-warnings = ⚠️  {$name} — {$count} avertissement(s) :
cli-sop-all-passed = Tous les SOP ont réussi la validation.
cli-sop-priority = {"  "}Priorité :      {$value}
cli-sop-execution-mode = {"  "}Mode d'exécution : {$value}
cli-sop-deterministic = {"  "}Déterministe :  {$value}
cli-sop-cooldown = {"  "}Délai :         {$value}s
cli-sop-max-concurrent = {"  "}Max simultanés : {$value}
cli-sop-admission-policy = {"  "}Admission :     {$value}
cli-sop-max-pending-approvals = {"  "}Max en attente : {$value}
cli-sop-location = {"  "}Emplacement :   {$value}
cli-sop-triggers = {"  "}Déclencheurs :
cli-sop-steps = {"  "}Étapes :
cli-sop-step-tools = Outils : {$tools}
cli-memory-reindexing = Réindexation du backend mémoire...
cli-memory-none = Aucune entrée mémoire trouvée.
cli-memory-none-at-offset = Aucune entrée à la position {$offset} (total : {$total}).
cli-memory-next-page = Utilisez --offset {$offset} pour voir la page suivante.
cli-memory-key-not-found = Aucune entrée mémoire trouvée pour la clé : {$key}
cli-memory-prefix-matched = Le préfixe « {$key} » correspond à {$n} entrées :
cli-memory-narrow-prefix = Spécifiez un préfixe plus long pour affiner la correspondance.
cli-memory-key = Clé :       {$value}
cli-memory-category = Catégorie :  {$value}
cli-memory-timestamp = Horodatage : {$value}
cli-memory-session = Session :   {$value}
cli-memory-stats-header = Statistiques mémoire :
cli-memory-backend = {"  "}Backend :  {$value}
cli-memory-total = {"  "}Total :    {$value}
cli-memory-by-category = {"  "}Par catégorie :
cli-memory-none-to-clear = Aucune entrée à effacer.
cli-memory-found-in-scope = {$count} entrées trouvées dans « {$scope} ».
cli-memory-aborted = Abandonné.
cli-memory-deleted-key = Clé supprimée : {$key}
cli-cron-none = Aucune tâche planifiée pour l'instant.
cli-cron-usage = Utilisation :
cli-cron-jobs-header = 🕒 Tâches planifiées ({$count}) :
cli-cron-list-cmd = {"    "}cmd : {$cmd}
cli-cron-list-prompt = {"    "}invite : {$prompt}
cli-cron-added-agent = ✅ Tâche cron d'agent {$id} ajoutée
cli-cron-added = ✅ Tâche cron {$id} ajoutée
cli-cron-added-oneshot-agent = ✅ Tâche cron d'agent à exécution unique {$id} ajoutée
cli-cron-added-oneshot = ✅ Tâche cron à exécution unique {$id} ajoutée
cli-cron-added-interval-agent = ✅ Tâche cron d'agent par intervalle {$id} ajoutée
cli-cron-added-interval = ✅ Tâche cron par intervalle {$id} ajoutée
cli-cron-updated = ✅ Tâche cron {$id} mise à jour
cli-cron-update-no-field = Au moins un des paramètres --expression, --tz, --command, --name, --allowed-tool ou --uses-memory doit être fourni
cli-cron-removed = ✅ Tâche cron {$id} supprimée
cli-cron-paused = ⏸️  Tâche cron {$id} en pause
cli-cron-resumed = ▶️  Tâche cron {$id} reprise
cli-cron-expr = {"  "}Expr  : {$v}
cli-cron-expr2 = {"  "}Expr: {$v}
cli-cron-next = {"  "}Suivant : {$v}
cli-cron-next2 = {"  "}Suivant : {$v}
cli-cron-next3 = {"  "}Suivant  : {$v}
cli-cron-prompt = {"  "}Invite : {$v}
cli-cron-prompt3 = {"  "}Invite   : {$v}
cli-cron-cmd = {"  "}Cmd : {$v}
cli-cron-cmd3 = {"  "}Cmd      : {$v}
cli-cron-at = {"  "}À     : {$v}
cli-cron-at2 = {"  "}À   : {$v}
cli-cron-every = {"  "}Toutes(ms): {$v}
cli-no-command = Aucune commande fournie.
cli-press-enter = Appuyez sur Entrée pour quitter...
cli-quickstart-title = Quickstart — créez un agent fonctionnel de bout en bout.
cli-quickstart-needs-tty = Quickstart est interactif et nécessite un terminal sur stdin et stderr. Lancez-le depuis un shell interactif, ou utilisez `zeroclaw config set <path> <value>` pour une configuration headless.
cli-quickstart-cancelled = Quickstart annulé. Aucune configuration écrite.
cli-quickstart-incomplete = {"  "}Tous les sélecteurs ne sont pas encore renseignés.
cli-quickstart-create-agent = ── Créer un agent
cli-quickstart-create-agent-locked = ── Créer un agent (verrouillé — renseignez d'abord tous les sélecteurs)
cli-quickstart-open-selector-prompt = Ouvrez un sélecteur (Entrée) ou choisissez Créer. Échap pour quitter.
cli-quickstart-use-existing = Utiliser l'existant
cli-quickstart-create-new = Créer nouveau
cli-quickstart-model-provider-prompt = Fournisseur de modèle
cli-quickstart-pick-configured-provider = Choisissez un fournisseur configuré
cli-quickstart-row-model-provider = {$glyph} Fournisseur modèle — {$summary}
cli-quickstart-row-risk-profile = {$glyph} Profil de risque   — {$summary}
cli-quickstart-row-memory = {$glyph} Mémoire            — {$summary}
cli-quickstart-row-channels = {$glyph} Canaux (0..N)     — {$summary}
cli-quickstart-row-peer-groups = {$glyph} Groupes de pairs — {$summary}
cli-quickstart-row-agent-identity = {$glyph} Identité agent   — {$summary}
cli-quickstart-summary-not-yet-chosen = pas encore choisi
cli-quickstart-summary-not-yet-visited = pas encore visité
cli-quickstart-summary-not-yet-named = pas encore nommé
cli-quickstart-summary-provider-fresh = {$name} (alias : {$alias}, modèle : {$model})
cli-quickstart-summary-use-existing = utiliser l'existant {$reference}
cli-quickstart-summary-preset-fresh = preset : {$name}
cli-quickstart-summary-channels-none = aucun (discussion uniquement via `zeroclaw agent`)
cli-quickstart-summary-agent = alias : {$alias}, prompt système : {$chars} caractères, {$files} fichier(s) de personnalité
cli-quickstart-summary-peer-groups-none = aucun — les canaux n'acceptent aucun pair
cli-quickstart-channel-remove-row = {"  "}{$reference} (supprimer)
cli-quickstart-peer-group-row = {$channel} → {$name} ({$count} pairs)
cli-quickstart-provider-local-label = {$name} (local)
cli-quickstart-provider-type-prompt = Type de fournisseur
cli-quickstart-alias-for = Alias pour {$name}
cli-quickstart-openai-auth-mode-label = Authentification
cli-quickstart-openai-auth-mode-help = Choisissez `codex` pour utiliser un profil d'authentification par abonnement ChatGPT/Codex. Si vous êtes déjà connecté avec le CLI Codex, exécutez `zeroclaw auth login --model-provider openai-codex --import ~/.codex/auth.json` ; sinon exécutez `zeroclaw auth login --model-provider openai-codex`.
cli-quickstart-anthropic-auth-mode-label = Authentification
cli-quickstart-anthropic-auth-mode-help = Choisissez `api_key` pour une clé Anthropic Console, ou `setup_token` si vous exécutez `claude setup-token` pour Claude Max et collez le jeton généré.
cli-quickstart-anthropic-api-key-help = Collez une clé d'API Anthropic Console ou le jeton généré par `claude setup-token`.
cli-quickstart-auth-codex-prompt = Se connecter à OpenAI Codex avec votre compte ChatGPT maintenant ?
cli-quickstart-auth-codex-import-prompt = Connexion Codex existante trouvée (~/.codex/auth.json) — l'importer maintenant ?
cli-quickstart-auth-codex-skip-hint = {"  "}Terminez plus tard avec : zeroclaw auth login --model-provider openai-codex
cli-quickstart-auth-anthropic-prompt = Exécuter `claude setup-token` pour le fournisseur Anthropic `{$alias}` maintenant ?
cli-quickstart-auth-anthropic-token-prompt = Collez le jeton de `claude setup-token`
cli-quickstart-auth-anthropic-saved = {"  "}Jeton de configuration Claude enregistré pour anthropic.{$alias}
cli-quickstart-auth-anthropic-skip-hint = {"  "}Terminez plus tard avec : claude setup-token, puis zeroclaw config set providers.models.anthropic.{$alias}.api_key <token>
cli-quickstart-auth-failed = {"  "}La configuration de l'authentification ne s'est pas terminée : {$error}
cli-quickstart-model-field-missing-warning = AVERTISSEMENT : le schéma n'a produit aucun champ `model` pour `{$provider}` — saisie manuelle utilisée. Merci de le signaler.
cli-quickstart-model-id-for = ID de modèle pour {$name}
cli-quickstart-risk-profile-prompt = Profil de risque
cli-quickstart-memory-backend-prompt = Backend mémoire
cli-quickstart-add-channel = + Ajouter un canal
cli-quickstart-channels-done = Terminé (le sélecteur de canaux compte comme visité)
cli-quickstart-channels-prompt = Canaux (facultatif, 0..N)
cli-quickstart-channel-source-prompt = Source du canal
cli-quickstart-all-channels-bound = {"  "}Tous les canaux configurés sont déjà liés à un agent. Libérez-en un avec `zeroclaw config set agents.<alias>.channels ...` avant de le réutiliser ici.
cli-quickstart-pick-configured-channel = Choisir un canal configuré
cli-quickstart-channel-type-prompt = Type de canal
cli-quickstart-add-peer-group = + Ajouter un groupe de pairs
cli-quickstart-done = Terminé
cli-quickstart-peer-groups-prompt = Groupes de pairs (Entrée sur une ligne pour supprimer, + Ajouter pour créer)
cli-quickstart-channel-to-authorize-prompt = Canal à autoriser
cli-quickstart-external-peers-prompt = Pairs externes (séparés par virgules ou retours ligne, vide pour aucun)
cli-quickstart-agent-alias-prompt = Alias de l'agent
cli-quickstart-edit-system-prompt = Modifier le prompt système dans $EDITOR ? (vide pour ignorer)
cli-quickstart-personality-start-template = Commencer avec le modèle (ouvrir dans $EDITOR)
cli-quickstart-personality-start-current = Commencer avec le contenu actuel (ouvrir dans $EDITOR)
cli-quickstart-personality-start-scratch = Commencer de zéro (ouvrir dans $EDITOR)
cli-quickstart-personality-skip = Ignorer
cli-quickstart-esc-go-back = {" "}(Échap pour revenir)
cli-quickstart-esc-return-checklist = {" "}(Échap pour revenir à la liste)
cli-quickstart-personality-file-prompt = {$filename}{$position} — suite ?{$back_hint}
cli-quickstart-next-agent-command = {"  "}zeroclaw agent -a {$alias}  # discuter avec cet agent dans le terminal
cli-quickstart-fix-and-rerun = Votre configuration existante est inchangée. Corrigez ce qui suit puis relancez quickstart :
cli-quickstart-could-not-finish = quickstart n'a pas pu se terminer : {$count} problème(s) à corriger
cli-quickstart-pick-preset = Choisir un preset
cli-quickstart-pick-existing-prompt = Choisir un {$prompt} existant
cli-quickstart-pick-preset-prompt = Choisir un preset {$prompt}
cli-quickstart-step-model-provider = Fournisseur de modèle
cli-quickstart-step-risk-profile = Profil de risque
cli-quickstart-step-runtime-profile = Profil runtime
cli-quickstart-step-memory = Mémoire
cli-quickstart-step-channels = Canaux
cli-quickstart-step-peer-groups = Groupes de pairs
cli-quickstart-step-agent = Agent
cli-quickstart-error-internal-no-result = erreur interne : apply_into n'a renvoyé aucun résultat malgré l'absence d'erreurs de validation
cli-quickstart-error-completion-flag = impossible de basculer quickstart-completed : {$err}
cli-quickstart-error-persist-config = impossible de persister la configuration : {$err}
cli-quickstart-error-not-type-alias-ref = `{$reference}` n'est pas une référence `<type>.<alias>`
cli-quickstart-error-no-configured-path = aucun `{$path}` configuré
cli-quickstart-error-provider-required = le type de fournisseur, l'alias et le modèle sont requis
cli-quickstart-error-unknown-provider-type = type de fournisseur de modèle inconnu `{$provider}` — choisissez-en un dans la liste des fournisseurs
cli-quickstart-error-unknown-openai-auth-mode = mode d'authentification OpenAI inconnu `{$mode}` — choisissez `api_key` ou `codex`
cli-quickstart-error-unknown-anthropic-auth-mode = mode d'authentification Anthropic inconnu `{$mode}` — choisissez `api_key` ou `setup_token`
cli-quickstart-error-alias-exists = l'alias `{$alias}` existe déjà
cli-quickstart-error-no-profile = aucun profil `{$alias}` configuré
cli-quickstart-error-unknown-risk-preset = preset de risque inconnu `{$preset}`
cli-quickstart-error-unknown-runtime-preset = preset runtime inconnu `{$preset}`
cli-quickstart-error-channel-bound = le canal `{$reference}` est déjà lié à l'agent `{$owner}`
cli-quickstart-error-channel-required = le type de canal et l'alias sont requis
cli-quickstart-error-channel-field-not-advertised = le champ de canal `{$field}` n'est pas disponible dans Quickstart
cli-quickstart-error-channel-token-required = le jeton du bot Telegram est requis
cli-quickstart-error-peer-group-name-required = le nom du groupe de pairs est requis
cli-quickstart-error-peer-group-channel-required = la référence de canal du groupe de pairs est requise
cli-quickstart-error-peer-group-unknown-channel = le groupe de pairs `{$name}` référence un canal inconnu `{$channel}`
cli-quickstart-error-peer-group-exists = le groupe de pairs `{$name}` existe déjà
cli-quickstart-error-personality-workspace = impossible de créer le workspace de l'agent : {$err}
cli-quickstart-error-personality-filename-required = le nom de fichier est requis
cli-quickstart-error-personality-not-editable = `{$filename}` n'est pas un fichier de personnalité modifiable
cli-quickstart-error-personality-too-large = le contenu dépasse la limite de {$limit} caractères
cli-quickstart-error-personality-stage-failed = préparation de {$filename} échouée : {$err}
cli-quickstart-error-personality-write-failed = écriture de {$path} échouée : {$err}
cli-quickstart-error-agent-name-required = le nom de l'agent est requis
cli-quickstart-error-agent-exists = l'agent `{$name}` existe déjà
cli-no-channels-compiled = {"  "}Aucun type de canal n'est compilé dans ce binaire.
cli-quickstart-complete = Quickstart terminé. Agent `{$alias}` créé.
cli-next-steps = Étapes suivantes :
cli-agent-not-created = Votre agent n'a pas été créé — et rien n'a été modifié sur le disque.
cli-onboard-deprecated = `zeroclaw onboard` est obsolète — utilisez `zeroclaw quickstart`.
cli-otp-initialized = Secret OTP initialisé pour ZeroClaw.
cli-otp-enrollment-uri = URI d'enregistrement : {$uri}
cli-otp-received = {"  "}✓ OTP reçu
cli-secret-captured = {"  "}● Valeur capturée — appuyez sur Entrée pour enregistrer
cli-secret-received = {"  "}✓ Secret reçu
cli-pairing-enabled = 🔐 L'appairage de la passerelle est activé.
cli-pairing-use-code = {"  "}Utilisez ce code à usage unique pour appairer un nouvel appareil :
cli-pairing-post = {"    "}POST /pair avec l'en-tête X-Pairing-Code: {$code}
cli-pairing-restart = {"   "}Redémarrez la passerelle pour générer un nouveau code d'appairage.
cli-pairing-disabled = ⚠️  L'appairage de la passerelle est désactivé dans la configuration.
cli-gateway-running-q = {"   "}La passerelle est-elle en cours d'exécution ? Démarrez-la avec :
cli-status-title = 🦀 État de ZeroClaw
cli-security-status-title = État de sécurité ZeroClaw
cli-security-status-source = Source :      {$v}
cli-security-status-agent = Agent :       {$v}
cli-security-status-agent-enabled = Agent activé : {$enabled}
cli-security-status-risk-profile = Profil de risque : {$v}
cli-security-status-autonomy = Autonomie :   {$v}
cli-security-status-approvals = Approbations :  approbation requise pour risque moyen : {$medium}, commandes à haut risque bloquées : {$high}
cli-security-status-sandbox = Bac à sable :    demandé {$requested}, actif {$active} ({$description})
cli-security-status-workspace = Espace de travail :  {$dir} ; espace de travail uniquement : {$workspace_only} ; racines lecture-écriture : {$read_write_roots} ; racines lecture seule : {$read_only_roots} ; racines écriture seule : {$write_only_roots} ; transmission env : {$env_passthrough}
cli-security-status-credentials = Identifiants : chiffrement : {$encryption} ; secrets définis : {$secrets_set}/{$secrets_total} ; champs classifiés : {$classified_total} ; classes : {$classification_summary}
cli-security-status-credentials-classes-none = aucune
cli-security-status-gateway = Passerelle :    {$host}:{$port} ; appairage requis : {$pairing} ; liaison publique : {$public_bind} ; TLS : {$tls}
cli-security-status-warnings = Avertissements :   {$v}
cli-security-status-warnings-none = Avertissements :   aucun
cli-security-status-warning-agent-disabled = l'agent est désactivé
cli-security-status-warning-sandbox-disabled = le bac à sable est désactivé pour ce profil de risque d'agent
cli-security-status-warning-sandbox-none = le bac à sable actif est uniquement au niveau applicatif
cli-security-status-warning-sandbox-fallback = le backend de bac à sable demandé `{$requested}` a basculé vers `{$active}`
cli-security-status-warning-workspace-not-restricted = la politique de système de fichiers limitée à l'espace de travail est désactivée
cli-security-status-warning-shell-env-passthrough = {$count} variable(s) d'environnement shell sont transmises
cli-security-status-warning-secrets-unencrypted = le chiffrement des secrets de config est désactivé
cli-security-status-warning-credential-follow-up = certaines surfaces de config ayant la forme d'identifiants nécessitent encore un suivi
cli-security-status-warning-pairing-disabled = l'appairage de la passerelle n'est pas requis
cli-security-status-warning-public-bind-no-tls = la passerelle autorise une liaison publique sans TLS activé
cli-status-provider-none = 🤖 ModelProvider :      (aucun configuré)
cli-status-agents-none = 🛡️  Agents :        (aucun configuré)
cli-status-service-running = 🟢 Service :       en cours d'exécution
cli-status-service-stopped = 🔴 Service :       arrêté
cli-status-channels = Canaux :
cli-status-cli-always = {"  "}CLI :      ✅ toujours
cli-status-peripherals = Périphériques :
cli-desktop-download = Téléchargez l'application compagnon ZeroClaw :
cli-desktop-homebrew = Ou installez via Homebrew (bientôt disponible) :
cli-desktop-linux-pkg = {"  "}Téléchargez le fichier .deb ou .AppImage pour votre architecture.
cli-desktop-launching = Lancement de l'application compagnon ZeroClaw...
cli-status-version = Version :     {$v}
cli-status-workspace = Espace de travail :   {$v}
cli-status-config = Config :      {$v}
cli-status-provider-indent = {"   "}ModelProvider :      {$family}.{$alias}
cli-status-provider = 🤖 ModelProvider :      {$family}.{$alias}
cli-status-model = {"   "}Modèle :         {$model}
cli-status-observability = 📊 Observabilité :  {$v}
cli-status-trace-storage = 🧾 Stockage des traces :  {$mode} ({$path})
cli-status-agents = 🛡️  Agents :        {$v}
cli-status-agent-risk-profile = {$alias}={$level}
cli-status-agent-no-risk-profile-summary = {$alias}=<aucun risk_profile>
cli-status-runtime = ⚙️  Runtime :       {$v}
cli-status-web-ui-found = 🌐 Web UI :        TROUVÉ ({$path})
cli-status-web-ui-missing = 🌐 Web UI :        MANQUANT
cli-status-heartbeat = 💓 Battement de cœur :      {$v}
cli-status-heartbeat-every-minutes = toutes les {$minutes}min
cli-status-memory = 🧠 Mémoire :         {$backend} (sauvegarde auto : {$auto_save})
cli-status-security-noprofile = Sécurité ({$alias}) : <aucun risk_profile>
cli-status-security = Sécurité ({$alias}) :
cli-status-workspace-only = {"  "}Espace de travail uniquement :    {$v}
cli-status-allowed-roots = {"  "}Racines autorisées :     {$v}
cli-status-allowed-commands = {"  "}Commandes autorisées :  {$v}
cli-status-max-actions = {"  "}Actions max/heure :  {$v}
cli-status-cost-tracking = {"  "}Suivi des coûts :     {$v}
cli-status-max-cost-day = {"  "}Coût max/jour :      ${$v}
cli-status-max-cost-month = {"  "}Coût max/mois :    ${$v}
cli-status-spent-today = {"  "}Dépensé aujourd'hui :       {$spent} $ / {$limit} $
cli-status-spent-month = {"  "}Dépensé ce mois-ci :  {$spent} $ / {$limit} $
cli-status-otp = {"  "}OTP activé :       {$v}
cli-status-estop = {"  "}Arrêt d'urgence activé :    {$v}
cli-status-peripherals-enabled = {"  "}Activé :   {$v}
cli-status-boards = {"  "}Cartes :    {$v}
cli-status-word-enabled = activé
cli-status-word-disabled = désactivé
cli-status-word-yes = oui
cli-status-word-no = non
cli-status-word-on = activé
cli-status-word-off = désactivé
cli-status-word-none = (aucun)
cli-status-word-configured = configuré
cli-status-word-not-configured = non configuré
cli-status-channel-configured = ✅ {$status}
cli-status-channel-not-configured = ❌ {$status}
cli-status-channel-not-compiled = 🚫 configuré, non compilé
cli-desktop-not-installed = L'application compagnon ZeroClaw n'est pas installée.
cli-desktop-blurb1 = L'application compagnon est une application légère de barre de menus qui
cli-desktop-blurb2 = se connecte à la même passerelle que la CLI.
cli-config-all-configured = Toutes les sections sont déjà configurées.
cli-config-schema-current = La configuration est déjà à la version actuelle du schéma.
cli-config-applied-ops = {$count} opération(s) appliquée(s) :
cli-plugins-none = Aucun plugin installé.
cli-plugins-installed = Plugins installés :
cli-plugin-search-none = Aucun plugin ne correspond à '{$query}'.
cli-plugin-search-results = Plugins correspondant à '{$query}' ({$count}) :
cli-plugin-search-result = {$name} v{$version} — {$description}
cli-plugin-no-description = (aucune description)
cli-plugin-install-resolving = Résolution de '{$source}' depuis le registre de plugins...
cli-plugin-installed-from = Plugin installé depuis {$source}
cli-plugin-installed-name-version = Plugin {$name} v{$version} installé
cli-plugin-config-entry-seeded = [[plugins.entries]] initialisé pour '{$name}'. Définissez les valeurs de configuration du plugin avec `zeroclaw config set plugins.entries.{$name}.config.<key>`.
cli-plugin-config-entry-seed-skipped = avertissement : initialisation de l'entrée de configuration ignorée pour '{$name}' : la section [plugins] sur disque est mal formée. Réparez-la, ajoutez un bloc [[plugins.entries]] avec `name = "{$name}"`, puis définissez les valeurs avec `zeroclaw config set plugins.entries.{$name}.config.<key>`.
cli-plugin-config-entry-seed-unaddressable = avertissement : initialisation de l'entrée de configuration ignorée pour '{$name}' : les noms de plugin contenant '.' ne peuvent pas être adressés par des chemins de configuration pointés (`config set` découpe sur '.'). Ajoutez manuellement un bloc [[plugins.entries]] avec `name = "{$name}"` au fichier de configuration.
cli-config-section-degraded = avertissement : la section de configuration `{$section}` dans {$path} est mal formée et a été réinitialisée aux valeurs par défaut pour cette exécution. Les valeurs de cette section NE sont PAS appliquées. Exécutez `zeroclaw config migrate` pour voir l'erreur d'analyse, puis réparez le fichier.
cli-plugin-removed = Plugin « {$name} » supprimé.
cli-plugin-not-found = Plugin « {$name} » introuvable.
cli-plugin-legacy-detected = Remarque : les plugins situés à un emplacement hérité ({$path}) ne sont pas chargés par l'agent. Exécutez `zeroclaw plugin migrate` pour les déplacer vers {$target}.
cli-plugin-migrated = {$count} plugin(s) déplacé(s) de {$path} vers {$target}.
cli-plugin-migrate-none = Rien à migrer.
cli-estop-resume-done = Reprise après arrêt d'urgence terminée.
cli-estop-engaged = Arrêt d'urgence engagé.
cli-estop-status = État de l'arrêt d'urgence :
cli-auth-none = Aucun profil d'authentification configuré.
cli-auth-active = Profils actifs :
cli-warn-crypto-provider = Avertissement : Échec de l'installation du fournisseur de cryptographie par défaut : {$err}
cli-error-label = {"   "}Erreur : {$err}
cli-warn-cost-usage = {"  "}⚠ Impossible de charger l'utilisation des coûts : {$err}
cli-warn-cost-tracker = {"  "}⚠ Impossible d'initialiser le suivi des coûts : {$err}
cli-desktop-download-at = {"  "}Téléchargez-la sur : {$url}
cli-config-legend = Légende : 💉 remplacé par env  🔒 secret
cli-config-secret-set = {$path} est défini (secret chiffré — valeur non affichée)
cli-config-secret-unset = {$path} n'est pas défini (secret chiffré)
cli-config-updated = {$path} mis à jour.
cli-config-review-hint = Exécutez `zeroclaw config list` pour vérifier, puis définissez les champs requis.
cli-config-backed-up = Sauvegardé vers { $path }
cli-plugin-name-version = Plugin : { $name } v{ $version }
cli-plugin-description = Description : { $desc }
cli-plugin-capabilities = Capacités : { $v }
cli-plugin-permissions = Permissions : { $v }
cli-plugin-wasm = WASM : { $path }
cli-plugin-wasm-none = WASM : (plugin compétence uniquement)
cli-estop-domains-none = {"  "}domain_blocks:  (aucun)
cli-estop-domains = {"  "}domain_blocks:  { $v }
cli-estop-tools-none = {"  "}tool_freeze:    (aucun)
cli-estop-tools = {"  "}tool_freeze:    { $v }
cli-estop-updated-at = {"  "}updated_at:     { $v }
cli-auth-saved = Profil { $profile } enregistré
cli-auth-active-for = Profil actif pour { $provider } : { $profile }
cli-auth-refresh-ok = ✓ Actualisation du jeton OK (profil { $profile })
cli-auth-removed = Profil d'authentification supprimé { $provider }:{ $profile }
cli-auth-not-found = Profil d'authentification introuvable : { $provider }:{ $profile }
cli-auth-xai-imported = Profil d'authentification xAI importé depuis { $path }
cli-auth-xai-device-code-started = Connexion xAI par code d'appareil démarrée.
cli-auth-oauth-visit = Visitez : { $uri }
cli-auth-oauth-code = Code :  { $code }
cli-auth-oauth-fast-link = Lien rapide : { $uri }
cli-auth-xai-open-oauth-url = Ouvrez cette URL OAuth xAI dans votre navigateur et autorisez l'accès :
cli-auth-callback-capture-failed = Échec de la capture du callback : { $error }
cli-auth-run-paste-redirect = Exécutez `zeroclaw auth paste-redirect --model-provider { $provider } --profile { $profile }`
cli-auth-xai-no-pending-login = Aucune connexion xAI en attente trouvée. Exécutez d'abord `zeroclaw auth login --model-provider xai`.
cli-auth-paste-redirect-requires-input = paste-redirect requiert l'URL de redirection ou le code OAuth
cli-locales-fetched = {"  "}récupéré {$name} -> {$path}
cli-locales-skipped = {"  "}ignoré {$name} : absent en amont ({$path} ; essayé {$refs})
cli-locales-installed = {$count} catalogue(s) installé(s) pour « {$locale} » dans {$dir}
cli-browse-header = { $path } ({ $count } entrées)
cli-browse-empty = (vide)
cli-browse-file-bytes = { $name } ({ $bytes } octets)
cli-hardware-feature-required = La découverte du matériel nécessite la fonctionnalité « hardware ».
cli-hardware-feature-build = Compiler avec : cargo build --features hardware
cli-hardware-unsupported-platform = La découverte USB du matériel n'est pas prise en charge sur cette plateforme.
cli-hardware-supported-platforms = Plateformes prises en charge : Linux, macOS, Windows.
cli-update-already-current = Déjà à jour (v{ $version }).
cli-update-success = Mise à jour réussie vers la v{ $version } !
cli-update-prebuilt-channel-note = Les mises à jour précompilées utilisent la distribution standard légère. Compilez depuis les sources avec `./install.sh --source --preset full`, `--features channels-full` ou une fonctionnalité `channel-*` spécifique pour Slack et les autres canaux non inclus dans cette distribution.
cli-update-available = Mise à jour disponible : v{ $current } -> v{ $latest }
cli-update-forcing-reinstall = Réinstallation forcée : v{ $current } -> v{ $latest }
cli-update-not-writable = le répertoire d'installation { $dir } n'est pas accessible en écriture ({ $error }) ; relancez `zeroclaw update` avec des privilèges élevés (sudo sur macOS/Linux, une console Administrateur sous Windows)
cli-selftest-all-passed = Les { $total } vérifications ont toutes réussi.
cli-selftest-some-failed = { $failed }/{ $total } vérifications ont échoué.
cli-selftest-channel-config-uncompiled = { $compiled } types de canaux compilés, { $configured } compilés/configurés ; configurés mais non compilés : { $names }. Compilez depuis les sources avec `./install.sh --source --preset full`, `--features channels-full` ou la fonctionnalité `channel-*` spécifique.
cli-channels-header = Canaux :
cli-channels-cli-always = {"  "}✅ CLI (toujours disponible)
cli-channels-notion = {"  "}{ $status } Notion
cli-channels-not-compiled-header = {"  "}Configurés mais non compilés dans ce binaire :
cli-channels-not-compiled-entry = {"  "}🚫 {$name} (configuré, non compilé)
cli-channels-build-hint = {"  "}Compilez depuis les sources avec `./install.sh --source --preset full`, `--features channels-full` ou la fonctionnalité `channel-*` spécifique.
cli-channels-start-hint = Pour démarrer les canaux : zeroclaw channel start
cli-channels-doctor-hint = Pour vérifier l'état :    zeroclaw channel doctor
cli-channels-configure-hint = Pour configurer :      zeroclaw config set channels.<name>.<field>=<value>
cli-models-set-ok = Modèle par défaut défini sur « { $model } » sur { $provider }.
cli-models-status-current = Modèle par défaut : { $model } (fournisseur : { $provider })
cli-models-status-none = Aucun modèle par défaut configuré.
turn-interrupted-by-user = [interrompu par l'utilisateur]
turn-cancelled-client-rpc = [tour annulé via le client]
turn-stream-interrupted = [flux interrompu]
turn-model-fallback-notice = ⚡ { $requested_model } ({ $requested_provider }) était indisponible ; cette réponse a été générée par { $actual_model } ({ $actual_provider }).
history-trim-breadcrumb = [earlier turns omitted to fit the context window]
history-trim-reason-budget = context token budget exceeded
history-trim-reason-message-cap = limite de messages de l’historique dépassée
history-trim-floor-exceeds-budget = system prompt and tool definitions ({$floor} tokens) alone meet or exceed the context budget ({$budget} tokens); raise [runtime_profiles.<name>] max_context_tokens or reduce the tool surface by disabling unused integrations
turn-ingress-dropped = Cette requête n'a pas été traitée : { $reason }
turn-tool-interrupted-before-result = [interrompu par l'utilisateur avant que cet outil ne produise un résultat]
channel-runtime-malformed-tool-output = J'ai généré une erreur de format d'appel d'outil interne et n'ai pas pu terminer cette requête. Veuillez réessayer.
channel-runtime-new-session = Historique de conversation effacé. Nouveau départ.
channel-runtime-stop-sent = Signal d'arrêt envoyé.
channel-runtime-stop-no-task = Aucune tâche en cours pour ce périmètre d'expéditeur.
channel-runtime-model-empty = L'ID du modèle ne peut pas être vide. Utilisez `/model <model-id>`.
channel-runtime-model-switched = Modèle changé vers `{ $model }` (model_provider : `{ $provider }`). Contexte conservé.
channel-runtime-agent-scope-rejected = L'expéditeur `{ $sender }` n'est pas autorisé à utiliser `/model --agent` sur l'agent `{ $agent }`. Utilisez `/model --user { $model }` pour un remplacement limité à la session, ou demandez à un administrateur de marquer un groupe de pairs `admin_for_agent_scope = true` avec vous comme membre.
channel-runtime-request-timeout = ⚠️ La requête a expiré en attendant le modèle. Veuillez réessayer.
channel-runtime-current-model-status =
    model_provider actuel : `{ $provider }`
    Modèle actuel : `{ $model }`
channel-runtime-model-switch-hint = Changez de modèle avec `/model <model-id>` ou `/model <hint>`.
channel-runtime-provider-switch-hint = Changez de model_provider avec `/models <model_provider>`.
channel-runtime-available-providers-header = model_providers disponibles :
channel-runtime-configured-routes-header = Routes de modèle configurées :
channel-runtime-no-cached-models = Aucune liste de modèles en cache trouvée pour `{ $provider }`. Demandez à l'opérateur d'exécuter `zeroclaw models refresh --model-provider { $provider }`.
channel-runtime-cached-model-ids-header = IDs de modèle en cache ({ $count } premiers) :
channel-runtime-config-switch-hints =
    Utilisez `/models <model_provider>` pour changer de model_provider.
    Utilisez `/model <model-id>` pour changer de modèle.
channel-runtime-config-block-title =
    { "*" }Configuration du modèle{ "*" }
    Actuel : `{ $provider }` / `{ $model }`
channel-runtime-config-select-provider-placeholder = Sélectionner model_provider
channel-runtime-config-select-model-placeholder = Sélectionner le modèle
channel-runtime-config-provider-label = *ModelProvider*
channel-runtime-config-model-label = *Modèle*
channel-runtime-scope-user = utilisateur
channel-runtime-scope-agent = agent
channel-runtime-scope-overrides-summary =
    { "**" }Surcharges de modèle{ "**" } (session uniquement ; précédence user > agent > session > default) :
    • user : { $user }
    • agent : { $agent }
    • session (cette discussion) : { $session }
    • default (config) : { $default }
    Définissez un périmètre avec `/model --user|--agent <model-id>` ; effacez-le en le remettant sur la valeur par défaut.
channel-runtime-set-provider-switched =
    ModelProvider changé vers `{ $provider }` pour cette session d'expéditeur. Le modèle actuel est `{ $model }`.
    Utilisez `/model <model-id>` pour définir un modèle compatible avec ce provider.
channel-runtime-set-provider-init-failed =
    Échec de l'initialisation de model_provider `{ $provider }`. Route inchangée.
    Détails : { $error }
channel-runtime-provider-ambiguous = ModelProvider `{ $family }` a plusieurs alias configurés. Précisez lequel avec `/models { $family }.<alias>` : { $list }
channel-runtime-provider-no-alias = Aucune entrée de provider configurée pour `{ $provider }`. Ajoutez `[providers.models.{ $provider }]` (avec api_key/uri) ou sélectionnez un provider configuré ; `/models` liste les valeurs valides.
channel-runtime-provider-unknown = model_provider inconnu `{ $provider }`. Utilisez `/models` pour lister les model_providers valides.
channel-runtime-scoped-model-empty = L'ID du modèle ne peut pas être vide. Utilisez `/model --user|--agent <model-id>`.
channel-runtime-scoped-model-switched = Modèle défini sur `{ $model }` (model_provider : `{ $provider }`) pour le périmètre **{ $scope }**. Session uniquement ; réinitialisé au redémarrage.
channel-runtime-shadow-note = ⚠️ Une surcharge de plus haute précédence est active, donc les messages utiliseront plutôt `{ $model }` (`{ $provider }`) ; voir `/model`.
channel-runtime-thinking-set =
    Thinking défini sur `{ $level }` pour cette session d'expéditeur.
    Utilisez `/thinking reset` pour revenir à la valeur par défaut de l'agent.
channel-runtime-thinking-cleared = Surcharge de thinking effacée. Utilisation de la valeur par défaut de l'agent `{ $default }` pour cette session d'expéditeur.
channel-runtime-thinking-default =
    Thinking utilise déjà la valeur par défaut de l'agent `{ $default }` pour cette session d'expéditeur.
    Utilisez `/thinking high`, `/thinking max` ou `/thinking off` pour le surcharger.
channel-runtime-thinking-invalid = Niveau de thinking inconnu `{ $raw }`. Utilisez `/thinking off|minimal|low|medium|high|max`, `/thinking on` ou `/thinking reset`.
channel-runtime-provider-turn-init-failed =
    ⚠️ Échec de l'initialisation de model_provider `{ $provider }`. Exécutez `/models` pour choisir un autre model_provider.
    Détails : { $error }
channel-runtime-fallback-footer =
    ⚡ `{ $requested }` indisponible — réponse de **{ $actual }** (`{ $model }`)
    Changer de modèle : /models
cli-alias-list-empty = (aucune entrée sous {$section})
cli-alias-created = {$section}.{$alias} créé
cli-alias-exists = {$section}.{$alias} existe déjà (aucun changement)
cli-alias-impact-scrub-header = la suppression de {$section}.{$alias} nettoierait {$count} référence(s) :
cli-alias-impact-blocked-header = la suppression de {$section}.{$alias} est BLOQUÉE par {$count} référence(s) stricte(s) :
cli-alias-impact-blocker = ✗ {$path} (référence stricte)
cli-alias-impact-scrub = • {$path} (serait nettoyé)
cli-alias-no-changes = Aucun changement effectué. Relancez avec --yes pour appliquer (ou --dry-run pour prévisualiser).
cli-alias-warn-workspace-archive = avertissement : échec de l'archivage de l'espace de travail : {$error}
cli-alias-owned-cascaded = état détenu propagé : memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions} → {$archive}
cli-alias-owned-repointed = état détenu redirigé : memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions}
cli-alias-warn-workspace-move = avertissement : échec du déplacement de l'espace de travail : {$error}
cli-alias-warn = avertissement : {$warning}
cli-alias-deleted = {$section}.{$alias} supprimé ({$count} référence(s) nettoyée(s))
cli-alias-delete-refused-header = refusé : {$count} référence(s) stricte(s) bloquent la suppression :
cli-alias-delete-refused-hint = suppression refusée — résolvez d'abord les références strictes
cli-alias-not-configured = {$path} n'est pas configuré
cli-alias-delete-failed = échec de la suppression : {$error}
cli-alias-delete-reserved-default = l'agent `default` est réservé et ne peut pas être supprimé
cli-alias-create-reserved-default = l'agent `default` est réservé et ne peut pas être créé
cli-alias-renamed = {$section}.{$from} → {$section}.{$to} renommé ({$count} chemin(s) de référence réécrit(s))
cli-alias-rename-invalid = nouvel alias invalide : {$message}
cli-alias-rename-reserved = l'alias `{$alias}` est réservé et ne peut pas être renommé
cli-alias-rename-postcondition = échec de la post-condition de la propagation du renommage : {$message}
cli-alias-unknown-provider-category = catégorie de fournisseur inconnue `{$category}` (attendu : models | tts | transcription)
cli-alias-no-such-section = section de configuration introuvable : {$section}
cli-alias-live-acp-sessions = {$count} session(s) ACP active(s) pour `{$alias}` — terminez-les d'abord
cli-alias-owned-state-unavailable = note : les références de configuration ont été mises à jour, mais l'état détenu de l'agent (lignes de mémoire, répertoire d'espace de travail, lignes cron/acp/session) N'A PAS encore été propagé par ce CLI — utilisez l'API de la passerelle pour la propagation complète de l'état détenu.
cli-bundle-not-configured = le bundle de compétences '{$alias}' n'est pas configuré
cli-bundle-rename-failed = échec du renommage : {$error}
cli-bundle-exists = le bundle de compétences '{$alias}' existe déjà (aucun changement)
cli-bundle-created = skill_bundles.{$alias} créé (rép. : {$dir})
cli-bundle-created-warn = skill_bundles.{$alias} créé (avertissement : échec de la résolution du répertoire : {$error})
cli-bundle-impact-header = la suppression de skill_bundles.{$alias} le retirerait de {$count} référence(s) d'agent :
cli-bundle-no-changes = Aucun changement effectué. Relancez avec --yes pour appliquer.
cli-bundle-archived = répertoire de bundle archivé → {$path}
cli-bundle-warn-archive = avertissement : échec de l'archivage du répertoire de bundle : {$error}
cli-bundle-deleted = skill_bundles.{$alias} supprimé (retiré de {$count} agent(s))
cli-bundle-warn-move = avertissement : échec du déplacement du répertoire de bundle : {$error}
cli-bundle-renamed = skill_bundles.{$from} → skill_bundles.{$to} renommé

# ── Indications de redémarrage du tableau de bord web — RestartInfo.hint (PR #8173) ──
# Les quatre premières sont des modèles de commande shell affichés tels quels ; non traduits.
cli-gateway-restart-hint-kubernetes = kubectl rollout restart deployment/zeroclaw
cli-gateway-restart-hint-container = docker compose restart
cli-gateway-restart-hint-systemd = systemctl restart zeroclaw
cli-gateway-restart-hint-launchd = launchctl kickstart -k <your-zeroclaw-label>
cli-gateway-restart-hint-process = redémarrez le processus `zeroclaw daemon`

cli-daemon-gateway-already-running = Une passerelle ZeroClaw est déjà en cours d'exécution sur {$host}:{$port}. Le démon supervise sa propre passerelle et ne démarrera pas une seconde passerelle sur la même adresse. Arrêtez cette passerelle (ou pointez le démon vers un port libre avec `zeroclaw config set gateway.port <port>`), puis relancez le démon.
cli-daemon-gateway-port-occupied = L'adresse de passerelle {$host}:{$port} est déjà utilisée par un autre processus. Libérez le port ou pointez le démon vers un port libre (`zeroclaw config set gateway.port <port>`), puis relancez le démon.
cli-agent-context-bar = ctx: {$used} / {$max}  {$bar}  {$pct}%
cli-agent-context-bar-unknown = ctx: inconnu / {$max}
cli-doctor-ctxwin-already-set = {$provider_ref}: a déjà context_window = {$ctx}
cli-doctor-ctxwin-no-model = {$provider_ref}: aucun modèle configuré, ignoré
cli-doctor-ctxwin-would-set = {$provider_ref}: définirait context_window = {$ctx} (simulation)
cli-doctor-ctxwin-set = {$provider_ref}: context_window défini = {$ctx}
cli-doctor-ctxwin-not-found = {$provider_ref}: entrée introuvable pour mise à jour
cli-doctor-ctxwin-fetch-failed = {$provider_ref}: n'expose pas la fenêtre de contexte ou l'obtention a échoué
cli-doctor-ctxwin-saved = {$updated} mise(s) à jour enregistrée(s) dans config.toml
cli-doctor-ctxwin-dry-run = Simulation terminée — aucun changement. Relancez sans --dry-run pour appliquer.
cli-doctor-ctxwin-none = Aucune mise à jour nécessaire.
cli-doctor-ctxwin-write-failed = {$provider_ref}: échec de l'écriture de context_window: {$error}

# ── Degraded config sections (doctor diagnose, #8835) ──
cli-doctor-degraded-security = La section de configuration CRITIQUE POUR LA SÉCURITÉ `{$path}` est invalide et a été réinitialisée à sa valeur par défaut pour permettre au daemon de démarrer ; la posture en cours d'exécution peut être PLUS FAIBLE que prévu. Exécutez `zeroclaw config migrate` pour voir l'erreur d'analyse, puis réparez le fichier.
cli-doctor-degraded-section = La section de configuration `{$path}` est malformée et a été réinitialisée aux valeurs par défaut ; les valeurs de cette section ne sont PAS en vigueur. Exécutez `zeroclaw config migrate` pour voir l'erreur d'analyse, puis réparez le fichier.
sop-approval-deferred-at-capacity = Impossible de reprendre l’exécution {$run_id} : tous les créneaux d’exécution sont occupés. L’approbation reste en attente ; réessayez lorsqu’un créneau se libère.
sop-approval-policy-unavailable = L’approbation a échoué car l’étape SOP en attente est indisponible : {$reason}. L’exécution reste en attente.
sop-rpc-decision-invalid-state = L’exécution {$run_id} ne peut pas être résolue dans son état actuel.
sop-rpc-decision-unauthorized = L’identité RPC n’est pas autorisée à résoudre cette étape SOP.
sop-rpc-policy-missing = La politique d’approbation SOP « {$name} » n’est pas configurée.
sop-rpc-policy-unavailable = La politique du SOP en attente est indisponible : {$reason}.
