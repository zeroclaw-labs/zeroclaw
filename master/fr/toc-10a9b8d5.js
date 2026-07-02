// Populate the sidebar
//
// This is a script, and not included directly in the page, to control the total size of the book.
// The TOC contains an entry for each page, so if each page includes a copy of the TOC,
// the total size of the page becomes O(n**2).
class MDBookSidebarScrollbox extends HTMLElement {
    constructor() {
        super();
    }
    connectedCallback() {
        this.innerHTML = '<ol class="chapter"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="introduction.html"><strong aria-hidden="true">1.</strong> Introduction</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/index.html"><strong aria-hidden="true">2.</strong> Philosophie</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/you-own-it.html"><strong aria-hidden="true">2.1.</strong> Vous en êtes le propriétaire</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/security-first.html"><strong aria-hidden="true">2.2.</strong> La sécurité d&#39;abord, avec des échappatoires</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/minimal.html"><strong aria-hidden="true">2.3.</strong> Minimal : en taille de binaire, en dépendances et en surface d&#39;exposition</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/provider-agnostic.html"><strong aria-hidden="true">2.4.</strong> Indépendant du fournisseur</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/what-this-isnt.html"><strong aria-hidden="true">2.5.</strong> Ce que ce n&#39;est pas</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="philosophy/how-decisions-get-made.html"><strong aria-hidden="true">2.6.</strong> Comment les décisions sont prises</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/index.html"><strong aria-hidden="true">3.</strong> Premiers pas</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/concepts.html"><strong aria-hidden="true">3.1.</strong> Concepts</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/quickstart.html"><strong aria-hidden="true">3.2.</strong> Démarrage rapide</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/yolo.html"><strong aria-hidden="true">3.3.</strong> Mode YOLO</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/multi-model-setup.html"><strong aria-hidden="true">3.4.</strong> Configuration multi-modèles</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/zerocode.html"><strong aria-hidden="true">3.5.</strong> zerocode</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="getting-started/language.html"><strong aria-hidden="true">3.6.</strong> Langue et traductions</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/overview.html"><strong aria-hidden="true">4.</strong> zerocode</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/running.html"><strong aria-hidden="true">4.1.</strong> Exécution de zerocode</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/config.html"><strong aria-hidden="true">4.2.</strong> Volet de configuration</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/themes.html"><strong aria-hidden="true">4.3.</strong> Thèmes et couleurs du terminal</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/remote.html"><strong aria-hidden="true">4.4.</strong> Configuration distante (WSS)</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="zerocode/environment.html"><strong aria-hidden="true">4.5.</strong> Transmission directe de l&#39;environnement</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/index.html"><strong aria-hidden="true">5.</strong> Installation</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/linux.html"><strong aria-hidden="true">5.1.</strong> Linux</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/macos.html"><strong aria-hidden="true">5.2.</strong> macOS</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/windows.html"><strong aria-hidden="true">5.3.</strong> Windows</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/freebsd.html"><strong aria-hidden="true">5.4.</strong> FreeBSD</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/nixos.html"><strong aria-hidden="true">5.5.</strong> NixOS</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/container.html"><strong aria-hidden="true">5.6.</strong> Docker et conteneurs</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/service.html"><strong aria-hidden="true">5.7.</strong> Gestion des services</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="setup/dist-files.html"><strong aria-hidden="true">5.8.</strong> Fichiers d&#39;installation de la plateforme</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/overview.html"><strong aria-hidden="true">6.</strong> Architecture</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/request-lifecycle.html"><strong aria-hidden="true">6.1.</strong> Cycle de vie de la requête</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/crates.html"><strong aria-hidden="true">6.2.</strong> Crate</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/logging.html"><strong aria-hidden="true">6.3.</strong> Journalisation</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/runtime-state-and-persistence.html"><strong aria-hidden="true">6.4.</strong> Runtime state and persistence</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/config-lifecycle.html"><strong aria-hidden="true">6.5.</strong> Config lifecycle</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="architecture/rpc-socket.html"><strong aria-hidden="true">6.6.</strong> Transport socket RPC</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="reference/index.html"><strong aria-hidden="true">7.</strong> Référence</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="reference/cli.html"><strong aria-hidden="true">7.1.</strong> CLI</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="reference/config.html"><strong aria-hidden="true">7.2.</strong> Config</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="reference/env-vars.html"><strong aria-hidden="true">7.3.</strong> Variables d&#39;environnement</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="api.html"><strong aria-hidden="true">7.4.</strong> API (rustdoc)</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="gateway/api.html"><strong aria-hidden="true">7.5.</strong> API HTTP Gateway</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="gateway/a2a-discovery.html"><strong aria-hidden="true">7.6.</strong> A2A agent discovery</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="gateway/web-dashboard.html"><strong aria-hidden="true">7.7.</strong> Tableau de bord web (web_dist_dir)</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/overview.html"><strong aria-hidden="true">8.</strong> Agents</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/anatomy.html"><strong aria-hidden="true">8.1.</strong> Anatomie d&#39;un agent</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/filesystem.html"><strong aria-hidden="true">8.2.</strong> Composants du système de fichiers</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/operating.html"><strong aria-hidden="true">8.3.</strong> Exécution des agents</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/delegation.html"><strong aria-hidden="true">8.4.</strong> Délégation et sous-agents</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/internals.html"><strong aria-hidden="true">8.5.</strong> Mécanismes internes du runtime</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="agents/history-management.html"><strong aria-hidden="true">8.6.</strong> History management</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/overview.html"><strong aria-hidden="true">9.</strong> Fournisseurs de modèles</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/catalog.html"><strong aria-hidden="true">9.1.</strong> Catalogue des fournisseurs</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/configuration.html"><strong aria-hidden="true">9.2.</strong> Configuration</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/streaming.html"><strong aria-hidden="true">9.3.</strong> Diffusion en continu</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/routing.html"><strong aria-hidden="true">9.4.</strong> Routage</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/custom.html"><strong aria-hidden="true">9.5.</strong> Fournisseurs personnalisés</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="providers/openai-codex-subscription.html"><strong aria-hidden="true">9.6.</strong> OpenAI Codex (abonnement)</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/overview.html"><strong aria-hidden="true">10.</strong> Canaux et intégrations</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/peer-groups.html"><strong aria-hidden="true">10.1.</strong> Groupes de pairs</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/matrix.html"><strong aria-hidden="true">10.2.</strong> Matrice</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/discord.html"><strong aria-hidden="true">10.3.</strong> Discord</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/slack.html"><strong aria-hidden="true">10.4.</strong> Slack</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/mattermost.html"><strong aria-hidden="true">10.5.</strong> Mattermost</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/line.html"><strong aria-hidden="true">10.6.</strong> LINE</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/nextcloud-talk.html"><strong aria-hidden="true">10.7.</strong> Nextcloud Talk</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/signal.html"><strong aria-hidden="true">10.8.</strong> Signal</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/whatsapp.html"><strong aria-hidden="true">10.9.</strong> WhatsApp</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/chat-others.html"><strong aria-hidden="true">10.10.</strong> Autres plateformes de chat</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/social.html"><strong aria-hidden="true">10.11.</strong> Social (Bluesky, Nostr, Twitter, Reddit)</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/email.html"><strong aria-hidden="true">10.12.</strong> E-mail</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/voice.html"><strong aria-hidden="true">10.13.</strong> Voix et téléphonie</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/webhook.html"><strong aria-hidden="true">10.14.</strong> Webhooks</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/mqtt.html"><strong aria-hidden="true">10.15.</strong> MQTT</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/amqp.html"><strong aria-hidden="true">10.16.</strong> AMQP</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/filesystem.html"><strong aria-hidden="true">10.17.</strong> Système de fichiers</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="channels/acp.html"><strong aria-hidden="true">10.18.</strong> ACP (Agent Client Protocol)</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/overview.html"><strong aria-hidden="true">11.</strong> Outils et extensibilité</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/mcp.html"><strong aria-hidden="true">11.1.</strong> MCP (Protocole de Contexte du Modèle)</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/browser.html"><strong aria-hidden="true">11.2.</strong> Automatisation du navigateur</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/relationship-memory.html"><strong aria-hidden="true">11.3.</strong> Relationship memory</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/relationship-memory-skill-template.html"><strong aria-hidden="true">11.3.1.</strong> Using relationship memory from skills</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/skills.html"><strong aria-hidden="true">11.4.</strong> Compétences</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="tools/python-skills.html"><strong aria-hidden="true">11.5.</strong> Compétences Python</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="security/overview.html"><strong aria-hidden="true">12.</strong> Sécurité et autonomie</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="security/model.html"><strong aria-hidden="true">12.1.</strong> Le modèle de sécurité</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="security/autonomy.html"><strong aria-hidden="true">12.2.</strong> Niveaux d&#39;autonomie</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="security/sandboxing.html"><strong aria-hidden="true">12.3.</strong> Isolation</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="security/tool-receipts.html"><strong aria-hidden="true">12.4.</strong> Reçus d&#39;outils</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/overview.html"><strong aria-hidden="true">13.</strong> Opérations et déploiement</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/service.html"><strong aria-hidden="true">13.1.</strong> Service et démon</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/observability.html"><strong aria-hidden="true">13.2.</strong> Journaux et observabilité</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/cost-tracking.html"><strong aria-hidden="true">13.3.</strong> Suivi des coûts</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/troubleshooting.html"><strong aria-hidden="true">13.4.</strong> Dépannage</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="ops/network-deployment.html"><strong aria-hidden="true">13.5.</strong> Déploiement du réseau</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/index.html"><strong aria-hidden="true">14.</strong> Matériel et cartes</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/subsystem.html"><strong aria-hidden="true">14.1.</strong> Sous-système matériel</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/adding-boards-and-tools.html"><strong aria-hidden="true">14.2.</strong> Ajout de tableaux et d&#39;outils</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/hardware-peripherals-design.html"><strong aria-hidden="true">14.3.</strong> Conception des périphériques</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/arduino-uno-q-setup.html"><strong aria-hidden="true">14.4.</strong> Arduino Uno Q</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/nucleo-setup.html"><strong aria-hidden="true">14.5.</strong> STM32 Nucleo</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/android-setup.html"><strong aria-hidden="true">14.6.</strong> Android</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/aardvark.html"><strong aria-hidden="true">14.7.</strong> Aardvark</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="hardware/raspberry-pi-setup.html"><strong aria-hidden="true">14.8.</strong> Raspberry Pi</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/index.html"><strong aria-hidden="true">15.</strong> Procédures opérationnelles standard</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/how-it-works.html"><strong aria-hidden="true">15.1.</strong> Fonctionnement des SOP</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/syntax.html"><strong aria-hidden="true">15.2.</strong> Syntaxe</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/cookbook.html"><strong aria-hidden="true">15.3.</strong> Recettes</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/overview.html"><strong aria-hidden="true">15.4.</strong> SOP Fan-In</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/mqtt.html"><strong aria-hidden="true">15.4.1.</strong> MQTT</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/filesystem.html"><strong aria-hidden="true">15.4.2.</strong> Système de fichiers</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/amqp.html"><strong aria-hidden="true">15.4.3.</strong> AMQP</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/manual.html"><strong aria-hidden="true">15.4.4.</strong> Manual</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/webhook.html"><strong aria-hidden="true">15.4.5.</strong> Webhook</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/cron.html"><strong aria-hidden="true">15.4.6.</strong> Cron</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/peripheral.html"><strong aria-hidden="true">15.4.7.</strong> Peripheral</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/fan-in/calendar.html"><strong aria-hidden="true">15.4.8.</strong> Calendar</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/observability.html"><strong aria-hidden="true">15.5.</strong> Observabilité</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="sop/example.html"><strong aria-hidden="true">15.6.</strong> Exemple détaillé</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/index.html"><strong aria-hidden="true">16.</strong> Extensions et plugins</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/first-party-extensions.html"><strong aria-hidden="true">16.1.</strong> Extensions internes</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/how-plugins-work.html"><strong aria-hidden="true">16.2.</strong> How plugins work</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/tool-inventory.html"><strong aria-hidden="true">16.3.</strong> Built-in tool inventory</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/plugin-protocol.html"><strong aria-hidden="true">16.4.</strong> Protocole du plugin</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/extension-examples.html"><strong aria-hidden="true">16.5.</strong> Exemples d’extension</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/building-docs.html"><strong aria-hidden="true">16.6.</strong> Génération de la documentation localement</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="developing/web.html"><strong aria-hidden="true">16.7.</strong> Création du tableau de bord web</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/index.html"><strong aria-hidden="true">17.</strong> Fondations (RFC)</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-001-intentional-architecture.html"><strong aria-hidden="true">17.1.</strong> Architecture intentionnelle</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-002-documentation-standards.html"><strong aria-hidden="true">17.2.</strong> Normes de documentation</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-003-governance.html"><strong aria-hidden="true">17.3.</strong> Gouvernance</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-004-engineering-infrastructure.html"><strong aria-hidden="true">17.4.</strong> Infrastructure d&#39;ingénierie</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-005-contribution-culture.html"><strong aria-hidden="true">17.5.</strong> Culture de contribution</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="foundations/fnd-006-zero-compromise-in-practice.html"><strong aria-hidden="true">17.6.</strong> Zéro compromis en pratique</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/index.html"><strong aria-hidden="true">18.</strong> Contribuer</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/how-to.html"><strong aria-hidden="true">18.1.</strong> Comment contribuer</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/architecture-map.html"><strong aria-hidden="true">18.2.</strong> Carte d&#39;architecture et de contribution</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/rfcs.html"><strong aria-hidden="true">18.3.</strong> Processus de la RFC</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/communication.html"><strong aria-hidden="true">18.4.</strong> Communication</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/privacy.html"><strong aria-hidden="true">18.5.</strong> Discipline relative à la confidentialité et aux données personnelles</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/testing.html"><strong aria-hidden="true">18.6.</strong> Tests</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/agent-policy-parity-harness.html"><strong aria-hidden="true">18.7.</strong> Agent-policy parity</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/pr-review-protocol.html"><strong aria-hidden="true">18.8.</strong> Protocole de revue de code</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/multi-agent-setup.html"><strong aria-hidden="true">18.9.</strong> Configuration multi-agents</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="contributing/cla.html"><strong aria-hidden="true">18.10.</strong> Accord de licence de contributeur</a></span></li></ol><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/index.html"><strong aria-hidden="true">19.</strong> Mainteneurs</a><a class="chapter-fold-toggle"><div>❱</div></a></span><ol class="section"><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/docs-and-translations.html"><strong aria-hidden="true">19.1.</strong> Docs et Traductions</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/ci-and-actions.html"><strong aria-hidden="true">19.2.</strong> CI &amp; Actions</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/skills.html"><strong aria-hidden="true">19.3.</strong> Compétences de Claude Code</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/pr-workflow.html"><strong aria-hidden="true">19.4.</strong> Flux de travail PR</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/reviewer-playbook.html"><strong aria-hidden="true">19.5.</strong> Guide du réviseur</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/labels.html"><strong aria-hidden="true">19.6.</strong> Étiquettes</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/superseding.html"><strong aria-hidden="true">19.7.</strong> PRs qui remplacent</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/release-runbook.html"><strong aria-hidden="true">19.8.</strong> Livre de procédure de version</a></span></li><li class="chapter-item "><span class="chapter-link-wrapper"><a href="maintainers/release-verification.html"><strong aria-hidden="true">19.9.</strong> Release artifact verification</a></span></li></ol></li></ol>';
        // Set the current, active page, and reveal it if it's hidden
        let current_page = document.location.href.toString().split('#')[0].split('?')[0];
        if (current_page.endsWith('/')) {
            current_page += 'index.html';
        }
        const links = Array.prototype.slice.call(this.querySelectorAll('a'));
        const l = links.length;
        for (let i = 0; i < l; ++i) {
            const link = links[i];
            const href = link.getAttribute('href');
            if (href && !href.startsWith('#') && !/^(?:[a-z+]+:)?\/\//.test(href)) {
                link.href = path_to_root + href;
            }
            // The 'index' page is supposed to alias the first chapter in the book.
            if (link.href === current_page
                || i === 0
                && path_to_root === ''
                && current_page.endsWith('/index.html')) {
                link.classList.add('active');
                let parent = link.parentElement;
                while (parent) {
                    if (parent.tagName === 'LI' && parent.classList.contains('chapter-item')) {
                        parent.classList.add('expanded');
                    }
                    parent = parent.parentElement;
                }
            }
        }
        // Track and set sidebar scroll position
        this.addEventListener('click', e => {
            if (e.target.tagName === 'A') {
                sessionStorage.setItem('sidebar-scroll', this.scrollTop);
            }
        }, { passive: true });
        const sidebarScrollTop = sessionStorage.getItem('sidebar-scroll');
        sessionStorage.removeItem('sidebar-scroll');
        if (sidebarScrollTop) {
            // preserve sidebar scroll position when navigating via links within sidebar
            this.scrollTop = sidebarScrollTop;
        } else {
            // scroll sidebar to current active section when navigating via
            // 'next/previous chapter' buttons
            const activeSection = document.querySelector('#mdbook-sidebar .active');
            if (activeSection) {
                activeSection.scrollIntoView({ block: 'center' });
            }
        }
        // Toggle buttons
        const sidebarAnchorToggles = document.querySelectorAll('.chapter-fold-toggle');
        function toggleSection(ev) {
            ev.currentTarget.parentElement.parentElement.classList.toggle('expanded');
        }
        Array.from(sidebarAnchorToggles).forEach(el => {
            el.addEventListener('click', toggleSection);
        });
    }
}
window.customElements.define('mdbook-sidebar-scrollbox', MDBookSidebarScrollbox);


// ---------------------------------------------------------------------------
// Support for dynamically adding headers to the sidebar.

(function() {
    // This is used to detect which direction the page has scrolled since the
    // last scroll event.
    let lastKnownScrollPosition = 0;
    // This is the threshold in px from the top of the screen where it will
    // consider a header the "current" header when scrolling down.
    const defaultDownThreshold = 150;
    // Same as defaultDownThreshold, except when scrolling up.
    const defaultUpThreshold = 300;
    // The threshold is a virtual horizontal line on the screen where it
    // considers the "current" header to be above the line. The threshold is
    // modified dynamically to handle headers that are near the bottom of the
    // screen, and to slightly offset the behavior when scrolling up vs down.
    let threshold = defaultDownThreshold;
    // This is used to disable updates while scrolling. This is needed when
    // clicking the header in the sidebar, which triggers a scroll event. It
    // is somewhat finicky to detect when the scroll has finished, so this
    // uses a relatively dumb system of disabling scroll updates for a short
    // time after the click.
    let disableScroll = false;
    // Array of header elements on the page.
    let headers;
    // Array of li elements that are initially collapsed headers in the sidebar.
    // I'm not sure why eslint seems to have a false positive here.
    // eslint-disable-next-line prefer-const
    let headerToggles = [];
    // This is a debugging tool for the threshold which you can enable in the console.
    let thresholdDebug = false;

    // Updates the threshold based on the scroll position.
    function updateThreshold() {
        const scrollTop = window.pageYOffset || document.documentElement.scrollTop;
        const windowHeight = window.innerHeight;
        const documentHeight = document.documentElement.scrollHeight;

        // The number of pixels below the viewport, at most documentHeight.
        // This is used to push the threshold down to the bottom of the page
        // as the user scrolls towards the bottom.
        const pixelsBelow = Math.max(0, documentHeight - (scrollTop + windowHeight));
        // The number of pixels above the viewport, at least defaultDownThreshold.
        // Similar to pixelsBelow, this is used to push the threshold back towards
        // the top when reaching the top of the page.
        const pixelsAbove = Math.max(0, defaultDownThreshold - scrollTop);
        // How much the threshold should be offset once it gets close to the
        // bottom of the page.
        const bottomAdd = Math.max(0, windowHeight - pixelsBelow - defaultDownThreshold);
        let adjustedBottomAdd = bottomAdd;

        // Adjusts bottomAdd for a small document. The calculation above
        // assumes the document is at least twice the windowheight in size. If
        // it is less than that, then bottomAdd needs to be shrunk
        // proportional to the difference in size.
        if (documentHeight < windowHeight * 2) {
            const maxPixelsBelow = documentHeight - windowHeight;
            const t = 1 - pixelsBelow / Math.max(1, maxPixelsBelow);
            const clamp = Math.max(0, Math.min(1, t));
            adjustedBottomAdd *= clamp;
        }

        let scrollingDown = true;
        if (scrollTop < lastKnownScrollPosition) {
            scrollingDown = false;
        }

        if (scrollingDown) {
            // When scrolling down, move the threshold up towards the default
            // downwards threshold position. If near the bottom of the page,
            // adjustedBottomAdd will offset the threshold towards the bottom
            // of the page.
            const amountScrolledDown = scrollTop - lastKnownScrollPosition;
            const adjustedDefault = defaultDownThreshold + adjustedBottomAdd;
            threshold = Math.max(adjustedDefault, threshold - amountScrolledDown);
        } else {
            // When scrolling up, move the threshold down towards the default
            // upwards threshold position. If near the bottom of the page,
            // quickly transition the threshold back up where it normally
            // belongs.
            const amountScrolledUp = lastKnownScrollPosition - scrollTop;
            const adjustedDefault = defaultUpThreshold - pixelsAbove
                + Math.max(0, adjustedBottomAdd - defaultDownThreshold);
            threshold = Math.min(adjustedDefault, threshold + amountScrolledUp);
        }

        if (documentHeight <= windowHeight) {
            threshold = 0;
        }

        if (thresholdDebug) {
            const id = 'mdbook-threshold-debug-data';
            let data = document.getElementById(id);
            if (data === null) {
                data = document.createElement('div');
                data.id = id;
                data.style.cssText = `
                    position: fixed;
                    top: 50px;
                    right: 10px;
                    background-color: 0xeeeeee;
                    z-index: 9999;
                    pointer-events: none;
                `;
                document.body.appendChild(data);
            }
            data.innerHTML = `
                <table>
                  <tr><td>documentHeight</td><td>${documentHeight.toFixed(1)}</td></tr>
                  <tr><td>windowHeight</td><td>${windowHeight.toFixed(1)}</td></tr>
                  <tr><td>scrollTop</td><td>${scrollTop.toFixed(1)}</td></tr>
                  <tr><td>pixelsAbove</td><td>${pixelsAbove.toFixed(1)}</td></tr>
                  <tr><td>pixelsBelow</td><td>${pixelsBelow.toFixed(1)}</td></tr>
                  <tr><td>bottomAdd</td><td>${bottomAdd.toFixed(1)}</td></tr>
                  <tr><td>adjustedBottomAdd</td><td>${adjustedBottomAdd.toFixed(1)}</td></tr>
                  <tr><td>scrollingDown</td><td>${scrollingDown}</td></tr>
                  <tr><td>threshold</td><td>${threshold.toFixed(1)}</td></tr>
                </table>
            `;
            drawDebugLine();
        }

        lastKnownScrollPosition = scrollTop;
    }

    function drawDebugLine() {
        if (!document.body) {
            return;
        }
        const id = 'mdbook-threshold-debug-line';
        const existingLine = document.getElementById(id);
        if (existingLine) {
            existingLine.remove();
        }
        const line = document.createElement('div');
        line.id = id;
        line.style.cssText = `
            position: fixed;
            top: ${threshold}px;
            left: 0;
            width: 100vw;
            height: 2px;
            background-color: red;
            z-index: 9999;
            pointer-events: none;
        `;
        document.body.appendChild(line);
    }

    function mdbookEnableThresholdDebug() {
        thresholdDebug = true;
        updateThreshold();
        drawDebugLine();
    }

    window.mdbookEnableThresholdDebug = mdbookEnableThresholdDebug;

    // Updates which headers in the sidebar should be expanded. If the current
    // header is inside a collapsed group, then it, and all its parents should
    // be expanded.
    function updateHeaderExpanded(currentA) {
        // Add expanded to all header-item li ancestors.
        let current = currentA.parentElement;
        while (current) {
            if (current.tagName === 'LI' && current.classList.contains('header-item')) {
                current.classList.add('expanded');
            }
            current = current.parentElement;
        }
    }

    // Updates which header is marked as the "current" header in the sidebar.
    // This is done with a virtual Y threshold, where headers at or below
    // that line will be considered the current one.
    function updateCurrentHeader() {
        if (!headers || !headers.length) {
            return;
        }

        // Reset the classes, which will be rebuilt below.
        const els = document.getElementsByClassName('current-header');
        for (const el of els) {
            el.classList.remove('current-header');
        }
        for (const toggle of headerToggles) {
            toggle.classList.remove('expanded');
        }

        // Find the last header that is above the threshold.
        let lastHeader = null;
        for (const header of headers) {
            const rect = header.getBoundingClientRect();
            if (rect.top <= threshold) {
                lastHeader = header;
            } else {
                break;
            }
        }
        if (lastHeader === null) {
            lastHeader = headers[0];
            const rect = lastHeader.getBoundingClientRect();
            const windowHeight = window.innerHeight;
            if (rect.top >= windowHeight) {
                return;
            }
        }

        // Get the anchor in the summary.
        const href = '#' + lastHeader.id;
        const a = [...document.querySelectorAll('.header-in-summary')]
            .find(element => element.getAttribute('href') === href);
        if (!a) {
            return;
        }

        a.classList.add('current-header');

        updateHeaderExpanded(a);
    }

    // Updates which header is "current" based on the threshold line.
    function reloadCurrentHeader() {
        if (disableScroll) {
            return;
        }
        updateThreshold();
        updateCurrentHeader();
    }


    // When clicking on a header in the sidebar, this adjusts the threshold so
    // that it is located next to the header. This is so that header becomes
    // "current".
    function headerThresholdClick(event) {
        // See disableScroll description why this is done.
        disableScroll = true;
        setTimeout(() => {
            disableScroll = false;
        }, 100);
        // requestAnimationFrame is used to delay the update of the "current"
        // header until after the scroll is done, and the header is in the new
        // position.
        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                // Closest is needed because if it has child elements like <code>.
                const a = event.target.closest('a');
                const href = a.getAttribute('href');
                const targetId = href.substring(1);
                const targetElement = document.getElementById(targetId);
                if (targetElement) {
                    threshold = targetElement.getBoundingClientRect().bottom;
                    updateCurrentHeader();
                }
            });
        });
    }

    // Takes the nodes from the given head and copies them over to the
    // destination, along with some filtering.
    function filterHeader(source, dest) {
        const clone = source.cloneNode(true);
        clone.querySelectorAll('mark').forEach(mark => {
            mark.replaceWith(...mark.childNodes);
        });
        dest.append(...clone.childNodes);
    }

    // Scans page for headers and adds them to the sidebar.
    document.addEventListener('DOMContentLoaded', function() {
        const activeSection = document.querySelector('#mdbook-sidebar .active');
        if (activeSection === null) {
            return;
        }

        const main = document.getElementsByTagName('main')[0];
        headers = Array.from(main.querySelectorAll('h2, h3, h4, h5, h6'))
            .filter(h => h.id !== '' && h.children.length && h.children[0].tagName === 'A');

        if (headers.length === 0) {
            return;
        }

        // Build a tree of headers in the sidebar.

        const stack = [];

        const firstLevel = parseInt(headers[0].tagName.charAt(1));
        for (let i = 1; i < firstLevel; i++) {
            const ol = document.createElement('ol');
            ol.classList.add('section');
            if (stack.length > 0) {
                stack[stack.length - 1].ol.appendChild(ol);
            }
            stack.push({level: i + 1, ol: ol});
        }

        // The level where it will start folding deeply nested headers.
        const foldLevel = 3;

        for (let i = 0; i < headers.length; i++) {
            const header = headers[i];
            const level = parseInt(header.tagName.charAt(1));

            const currentLevel = stack[stack.length - 1].level;
            if (level > currentLevel) {
                // Begin nesting to this level.
                for (let nextLevel = currentLevel + 1; nextLevel <= level; nextLevel++) {
                    const ol = document.createElement('ol');
                    ol.classList.add('section');
                    const last = stack[stack.length - 1];
                    const lastChild = last.ol.lastChild;
                    // Handle the case where jumping more than one nesting
                    // level, which doesn't have a list item to place this new
                    // list inside of.
                    if (lastChild) {
                        lastChild.appendChild(ol);
                    } else {
                        last.ol.appendChild(ol);
                    }
                    stack.push({level: nextLevel, ol: ol});
                }
            } else if (level < currentLevel) {
                while (stack.length > 1 && stack[stack.length - 1].level >= level) {
                    stack.pop();
                }
            }

            const li = document.createElement('li');
            li.classList.add('header-item');
            li.classList.add('expanded');
            if (level < foldLevel) {
                li.classList.add('expanded');
            }
            const span = document.createElement('span');
            span.classList.add('chapter-link-wrapper');
            const a = document.createElement('a');
            span.appendChild(a);
            a.href = '#' + header.id;
            a.classList.add('header-in-summary');
            filterHeader(header.children[0], a);
            a.addEventListener('click', headerThresholdClick);
            const nextHeader = headers[i + 1];
            if (nextHeader !== undefined) {
                const nextLevel = parseInt(nextHeader.tagName.charAt(1));
                if (nextLevel > level && level >= foldLevel) {
                    const toggle = document.createElement('a');
                    toggle.classList.add('chapter-fold-toggle');
                    toggle.classList.add('header-toggle');
                    toggle.addEventListener('click', () => {
                        li.classList.toggle('expanded');
                    });
                    const toggleDiv = document.createElement('div');
                    toggleDiv.textContent = '❱';
                    toggle.appendChild(toggleDiv);
                    span.appendChild(toggle);
                    headerToggles.push(li);
                }
            }
            li.appendChild(span);

            const currentParent = stack[stack.length - 1];
            currentParent.ol.appendChild(li);
        }

        const onThisPage = document.createElement('div');
        onThisPage.classList.add('on-this-page');
        onThisPage.append(stack[0].ol);
        const activeItemSpan = activeSection.parentElement;
        activeItemSpan.after(onThisPage);
    });

    document.addEventListener('DOMContentLoaded', reloadCurrentHeader);
    document.addEventListener('scroll', reloadCurrentHeader, { passive: true });
})();

