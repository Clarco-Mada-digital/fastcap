# FastCap

Application de capture d'écran desktop complète — capture, annotation, OCR, gestion
et export — construite avec **Tauri 2** (Rust) et **React + TypeScript**.

Buildable sur **Windows, macOS et Linux** depuis la même base de code.

---

## Fonctionnalités

### Capture
- **Plein écran** — capture instantanée du moniteur principal (`PrintScreen`)
- **Région** — sélection au rectangle sur une image d'écran figée (`Ctrl+Shift+R`)
  - l'écran est gelé avant la sélection : le résultat correspond exactement à ce que vous voyez
  - dimensions affichées en direct, `Entrée` pour valider, `Échap` pour annuler
- **Fenêtre** — liste des fenêtres ouvertes avec titre, application et dimensions (`Ctrl+Shift+W`)
- **Multi-écrans** — chaque moniteur est détecté ; la région est découpée dans l'image du bon écran
- **Auto-sauvegarde** — enregistrement automatique dans un dossier configurable
- **Copie automatique** dans le presse-papiers (option)
- **Zone de notification** — afficher l'app, lancer une capture, quitter

### Annotation
- Rectangle, ellipse, ligne, flèche, texte, **flou de masquage**
- Couleur et épaisseur réglables, superposition non destructive (liste d'annotations éditable)
- Rendu fidèle à l'aperçu lors de l'export

### OCR
- Extraction de texte + confiance et nombre de mots détectés
- S'appuie sur le binaire `tesseract` s'il est présent (`fra+eng` par défaut)
- Si tesseract est absent, l'interface l'indique clairement au lieu d'échouer silencieusement

### Enregistrement vidéo
- Sources : **plein écran**, **zone** (sélection au rectangle) ou **fenêtre**
- **Choix de l'écran** en configuration multi-moniteurs
- Capture **native** (`x11grab` / `gdigrab` / `avfoundation`) : la cadence demandée
  est réellement tenue et le **pointeur de souris** est inclus
- **Encodage matériel** automatique quand la machine le permet (VAAPI sous Linux,
  VideoToolbox sous macOS) ; sinon `libx264` avec un préréglage choisi selon la
  charge — l'encodeur retenu est affiché dans l'interface
- **Résolution de sortie** réglable : 480p / 720p / 1080p / native
- **Arrière-plans décoratifs** : la capture est réduite, arrondie et posée sur un
  dégradé (Aurora, Sunset, Mint, Slate, Cream)
- **Incrustation de la webcam** avec 7 dispositions : 4 coins, côte à côte
  (gauche/droite), caméra plein cadre — 3 formes (carré, arrondi, cercle) et
  4 présentations (minimal, cadre + ombre, halo studio, bulle)
- Aperçu caméra en direct et **aperçu de l'incrustation** fidèle au rendu final
- 15 / 24 / 30 / 60 images par seconde, trois profils de qualité
- **Son** : microphone **ou son du système** (les sorties haut-parleurs sont
  proposées comme sources, pour enregistrer ce que joue la machine)
- **Lecteur intégré** : chaque enregistrement se relit dans l'application, avec
  une vignette extraite de la vidéo — aucun lecteur externe requis
- Cadence réelle affichée en direct, avec alerte si les réglages sont trop lourds
- Fenêtre masquée pendant la capture, arrêt depuis la zone de notification ou `Ctrl+Shift+E`
- Encodage **H.264 / MP4** (compatible partout), liste des enregistrements avec
  durée, résolution, poids ; lecture et suppression depuis l'application

### Montage
- **Ligne de temps illustrée** : bande d'images extraites de la vidéo et
  **forme d'onde** de la piste sonore sur la même échelle — on voit où l'on
  parle et où l'on se tait avant de couper. Zoom jusqu'à ×60 (`Ctrl` + molette)
  pour viser à l'image près.
- **Découpe** : choisir le début et la fin, retirer un ou plusieurs passages
  au milieu. Les repères se posent au glissement (`Maj` + glisser pour retirer
  un passage) et s'**aimantent** aux silences repérés ; `Alt` désactive
  l'aimantation.
- **Retrait automatique des blancs** : les silences de plus de 0,8 s sont
  repérés et peuvent être retirés d'un clic, une marge de confort préservée de
  part et d'autre.
- **Aperçu du montage** : la lecture saute les passages retirés, applique les
  fondus et montre le carton de titre — on juge le résultat sans attendre le
  rendu.
- **Annuler / rétablir** (`Ctrl+Z`, `Ctrl+Maj+Z`) sur toute la découpe.
- **Clavier** : `espace` lecture, `←`/`→` image par image (`Maj` : une
  seconde), `I` / `O` début et fin, `Suppr` rétablit un passage retiré,
  `Début` / `Fin` sautent aux bornes du montage.
- **Deux régimes de coupe** : par copie de flux, instantanée et sans perte,
  mais alignée sur l'image-clé la plus proche ; ou **exacte à l'image**, au
  prix d'un réencodage des passages conservés. Le choix est explicite, avec son
  coût annoncé.
- **Réduction du bruit de fond** : on désigne un passage silencieux — ou on
  laisse FastCap prendre le plus long blanc repéré — il en relève le profil et
  retire ce bruit de toute la piste.
- **Passages muets** : garder l'image et couper le son sur un intervalle, sans
  rien retirer du montage (outil « Rendre muet », touche `M`).
- **Volume et normalisation** : gain manuel, ou mise au niveau de diffusion
  (EBU R128, −16 LUFS).
- Ces trois traitements tiennent dans **une seule passe qui ne réencode que le
  son** : l'image reste intacte.
- **Vitesse** de ×0,5 à ×2, la voix gardant sa hauteur.
- **Recadrage** au cadre manipulable sur l'aperçu, libre ou en 16:9, 1:1, 9:16.
- **Cartons d'ouverture et de fin** (titre, sous-titre, dégradé, durée).
- **Fondus** d'ouverture et de fermeture.
- **Export en GIF animé**, cadence et largeur réglables, palette calculée sur
  les images réelles.
- Avancement affiché pendant le rendu ; le montage rejoint la liste des
  enregistrements sans écraser l'original.

### Gestion et export
- Historique persistant (200 dernières captures) avec **vignettes réelles**
- Rouvrir une capture dans l'éditeur, la supprimer (fichier inclus)
- Export **PNG** (sans perte) et **JPEG** (qualité réglable)
- Chemins par défaut : images dans `Images/FastCap`, réglages dans le dossier de config système

---

## Prérequis

| Outil | Version |
| --- | --- |
| Node.js | ≥ 18 |
| Rust | ≥ 1.77 |
| Tauri CLI | 2.x (`npm run tauri`) |

### Dépendances système

- **Windows** : WebView2 (installé par défaut sur Windows 10/11)
- **macOS** : Xcode Command Line Tools
- **Linux** : `webkit2gtk-4.1`, `libappindicator3`, `librsvg2`, `libxcb`, `libxdo`
  ```bash
  # Debian / Ubuntu
  sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
       libxcb1-dev libxdo-dev libssl-dev
  ```

### OCR (optionnel)

```bash
sudo apt install tesseract-ocr tesseract-ocr-fra   # Linux
brew install tesseract tesseract-lang              # macOS
winget install UB-Mannheim.TesseractOCR            # Windows
```

Le chemin du binaire peut être forcé avec la variable `FASTCAP_TESSERACT`.
Si tesseract est absent, l'application l'indique et le reste continue de fonctionner.

### Enregistrement vidéo

ffmpeg est **indispensable** pour enregistrer. Si FastCap ne le trouve pas, il
propose de l'installer lui-même : le panneau « Dépendances » détecte le
gestionnaire de paquets (apt, dnf, pacman, zypper, winget, Homebrew), affiche
la commande exacte qui sera lancée, puis la exécute — le système demande alors
l'authentification.

Installation manuelle, si préférée :

```bash
sudo apt install ffmpeg        # Linux
brew install ffmpeg            # macOS
winget install Gyan.FFmpeg     # Windows
```

Pour relire les vidéos **dans** l'application, le moteur d'affichage a besoin
d'un décodeur H.264 système :

```bash
sudo apt install gstreamer1.0-libav   # Linux
```

Sans lui, le lecteur intégré propose de basculer vers le lecteur du système.

ffmpeg assure la capture d'écran, l'encodage H.264 et l'incrustation de la webcam :
il n'y a aucune bibliothèque vidéo à lier, et le build reste identique sur les
trois systèmes.

---

## Démarrage

```bash
npm install
npm run tauri dev
```

> `tauri dev` produit un binaire **non optimisé** : la capture, la composition
> et l'encodage y sont plusieurs fois plus lents, et l'application consomme
> nettement plus de mémoire. Pour juger des performances réelles, utilisez
> toujours une version compilée en mode release :
>
> ```bash
> npm run tauri build     # puis lancer le binaire produit
> ```

## Build de production

```bash
npm run tauri build
```

Les installeurs sont générés dans `src-tauri/target/release/bundle/`.

## Publier une version

```bash
npm run release 1.2.0
npm run release 1.2.0 -- --dry-run   # simulation, n'écrit ni ne pousse rien
```

Le script aligne le numéro de version dans `package.json`, `Cargo.toml` et
`tauri.conf.json`, lance les tests, commite, pose le tag `v1.2.0` et le pousse.
Le tag déclenche le workflow GitHub Actions, qui compile **en parallèle** :

| Plateforme | Formats |
| --- | --- |
| Windows | `.msi`, `.exe` |
| macOS (Apple Silicon et Intel) | `.dmg` |
| Linux | `.AppImage`, `.deb` |

La note de version est reprise de la section correspondante de `CHANGELOG.md` ;
à défaut, elle est reconstruite depuis les commits depuis le tag précédent,
regroupés par type (`feat:`, `fix:`, `perf:`, `docs:`).

> Le dépôt doit avoir un distant `origin` sur GitHub. Aucun secret n'est à
> configurer : le workflow utilise le `GITHUB_TOKEN` fourni automatiquement.

---

## Raccourcis

| Raccourci | Action |
| --- | --- |
| `PrintScreen` | Capture plein écran |
| `Ctrl+Shift+R` | Capture de région |
| `Ctrl+Shift+W` | Capture de fenêtre |
| `Ctrl+Shift+S` | Afficher FastCap |
| `Ctrl+Shift+E` | Arrêter l'enregistrement en cours |
| `Entrée` / `Échap` | Valider / annuler la sélection de région |

### Pendant l'enregistrement

Ces raccourcis agissent **sans interrompre** la capture ni le son : ffmpeg
accepte de modifier ses filtres en cours de route.

| Raccourci | Action |
| --- | --- |
| `Ctrl+Maj+H` | Masquer / afficher la caméra |
| `Ctrl+Maj+X` | Échanger les rôles : caméra en grand, capture en médaillon |
| `Ctrl+Maj+L` | Coin suivant |
| `Ctrl+Maj+F` | Forme suivante (carré, arrondi, cercle) |
| `Ctrl+Maj++` / `Ctrl+Maj+-` | Agrandir / réduire l'incrustation |

> Les dispositions « côte à côte » et « caméra plein cadre » modifient la
> structure du montage : elles se choisissent avant de lancer la capture.

---

## Architecture

```
src/                      Interface React
  App.tsx                 Fenêtre principale (accueil, prévisualisation, réglages)
  main.tsx                Routage : fenêtre "main" ou "region-overlay"
  components/
    RegionSelector.tsx    Sélection de région sur image figée
    PreviewPanel.tsx      Aperçu, annotations, OCR, export
    AnnotationCanvas.tsx  Canvas d'annotation
    RecordingPanel.tsx    Enregistrement vidéo et webcam
    Settings.tsx          Préférences
  hooks/useApp.ts         Accès typé aux commandes Tauri

src-tauri/src/            Backend Rust
  lib.rs                  Plugins, état, raccourcis globaux, zone de notification
  commands.rs             Commandes exposées au frontend
  capture.rs              Capture écran/fenêtre et énumération (xcap)
  recorder.rs             Enregistrement vidéo : assemble la commande ffmpeg
  screen_input.rs         Capture d'écran native par plateforme
  webcam.rs               Source webcam partagée (aperçu + enregistrement)
  encoder.rs              Choix de l'encodeur (matériel si possible)
  visuals.rs              Masques, ombres et arrière-plans pré-rendus
  compositor.rs           Composition des aperçus (même géométrie que la vidéo)
  process_util.rs         Exécution bornée dans le temps, capture de stderr
  image_utils.rs          Annotations, flou, encodage PNG/JPEG
  ocr.rs                  OCR via tesseract
  state.rs                Réglages, historiques, persistance
```

### Notes techniques

L'enregistrement repose sur **un seul processus ffmpeg** qui capture, compose et
encode. Trois décisions structurent le module, chacune motivée par une mesure
faite sur une machine modeste (2 cœurs) :

- **La capture est native.** Une boucle Rust poussant des images brutes dans un
  tube plafonnait à ~3 images/s (6,6 Mo par image à recopier), et produisait des
  vidéos accélérées : la cadence annoncée à ffmpeg n'était jamais tenue, si bien
  que 3 s de réel donnaient 1 s de vidéo. `x11grab` tient 24 images/s et horodate
  lui-même, donc la durée est exacte par construction.
- **Les décorations sont pré-rendues.** Les masques (cercle, coins arrondis), les
  ombres et les arrière-plans sont calculés une fois en PNG puis incrustés par
  `alphamerge` / `overlay`. Les filtres `geq` et `boxblur` employés auparavant
  évaluaient une expression par pixel et par image : mesurés à ~10x le temps réel
  en 1080p30, ils rendaient tout enregistrement fluide impossible.
- **Les erreurs sont lisibles.** La sortie d'erreur de ffmpeg est conservée et
  jointe aux messages ; elle était auparavant redirigée vers `/dev/null`, ce qui
  réduisait tout échec à un laconique « l'encodage n'a produit aucun fichier ».

Autres points :

- `xcap 0.4` reste utilisé pour les **captures ponctuelles** et l'énumération des
  fenêtres, où son coût (~60 ms par image) est sans importance. Les versions ≥ 0.5
  tirent `pipewire` sur Linux, ce qui imposerait `libpipewire-0.3-dev` au build.
- La webcam est ouverte **une seule fois** et partagée entre l'aperçu et
  l'enregistrement ; l'aperçu la relâche automatiquement au démarrage d'un
  enregistrement, le périphérique v4l2 étant exclusif.
- Le rendu du texte est assuré par le canvas du frontend ; le backend applique
  les autres annotations pour l'export côté Rust.
- L'icône peut être régénérée : `python3 scripts/generate-icon.py` puis
  `npx tauri icon src-tauri/icons/source.png`.

### Tests

```bash
cargo test --manifest-path src-tauri/Cargo.toml            # unitaires
cargo test --manifest-path src-tauri/Cargo.toml --release -- --ignored
```

Les tests `--ignored` réalisent de **vrais enregistrements** (écran, webcam et
ffmpeg requis) et vérifient que la durée du fichier correspond au temps écoulé
et que la cadence demandée est tenue.
