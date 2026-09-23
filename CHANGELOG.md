# Journal des modifications

Les sections de ce fichier alimentent directement les notes de version
publiées sur GitHub : `npm run release <version>` reprend la section portant
le numéro publié.

## [Non publié]

### Nouveautés

- **Ligne de temps illustrée dans le montage** : bande d'images extraite de la
  vidéo et **forme d'onde** de la piste sonore, sur la même échelle de temps,
  zoomable jusqu'à ×60. On coupait jusqu'ici à l'aveugle sur un ruban vide.
- **Repères au glissement**, aimantés aux silences repérés : `Maj` + glisser
  retire un passage, `Alt` désactive l'aimantation, les poignées de début et de
  fin se déplacent directement sur le ruban.
- **Retrait automatique des blancs** : les silences de plus de 0,8 s sont
  détectés et retirés d'un clic, une marge de confort préservée aux deux bouts.
- **Aperçu du montage** : la lecture saute les passages retirés, applique les
  fondus et montre le carton de titre, sans attendre le rendu.
- **Annuler / rétablir** la découpe (`Ctrl+Z`, `Ctrl+Maj+Z`) et **raccourcis
  clavier** de montage (`espace`, `←`/`→` à l'image près, `I`, `O`, `Suppr`).
- **Coupe exacte à l'image**, en option : la découpe par copie de flux glisse
  jusqu'à l'image-clé la plus proche, ce que l'interface passait sous silence.
  Le choix est désormais explicite, son coût annoncé.
- Le profil de bruit peut être **trouvé automatiquement** à partir du plus long
  blanc de l'enregistrement.
- **Passages muets** : couper le son sans couper l'image, tracés sur le ruban
  comme les passages retirés (outil « Rendre muet », touche `M`).
- **Volume et normalisation EBU R128** de la piste sonore. Avec les passages
  muets et le débruitage, le tout tient en **une seule passe qui ne réencode
  que le son**.
- **Vitesse** de ×0,5 à ×2, la hauteur de la voix étant préservée ; l'aperçu
  joue à la vitesse choisie.
- **Recadrage** au cadre manipulable directement sur l'aperçu, libre ou en
  16:9, 1:1, 9:16.
- **Carton de fin**, réglé comme le carton d'ouverture.
- **Export en GIF animé** : cadence et largeur réglables, palette calculée sur
  les images réelles plutôt que fixée d'avance. Les GIF s'affichent dans le
  lecteur intégré au lieu d'être confiés à une balise vidéo qui ne sait pas
  les lire.

- **Montage des enregistrements** : découper (début, fin, et suppression de
  passages au milieu), **réduire le bruit de fond**, ajouter un **carton de
  titre** en ouverture et des **fondus**. Une découpe seule se fait par copie
  de flux — instantanée et sans perte ; la réduction de bruit ne réencode que
  le son ; seuls le titre et les fondus imposent un réencodage de l'image.
- Le bruit de fond est retiré à partir d'un **profil relevé sur un passage
  silencieux** que vous désignez, comme dans un éditeur audio.
- Les enregistrements comportent désormais **une image-clé par seconde**, pour
  que les coupes sans réencodage tombent juste (+1,5 % de taille seulement).

- **Pilotage de l'incrustation en direct** : masquer ou afficher la caméra,
  changer de coin, changer de forme, l'agrandir, ou **échanger les rôles**
  (caméra en grand, capture en médaillon) — sans interrompre l'enregistrement.
  Raccourcis `Ctrl+Maj+H / X / L / F / + / -`.
- **Décompte avant démarrage** (3, 5 ou 10 secondes, ou aucun), suivi sans
  interruption visuelle jusqu'à la première image filmée.
- **Retour à l'écran pendant l'enregistrement** : l'indicateur flottant
  affiche la présentation en cours (coin, forme, taille, caméra masquée,
  rôles échangés) et met en avant le dernier ajustement au clavier.
- L'indicateur se place **hors de la zone filmée** quand le bureau le permet,
  pour ne pas s'incruster dans la vidéo.
- **Choix de l'écran** à filmer en configuration multi-moniteurs.
- **Retour après l'arrêt** : le fichier produit s'ouvre dans le lecteur, avec
  sa durée, son poids et la cadence réellement atteinte.
- **Son du système** enregistrable, seul ou **mélangé au microphone** : utile
  pour garder à la fois sa voix et celle des autres participants d'une réunion.
- **Installation assistée de ffmpeg** depuis l'application, via le
  gestionnaire de paquets du système.
- **Lecteur vidéo intégré** et vignettes extraites des enregistrements.
- Capture d'écran **native** (`x11grab` / `gdigrab` / `avfoundation`), avec le
  pointeur de souris.
- **Encodage matériel** automatique (VAAPI, VideoToolbox) quand il est
  disponible.
- **Arrière-plans décoratifs** : capture arrondie posée sur un dégradé.
- Choix de la résolution de sortie et affichage de la cadence réelle.

### Corrections

- Deux montages menés de front partageaient le même dossier de travail : le
  nettoyage du premier emportait les fichiers intermédiaires du second, qui
  échouait.
- Un glissement commencé sur le ruban de montage et relâché à côté de la
  fenêtre fermait l'éditeur, perdant la découpe en cours.
- Les vidéos ne sont plus accélérées : la durée du fichier correspond au temps
  réellement écoulé.
- Deux clics rapprochés sur « Démarrer » ne lancent plus deux enregistrements
  concurrents, dont l'un restait orphelin.
- Fermer l'application pendant une capture finalise le fichier au lieu de le
  laisser illisible.
- Les entrées audio sont de nouveau détectées (aucune ne l'était).
- Le message « installez ffmpeg » ne s'affiche plus pendant la détection.
- Les erreurs de ffmpeg sont remontées telles quelles au lieu d'être perdues.
- « Device or resource busy » : l'aperçu ne peut plus rouvrir la caméra pendant
  qu'un enregistrement la réclame.
- L'application n'apparaît plus dans les premières images de la vidéo : la
  fenêtre est retirée avant que ffmpeg ne commence à filmer.
- **ffmpeg ne plante plus** lors d'un ajustement en direct (corruption de tas,
  fichier inexploitable) : un seul filtre du graphe est désormais
  redimensionnable, et il alimente un `overlay`, non un `alphamerge`.
- Le médaillon caméra est carré : passer du cercle au carré en direct ne
  déforme plus le visage.
- **La fenêtre se restaure** depuis la zone de notification : la croix la
  masque désormais au lieu de la détruire. On quitte par « Quitter ».
- **Le raccourci d'arrêt fonctionne toujours** : il est traité directement
  côté Rust, sans dépendre d'une fenêtre visible.
- **Le lecteur intégré lit les vidéos** : le moteur multimédia de WebKitGTK ne
  sait pas charger un schéma d'URL personnalisé ; le fichier lui est désormais
  fourni sous forme de blob.
- **L'outil Texte fonctionne** : cliquer ailleurs ouvrait un nouveau brouillon
  avant que la validation du précédent ne s'exécute, et le texte saisi était
  perdu sans message.

### Performances

- Capture d'écran : de ~3 à plus de 23 images par seconde.
- Détection des périphériques au démarrage : de 5,9 s à environ 1,3 s.
- Génération des visuels décoratifs : six fois plus rapide.
- Suppression des filtres `geq` et `boxblur`, qui coûtaient à eux seuls dix
  fois le temps réel.

## [1.0.0]

Première version : capture d'écran, annotation, OCR, export et enregistrement
vidéo.
