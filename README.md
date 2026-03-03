<p align="center">
  <img src="website/img/ygege-logo-text.png" alt="Logo Ygégé" width="400"/>
</p>

Indexeur haute performance pour YGG Torrent écrit en Rust

## [AVERTISSEMENT LÉGAL](DISCLAIMER-fr.md)

> **Fork Scrappey** — Remplacement de FlareSolverr par [Scrappey](https://scrappey.com)
>
> Ce fork remplace l'intégration FlareSolverr par **Scrappey**, un service cloud de bypass Cloudflare plus fiable et sans infrastructure à maintenir (pas de conteneur headless Chrome).
>
> Basé sur [UwUDev/ygege](https://github.com/UwUDev/ygege) (fork intermédiaire : [Obijc/ygege](https://github.com/Obijc/ygege)).

---

## Caractéristiques principales

- ⚡ Recherche quasi instantanée
- 🔒 Bypass Cloudflare automatisé via émulation TLS/HTTP2 ([wreq](https://crates.io/crates/wreq))
- 🛡️ **Fallback Scrappey** automatique si le bypass `wreq` échoue
- 🔄 Résolution automatique du domaine actuel de YGG Torrent
- 🔁 Reconnexion transparente aux sessions expirées + cache de sessions
- 🌐 Contournement des DNS menteurs (fallback Cloudflare DNS)
- 💾 Consommation mémoire faible (~15 Mo en release sur Linux)
- 🔍 Recherche modulaire (nom, seed, leech, commentaires, date, etc.)
- 📦 Aucune dépendance externe locale, aucun driver de navigateur

---

## Différences avec le projet original

| | [UwUDev/ygege](https://github.com/UwUDev/ygege) | Ce fork |
|---|---|---|
| Bypass CF primaire | `wreq` (émulation TLS Chrome 132) | Identique |
| Bypass CF fallback | Aucun | **Scrappey** (cloud) |
| Login sous CF | Échoue si challenge CF actif | **browserActions** : résolution automatique du challenge puis login |
| Infrastructure | Aucune | Aucune (Scrappey = SaaS) |
| Cookies auth | Non persistés entre requêtes | **Cookiejar** : injection automatique dans chaque requête Scrappey |

---

## Installation rapide (Docker)

### 1. Avec Docker Compose

```bash
git clone https://github.com/tiff75009/ygege-scrappey.git
cd ygege-scrappey/docker
```

Éditez `compose.yml` avec vos identifiants :

```yaml
services:
  ygege:
    image: ghcr.io/tiff75009/ygege-scrappey:latest
    environment:
      YGG_USERNAME: "votre_username"
      YGG_PASSWORD: "votre_password"
      SCRAPPEY_API_KEY: "votre_clé_scrappey"  # Obtenir sur https://scrappey.com
    ports:
      - "8715:8715"
```

```bash
docker compose up -d
```

### 2. Obtenir une clé API Scrappey

1. Créez un compte sur [scrappey.com](https://scrappey.com)
2. Ajoutez du crédit (à partir de ~2€, soit ~10 000 requêtes)
3. Copiez votre clé API dans `SCRAPPEY_API_KEY`

> **Coût** : ~0.0002€ par requête. Le fallback Scrappey n'est utilisé que lorsque `wreq` est bloqué par Cloudflare. En fonctionnement normal, la majorité des requêtes passent via `wreq` (gratuit).

---

## Configuration

| Variable | Description | Défaut |
|---|---|---|
| `YGG_USERNAME` | Identifiant YGG *(obligatoire)* | — |
| `YGG_PASSWORD` | Mot de passe YGG *(obligatoire)* | — |
| `SCRAPPEY_API_KEY` | Clé API Scrappey *(recommandé)* | — |
| `BIND_IP` | IP d'écoute | `0.0.0.0` |
| `BIND_PORT` | Port d'écoute | `8715` |
| `LOG_LEVEL` | Niveau de log (`off`, `error`, `warn`, `info`, `debug`, `trace`) | `debug` |
| `TMDB_TOKEN` | Token API TMDB (optionnel, pour recherche TMDB/IMDB) | — |
| `YGG_DOMAIN` | Forcer un domaine YGG spécifique | auto-détecté |
| `TURBO_ENABLED` | Mode turbo (réduit le timer de téléchargement) | `false` |

---

## Comment ça marche

### Bypass Cloudflare

1. **Cookie magique** : Ygege injecte `account_created=true` pour tenter de désactiver le challenge CF initial
2. **Émulation TLS/HTTP2** : Via [wreq](https://crates.io/crates/wreq), reproduction fidèle du fingerprint Chrome 132
3. **Fallback Scrappey** : Si `wreq` est bloqué (HTTP 307/302/403/503), Scrappey prend le relais via un vrai navigateur cloud

### Login avec Scrappey

Quand Cloudflare bloque la page de login :
1. Scrappey ouvre la page de login dans un navigateur réel
2. Résout automatiquement le challenge CF / Turnstile
3. **Après résolution** (`after_captcha`), remplit le formulaire avec simulation de frappe humaine
4. Soumet avec Enter et attend la stabilisation réseau (`networkidle`)
5. Les cookies authentifiés sont stockés globalement et injectés via `cookiejar` dans toutes les requêtes suivantes

### Architecture des requêtes

```
Requête HTTP
    │
    ├─► wreq (émulation TLS Chrome 132)
    │       │
    │       ├─► Succès → Réponse directe
    │       │
    │       └─► Bloqué par CF (307/302/403/503)
    │               │
    │               └─► Scrappey (navigateur cloud)
    │                       │
    │                       ├─► cookiejar (cookies auth injectés)
    │                       ├─► User-Agent (matching login CF)
    │                       └─► Réponse via navigateur réel
    │
    └─► Réponse finale
```

> [!WARNING]
> L'émulation `wreq` ne fonctionne plus à partir de Chrome 133+ (HTTP/3). Scrappey assure la continuité du service.

---

## Intégration Prowlarr / Jackett

### Prowlarr

Copiez `ygege.yml` dans `{appdata prowlarr}/Definitions/Custom/`, puis redémarrez Prowlarr.

> [!NOTE]
> URL par défaut : `http://localhost:8715/`. En Docker Compose : `http://ygege:8715/`.

### Jackett

Copiez `ygege.yml` dans `{appdata jackett}/cardigann/definitions/`, puis redémarrez Jackett.

---

## Compilation locale

### Prérequis

- Rust 1.85.0+
- OpenSSL 3+
- Dépendances de [wreq](https://crates.io/crates/wreq)

```bash
cargo build --release
```

Ou utilisez Docker : voir le [Guide Docker](docs/build-docker-linux.md).

---

## Crédits

- **[UwUDev/ygege](https://github.com/UwUDev/ygege)** — Projet original
- **[Obijc/ygege](https://github.com/Obijc/ygege)** — Fork intermédiaire (intégration FlareSolverr)
- **[Scrappey](https://scrappey.com)** — Service de bypass Cloudflare
- **[wreq](https://crates.io/crates/wreq)** — Client HTTP avec émulation TLS

## Licence

Identique au projet original.
