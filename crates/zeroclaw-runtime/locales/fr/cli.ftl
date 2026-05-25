cli-about = L'assistant IA le plus rapide et le plus léger.
cli-no-command-provided = Aucune commande fournie.
cli-try-onboard = Essayez `zeroclaw onboard` pour initialiser votre espace de travail.
cli-onboard-about = Initialiser votre espace de travail et votre configuration
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
cli-skills-list-about = Lister toutes les compétences installées
cli-skills-audit-about = Auditer un répertoire source de compétence ou une compétence installée
cli-skills-install-about = Installer une nouvelle compétence à partir d'une URL ou d'un chemin local
cli-skills-remove-about = Supprimer une compétence installée
cli-skills-test-about = Exécuter la validation TEST.sh pour une compétence (ou toutes les compétences)
cli-cron-list-about = Lister toutes les tâches planifiées
cli-cron-add-about = Ajouter une nouvelle tâche planifiée récurrente
cli-cron-add-at-about = Ajouter une tâche unique qui se déclenche à un moment UTC spécifique
cli-cron-add-every-about = Ajouter une tâche qui se répète à un intervalle fixe
cli-cron-once-about = Ajouter une tâche unique qui se déclenche après un délai à partir de maintenant
cli-cron-remove-about = Supprimer une tâche planifiée
cli-cron-update-about = Mettre à jour un ou plusieurs champs d'une tâche planifiée existante
cli-cron-pause-about = Mettre en pause une tâche planifiée
cli-cron-resume-about = Reprendre une tâche en pause
cli-auth-login-about = Se connecter avec OAuth (OpenAI Codex ou Gemini)
cli-auth-refresh-about = Actualiser le jeton d'accès OpenAI Codex en utilisant le jeton d'actualisation
cli-auth-logout-about = Supprimer le profil d'authentification
cli-auth-use-about = Définir le profil actif pour un fournisseur
cli-auth-list-about = Lister les profils d'authentification
cli-auth-status-about = Afficher le statut d'authentification avec le profil actif et les informations d'expiration du jeton
cli-memory-list-about = Lister les entrées de mémoire avec des filtres optionnels
cli-memory-get-about = Obtenir une entrée de mémoire spécifique par clé
cli-memory-stats-about = Afficher les statistiques et l'état de santé du backend mémoire
cli-memory-clear-about = Effacer les mémoires par catégorie, par clé, ou tout effacer
cli-estop-status-about = Imprimer le statut actuel d'arrêt d'urgence
cli-estop-resume-about = Reprendre depuis un niveau d'arrêt d'urgence engagé
cli-models-refresh-about = Actualiser et mettre en cache les modèles du fournisseur
cli-models-list-about = Lister les modèles mis en cache pour un fournisseur
cli-models-set-about = Définir le modèle par défaut dans la configuration
cli-models-status-about = Afficher la configuration actuelle du modèle et l'état du cache
cli-doctor-models-about = Sonder les catalogues de modèles à travers les fournisseurs et signaler la disponibilité
cli-doctor-traces-about = Interroger les événements de trace d'exécution (diagnostics d'outils et réponses de modèle)
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
cli-memory-persist-about = Persister les données de l'état de l'agent dans des fichiers locaux ou un stockage distant
cli-memory-remove-about = Supprimer une entrée de mémoire par clé
cli-note-show-about = Afficher un contenu de note par nom
cli-note-update-about = Remplacer le contenu d'une note par son nom
cli-prompt-list-about = Lister les invites disponibles
cli-prompt-show-about = Montrer le contenu d'une invite par son nom
cli-secret-get-about = Voir un secret
cli-secret-list-about = Lister les secrets
cli-secret-long-about =
    Gérer les secrets chiffrés avec AES-256.

    Lister, ajouter, mettre à jour, effacer et chiffrer les secrets stockés de manière sécurisée pour l'authentification et la configuration.

    Exemples :
    zeroclaw secret list
    zeroclaw secret add OPENAI_API_KEY
    zeroclaw secret update OPENAI_API_KEY
    zeroclaw secret delete OPENAI_API_KEY
    zeroclaw secret encrypt "chiffrer ce message"
