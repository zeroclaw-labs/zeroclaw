# Dépannage (Français)

Cette page est une localisation initiale Wave 1 pour diagnostiquer rapidement les pannes courantes.

Source anglaise:

- [../../troubleshooting.md](../../troubleshooting.md)

## Quand l'utiliser

- Échecs d'installation ou de démarrage
- Diagnostic guidé via `status` et `doctor`
- Procédures minimales de récupération/rollback

## Règle

- Les codes d'erreur, clés de logs et commandes restent en anglais.
- Les signatures de panne détaillées sont définies en anglais.

## Notes de mise à jour

### Erreur `403`/`429` dans `web_search_tool`

**Symptôme** : Un message tel que `DuckDuckGo search failed with status: 403` (ou `429`) apparaît.

**Cause** : Certains réseaux/proxies bloquent l\u2019endpoint HTML de DuckDuckGo.

**Options de correction** :

1. Passer à Brave :
```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. Passer à Exa :
```toml
[web_search]
enabled = true
provider = "exa"
api_key = "<SECRET>"
# optionnel
# api_url = "https://api.exa.ai/search"
```

3. Passer à Tavily :
```toml
[web_search]
enabled = true
provider = "tavily"
api_key = "<SECRET>"
# optionnel
# api_url = "https://api.tavily.com/search"
```

4. Passer à Firecrawl (si inclus dans le build) :
```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

### `curl`/`wget` bloqués dans le shell tool

**Symptôme** : La sortie contient `Command blocked: high-risk command is disallowed by policy`.

**Cause** : `curl`/`wget` sont bloqués par la politique d\u2019autonomie en tant que commandes à haut risque.

**Correction** : Utiliser des outils dédiés à la place du shell fetch :
- `http_request` : appels API/HTTP directs
- `web_fetch` : extraction et résumé du contenu de page

Configuration minimale :
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_fetch`/`http_request` — Hôte non autorisé

**Symptôme** : Une erreur du type `Host '<domain>' is not in http_request.allowed_domains` apparaît.

**Correction** : Ajouter le domaine à la liste ou utiliser `"*"` pour l\u2019accès public :
```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

**Note de sécurité** : Les réseaux locaux/privés restent bloqués même avec `"*"`.
