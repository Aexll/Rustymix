<p align="center">
  <img src="assets/icons/logo-256.png" width="160" alt="Logo de Rustymix : un crabe DJ au casque, les pinces sur deux platines">
</p>

<h1 align="center">Rustymix</h1>

<p align="center">Application de mix DJ en Rust, pour Windows et Ubuntu.</p>

- **Deux platines** : formes d'onde colorées, marqueurs, SYNC, MATCH (tonalité), KEY LOCK,
  MAGNET, EQ 3 bandes avec kills, filtre passe-bas / passe-haut, boucles calées sur les temps.
- **Stems** : voix, batterie, basse et instruments séparés par l'IA (Demucs), chacun avec son
  fader.
- **Transitions automatiques** : éditeur de nœuds façon Blueprint (Unreal Engine), déclenché
  par les marqueurs du morceau ou à la main, avec délais en secondes ou en temps.
- **Bibliothèque** : analyse du tempo, de la tonalité et de l'énergie ; suggestions des
  morceaux qui s'enchaînent le mieux avec celui qui joue.
- **Installation de sons** : recherche YouTube / Spotify, et recommandations de morceaux
  similaires.

## Télécharger

Les versions compilées sont sur la page
[Releases](https://github.com/Aexll/Rustymix/releases).

**Windows** : `rustymix-setup-x64.exe`, l'installateur. Il installe Rustymix pour
l'utilisateur courant (sans droits administrateur ; « pour tous les utilisateurs » est
proposé), ajoute un raccourci au menu Démarrer (et au bureau si on le coche), et apparaît
dans « Programmes et fonctionnalités » pour la désinstallation. Une case facultative installe
aussi `yt-dlp` et FFmpeg avec winget, pour l'installation de sons. Les réglages, la
bibliothèque et les transitions sont conservés à la désinstallation. Sans installation :
`rustymix-windows-x64.zip`, à décompresser, puis lancer `rustymix.exe`.

**Ubuntu** (22.04 ou plus récent) : le paquet `.deb`, qui ajoute Rustymix au menu des
applications :

```bash
sudo apt install ./rustymix_0.2.0_amd64.deb
```

Sans installation : `rustymix-linux-x64.tar.gz`, à décompresser, puis lancer `./rustymix`
(`./install.sh` l'ajoute au menu des applications, dans `~/.local`).

Fonctions facultatives, qui demandent des outils externes :

- installer des sons : [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) et `ffmpeg` dans le
  PATH (et `spotdl` pour Spotify) ; sous Ubuntu : `sudo apt install ffmpeg yt-dlp` ;
- séparer les stems : Python 3 avec PyTorch et Demucs (voir plus bas).

## Compiler et lancer

```bash
cargo run --release
```

Sous Ubuntu, installer d'abord les en-têtes nécessaires (son ALSA, fenêtre X11 / Wayland) :

```bash
sudo apt install build-essential pkg-config libasound2-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

Pour les stems (facultatif) : Python 3 avec PyTorch (version CUDA pour une carte
NVIDIA, voir pytorch.org), puis :

```bash
pip install demucs soundfile
```

Le modèle (environ 80 Mo) est téléchargé à la première séparation. La variable
d'environnement `RUSTYMIX_PYTHON` permet de choisir l'interpréteur Python.

L'interface est dessinée en OpenGL : si la carte graphique manque de mémoire ou se
réinitialise, l'affichage continue au lieu de planter. `RUSTYMIX_RENDERER=wgpu` revient au
rendu wgpu.

On peut aussi passer un ou deux fichiers en argument pour les charger au démarrage
(le premier sur la platine A, le second sur la B) :

```bash
cargo run --release -- morceau_a.mp3 morceau_b.flac
```

Formats lus : MP3, FLAC, WAV, OGG Vorbis, AAC/M4A, ALAC, AIFF, CAF.

## Utilisation

- **Panneaux et barre d'activité** : sous les platines, une barre verticale à icônes (comme
  celle de VS Code) choisit le panneau affiché à la place de la liste des morceaux :
  **Bibliothèque**, **Transitions** (éditeur de nœuds), **Installer un son** (raccourcis
  `Ctrl + 1` à `Ctrl + 3`). L'icône en haut à droite d'un panneau le détache dans une
  fenêtre flottante (la bibliothèque reprend sa place) ; l'icône de la fenêtre, un clic sur
  son icône dans la barre ou la fermeture de la fenêtre le remet en place. Les icônes
  portent une pastille : verte quand une transition s'exécute, rouge pendant un
  enregistrement, bleue avec le nombre de téléchargements en cours.
- **Bibliothèque** : « Ajouter un dossier… » puis glisser un morceau vers une platine,
  ou le sélectionner et faire `Maj + ←` / `Maj + →`. Clic sur un en-tête de colonne pour
  trier, champ de recherche pour filtrer. Une platine en lecture refuse tout chargement.
  Les morceaux déjà lancés sur une platine pendant la session apparaissent assombris.
- **Suggestions** (bouton aux deux cercles, à gauche de la barre de la bibliothèque) :
  classe les morceaux selon leur compatibilité avec celui d'une platine — par défaut celle
  qui joue, ou celle choisie avec les boutons A / B à côté (un second clic revient à la
  platine qui joue). Tout se calcule en local, à partir de l'analyse des morceaux :
  - **tempo** : correction à faire au fader pour se caler sur le tempo réellement joué
    (« +2% »), au double ou à la moitié si besoin (« ½ », « ×2 ») ; en rouge au-delà de
    ±12 % ;
  - **tonalité** (pastille) : verte pour la même tonalité, vert pâle pour une tonalité
    compatible (roue Camelot), jaune si elle le devient en transposant d'un ou deux
    demi-tons (MATCH), rouge si elles se heurtent ;
  - **énergie** (flèche) : plus, autant ou moins d'énergie (niveau sonore et densité
    d'attaques) ; garder ou monter un peu l'énergie est préféré.

  La barre résume la note (tempo 45 %, tonalité 40 %, énergie 15 %), le détail est au
  survol. Le morceau de référence est retiré de la liste, ceux déjà joués passent en fin
  de liste, et la recherche reste active. Cliquer un en-tête de colonne revient au tri
  normal. L'énergie est mesurée au scan : les morceaux déjà analysés avant cette version
  sont réanalysés une fois.
- **Vue d'ensemble** : clic ou glisser pour déplacer la lecture.
- **Vue zoomée** : molette pour zoomer (±0,5 s à ±16 s), glisser pour déplacer le morceau.
- **Cue** : en lecture, revient au point de cue et s'arrête ; à l'arrêt, pose le point de
  cue à la position courante. Clic droit : retour au point de cue.
- **Installer un son** (icône de la barre, ou « Installer… » de la bibliothèque) : rechercher un son sur YouTube (`yt-dlp`) ou Spotify
  (`spotdl`, avec ses propres identifiants d'application Spotify), puis l'installer en MP3.
  Le dossier de destination n'est demandé que la première fois (puis « Changer… » dans le
  panneau). Changer de panneau ne perd rien : les téléchargements continuent, et le panneau
  se retrouve tel qu'il était. Il faut `yt-dlp`, `spotdl` et `ffmpeg` dans le PATH.
  - **Similaires** (troisième onglet du panneau) : propose une trentaine de morceaux proches
    d'un morceau choisi — la sélection de la bibliothèque (bouton ⤓ à côté du champ), un
    « Artiste - Titre » saisi, le bouton aux deux cercles d'un résultat de recherche, ou le
    clic droit « Morceaux similaires… » sur un morceau de la bibliothèque. Les propositions
    viennent de l'API publique de **Deezer** (sans compte ni clé) : la « radio » de
    l'artiste et les titres phares des artistes que Deezer juge proches, en alternance ; la
    provenance est indiquée (même artiste, artiste proche, radio de l'artiste). Les titres
    YouTube sont nettoyés avant la recherche (« (Official Video) », « [HD] », chaînes
    « … - Topic »…). « Installer » cherche le morceau sur YouTube (premier résultat) et
    l'installe comme une recherche normale ; ceux déjà présents dans la bibliothèque sont
    marqués « installé ». Le bouton aux deux cercles d'une proposition relance la
    recherche à partir d'elle, pour explorer de proche en proche. Il faut une connexion
    Internet.
- **Marqueurs** : 8 par platine, sous la vue d'ensemble. Clic sur un marqueur vide : le
  pose à la position courante ; clic sur un marqueur posé : y saute (en lecture comme à
  l'arrêt) ; clic droit : l'efface. Ils sont affichés sur les formes d'onde et enregistrés
  par morceau (`markers.toml`, à côté de la config).
  Deux marqueurs spéciaux, **DÉBUT** et **FIN**, sont placés automatiquement au début et
  à la fin de la musique (premier et dernier passage audible, le silence des bords est
  ignoré) : boutons aux deux bouts de la rangée pour y aller, étiquettes sur la vue zoomée,
  silence assombri sur la vue d'ensemble.
- **Transitions** (icône de la barre) : des étapes automatiques
  déclenchées quand le morceau de départ atteint ses marqueurs. Exemple : « au marqueur 1,
  SYNC puis lancer la destination ; au marqueur 2, couper la voix du départ et descendre
  son High à −15 dB en 1 s ; au marqueur 3, crossfader vers la destination en 8 s ».
  Le panneau montre l'éditeur de nœuds de la transition chargée et, à sa droite, une
  colonne sobre :
  - En haut, le sens **A → B** : de la platine qui joue vers l'autre (quand aucune ou les
    deux jouent, un clic l'inverse ; les titres apparaissent au survol). Le **+** crée une
    transition dans ce sens, vide ou à partir d'un **type de transition** (description au
    survol) : lancement simple, échange des basses, voix coupée puis fondu, fondu long,
    coupure nette (manuelle), balayage EQ, montée progressive (enchaînée), mashup a cappella
    (stems), enchaînement en fin de musique, drop manuel, **loop roll** (le départ boucle
    4, 2, 1, ½ puis ¼ temps pendant que son filtre monte, puis la destination part) et
    **sortie filtrée** (à la mesure suivante, la destination entre en s'ouvrant d'un
    passe-bas pendant que le passe-haut monte sur le départ).
  - Dessous, les transitions qui partent du morceau de l'une ou l'autre platine, chacune
    avec son sens (A → B ou B → A). Celles dont la destination est chargée sur l'autre
    platine sont en tête ; les autres, en grisé, ont un bouton pour charger leur destination.
    Clic : charger la transition dans l'éditeur ; double-clic : la renommer ; clic droit :
    renommer, supprimer (avec confirmation). Une pastille verte marque la transition en
    cours, une rouge celle qu'on enregistre.

  Barre d'outils de l'éditeur, à icônes (description au survol) :
  - **Activer** (interrupteur vert) : chaque événement « marqueur » se déclenche quand la
    lecture du départ franchit son marqueur (un marqueur sauté par un déplacement est
    ignoré, et réarmé si l'on revient avant lui). Chaque marqueur atteint s'affiche dans la
    barre d'état, et la transition s'arrête seule une fois terminée ou si le morceau de
    départ change.
  - **Enregistrer** (point rouge) : choisir un déclencheur dans le menu ; tant que
    l'enregistrement est actif, chaque réglage modifié sur les platines devient une étape de
    ce déclencheur — volume, EQ, kills, filtre, faders et coupures de stems, lecture /
    arrêt, crossfader, SYNC, MATCH. Un réglage modifié plusieurs fois ne donne qu'une étape (sa
    dernière valeur, avec la durée du geste comme durée de fondu) ; le ramener à sa valeur
    de départ retire l'étape. Clic sur le bouton rouge : arrêter. Les nœuds sont alors
    redisposés.
  - Tout cadrer, réorganiser (un événement par ligne), annuler, rétablir ; à droite le
    zoom et **?** (liste des commandes).

  L'éditeur, façon Blueprint d'Unreal Engine :
  - Nœuds **événement** (rouges : marqueurs 1 à 8, début / fin de la musique, manuels 1 à
    4 — touches `F7` à `F10` —, avec un bouton « Déclencher » qui marche même sans activer
    la transition), un nœud par élément de la platine, avec les mêmes composants que sur la
    platine (centrés, à la couleur de la platine visée) :
    - *Voix*, *Batterie*, *Basse*, *Instruments*, *Volume* : fader vertical et bouton de coupure ;
      *EQ High / Mid / Low* : bouton kill et potentiomètre. Glisser le fader règle le niveau
      visé (la durée du fondu apparaît dessous) ; le bouton passe de « ne rien changer » à
      « couper » (rouge) puis « rétablir » (couleur de la platine). Les deux se combinent :
      un seul nœud peut rétablir une piste et régler son niveau. Clic droit sur le fader :
      ne plus régler le niveau ; la valeur s'affiche au survol ;
    - *STEMS* (les 4 pistes côte à côte) et *HML* (les 3 lignes d'EQ, kill et potentiomètre) :
      plusieurs réglages dans un seul nœud, avec un fondu commun ;
    - *Tempo* : le bouton SYNC au-dessus du fader de tempo (SYNC allumé : calage unique ;
      fader : tempo visé, avec sa durée) ; *Tonalité* : le bouton MATCH au-dessus du slider de
      notes (MATCH allumé : accord unique ; sinon la note choisie) ;
    - *Lecture* (lancer / arrêter), *Crossfader* (fader horizontal de A à B), *Aller au
      marqueur* (rangée de marqueurs) ;
    - *Filtre* (effet) : le potentiomètre du filtre de la platine — à gauche passe-bas, à
      droite passe-haut, au centre neutre — et la durée du balayage ;
    - *Boucle* (effet) : boucler 8, 4, 2, 1, ½ ou ¼ temps, calé sur la grille de la
      platine, ou × pour sortir de la boucle. Une boucle en cours garde son début : des
      nœuds *Boucle* successifs de plus en plus courts font un « loop roll ».
  - **Position de départ** des faders et potentiomètres (réglages, STEMS, HML, tempo,
    crossfader, filtre) : le bouton |→ à côté de la durée passe le nœud en mode départ —
    les composants (en jaune) règlent alors la position de départ, la position visée restant
    visible en repère. Au déclenchement, le réglage saute à sa position de départ puis va
    vers la position visée pendant la durée donnée (par exemple : filtre fermé, puis
    ouverture en 8 s). Hors du mode départ, le départ apparaît comme un repère jaune ; clic
    droit sur |→ (ou sur le composant) pour le retirer.
  - La platine visée se choisit avec le petit interrupteur de l'en-tête du nœud, comme à
    l'écran : pastille à gauche pour la platine A (bleue), à droite pour la B (orange).
    Plus les nœuds de **flux**, reliés par des fils d'exécution :
    - *Délai* : attendre x secondes ou x **temps** (clic sur l'unité pour changer) ; en
      temps, l'attente suit le tempo de la platine de départ au moment du déclenchement ;
    - *Quantifier* : attendre le prochain temps, la prochaine mesure, le prochain bloc de 4
      ou de 8 mesures sur la grille du départ (sans attente si le départ est à l'arrêt) —
      pour qu'un déclenchement manuel tombe toujours en rythme ;
    - *Séquence* à plusieurs sorties.

    Pendant l'exécution, les nœuds qui s'exécutent s'illuminent, les délais montrent leur
    progression et les étapes programmées leur compte à rebours.
  - Commandes reprises d'Unreal :
    - clic droit sur le fond : recherche (tapez puis Entrée) pour ajouter un nœud ;
    - glisser d'une broche à une autre : relier ; lâcher un lien dans le vide ouvre la
      recherche et relie le nouveau nœud ; Alt + clic : rompre ; Ctrl + glisser une sortie :
      déplacer son lien ;
    - clic droit glissé (ou bouton du milieu) : déplacer la vue ; molette : zoom (les faders
      des nœuds se règlent en les glissant) ;
    - glisser sur le fond : sélection rectangulaire (Maj / Ctrl : ajouter) ; glisser un nœud
      déplace la sélection (calée sur la grille) ;
    - Suppr, Ctrl + C / X / V, Ctrl + D (dupliquer), Ctrl + Z / Y, Ctrl + A, Origine (tout
      cadrer), F (cadrer la sélection), C (commentaire autour de la sélection : glisser son
      titre déplace les nœuds qu'il contient), F7 à F10 (déclencheurs manuels).
  - Enregistrées localement dans `transitions.json`, à côté de la config.
- **Stems** (volume par instrument) : « SÉPARER » sur une platine découpe le morceau en
  quatre pistes — voix, batterie, basse et instruments (tout le reste) — avec le modèle
  d'IA [Demucs](https://github.com/adefossez/demucs) (`htdemucs`). Chaque piste a ensuite
  un bouton de coupure et un fader de volume (double-clic : volume d'origine). Tant que
  tous les faders sont au maximum, on entend le mix d'origine, sans artefact.
  La séparation n'utilise que 60 % de la mémoire de la carte graphique, pour laisser de la
  place à l'affichage. La séparation tourne en arrière-plan (une dizaine de secondes par morceau sur une carte
  graphique NVIDIA, plusieurs minutes sur le processeur) ; le résultat est gardé en cache
  (dossier `stems`, à côté de la config, environ 25 Mo par morceau) et rechargé
  automatiquement au chargement suivant du morceau.
- **Tempo et tonalité** : le BPM et la tonalité (en notation française, avec la notation Camelot) sont détectés
  au scan, en arrière-plan, et affichés dans la bibliothèque (colonnes triables) et sur
  chaque platine. Les résultats sont mis en cache (`analysis.toml`, à côté de la config) :
  un fichier inchangé n'est pas réanalysé.
  - **Fader TEMPO** : ±12 % (double-clic : tempo d'origine).
  - **SYNC** (au-dessus du fader de tempo) : un clic cale le tempo une fois sur celui de
    l'autre platine ; un appui maintenu active (ou coupe) le SYNC permanent, où le tempo
    suit l'autre platine (au double ou à la moitié si l'écart est énorme). Bouger le fader
    de tempo coupe le SYNC permanent. Au clavier, `1` / `6` basculent le SYNC permanent.
  - **KEY LOCK** : la tonalité ne bouge plus quand le tempo change (étirement temporel).
  - **Slider KEY** (à côté du tempo) : transpose le morceau par demi-tons (±6), chaque cran
    portant la note obtenue (« Lam » = la mineur) ; clic ou glisser sur une note, molette,
    double-clic pour la tonalité d'origine. Les notes qui s'accordent à la tonalité entendue
    sur l'autre platine sont en vert (même tonalité plus marquée, voisines sur la roue
    Camelot ou relatives). Remise à zéro à chaque chargement.
  - **MATCH** (au-dessus du slider KEY) : comme SYNC, un clic transpose une fois le morceau
    pour l'accorder à l'autre platine, un appui maintenu active (ou coupe) le MATCH
    permanent ; une tonalité mineure s'accorde à sa relative majeure. Bouger le slider coupe
    le MATCH permanent. Le décalage s'affiche en jaune dans l'en-tête.
  - **MAGNET** : au lancement, la platine attend le prochain temps de l'autre platine
    (si elle tourne) pour démarrer calée ; le bouton lecture clignote pendant l'attente.
    À l'arrêt, la lecture se cale sur le temps le plus proche quand on relâche la forme
    d'onde après l'avoir déplacée (vue zoomée, vue d'ensemble) ou à la fin d'une recherche
    au clavier, et les marqueurs posés se calent eux aussi sur les temps.
  - La vue zoomée affiche une barre par temps, plus marquée tous les 4 temps.
- **Filtre** (sous l'EQ, ligne FLT) : un potentiomètre — à gauche passe-bas, à droite
  passe-haut (avec une légère résonance), au centre neutre — et un bouton marche / arrêt
  (tourner le potentiomètre allume le filtre).
- **Boucles** : posées par les transitions (nœud *Boucle*), calées sur les temps du
  morceau. La boucle apparaît en vert sur les formes d'onde, avec une pastille
  « BOUCLE n » sur la vue zoomée : un clic dessus en sort, comme un déplacement de la
  lecture hors de la boucle. Le raccord est fondu sur quelques millisecondes (pas de clic).
- **RÉINIT.** (colonne KEY LOCK / MAGNET de chaque platine, deux clics) : remet tous les
  réglages de la platine à zéro — volume, EQ et kills, filtre, boucle, tempo, tonalité,
  SYNC, MATCH, faders et coupures des stems, zoom. KEY LOCK, MAGNET, le morceau, sa
  position, le cue et les marqueurs sont conservés.
- **Potentiomètres et faders** : double-clic pour revenir à 0 dB (ou au centre pour le
  crossfader), molette au survol, `Maj` pendant le glisser pour un réglage fin.

### Raccourcis clavier (disposition Mixxx)

Les lettres désignent la **position physique** des touches d'un clavier QWERTY, comme dans
les mappings localisés de Mixxx : sur un AZERTY, `A` est la touche marquée `Q` et `;` celle
marquée `M`.

| Fonction                         | Platine A       | Platine B       |
| -------------------------------- | --------------- | --------------- |
| Lecture / pause                  | `D`             | `L`             |
| Cue                              | `F`             | `;`             |
| Définir le point de cue          | `Maj + D`       | `Maj + L`       |
| Retour au cue et arrêt           | `Maj + F`       | `Maj + ;`       |
| Recherche arrière / avant        | `A` / `S`       | `J` / `K`       |
| Kill des basses                  | `B`             | `N`             |
| Charger le morceau sélectionné   | `Maj + ←`       | `Maj + →`       |
| Décharger la platine             | `Alt + Maj + ←` | `Alt + Maj + →` |
| Crossfader gauche / droite       | `G`             | `H`             |
| Transition : déclencheurs manuels 1 à 4 | `F7` `F8` `F9` `F10` | |
| Tempo − / + (1 %, Maj : 0,1 %)   | `F1` / `F2`     | `F5` / `F6`     |
| SYNC                             | `1`             | `6`             |
| Marqueurs 1 à 4 (poser / sauter) | `2` à `5`       | `7` à `0`       |
| Effacer les marqueurs 1 à 4      | `Maj + 2` à `5` | `Maj + 7` à `0` |
| Crossfader, petits pas           | `Maj + G`       | `Maj + H`       |

`↑` / `↓` déplacent la sélection dans la bibliothèque, `Ctrl + F` va au champ de recherche.

## Architecture

```
src/
├── main.rs            point d'entrée (fenêtre eframe, icône)
├── audio/
│   ├── dsp.rs         biquads, EQ 3 bandes, filtre DJ, lissage, limiteur, courbes de gain
│   ├── engine.rs      moteur de mixage temps réel (platines, boucles, crossfader, master)
│   ├── stretch.rs     étirement temporel WSOLA + transposition (key lock, match key)
│   ├── output.rs      sortie cpal (WASAPI / ALSA)
│   └── decoder.rs     décodage symphonia + rééchantillonnage rubato
├── analysis.rs        détection du tempo (grille de temps), de la tonalité et de l'énergie
├── waveform.rs        formes d'onde multi-résolution (pic, RMS, énergie par bande)
├── library.rs         scan récursif, tags (lofty), recherche, tri
├── loader.rs          chargement des morceaux en arrière-plan
├── config.rs          dossiers de la bibliothèque (TOML, dossier de config de l'OS)
├── markers.rs         marqueurs (hot cues) enregistrés par morceau
├── transitions.rs     transitions planifiées (étapes sur marqueurs) et leur stockage
├── transition_graph.rs graphe de nœuds des transitions et sa traduction en étapes
├── stems.rs           séparation en stems (Demucs, via separate_stems.py) et cache
├── suggest.rs         suggestions de morceaux compatibles (tempo, tonalité, énergie)
├── downloader.rs      recherche et téléchargement de sons (yt-dlp, spotdl)
├── recommend.rs       morceaux similaires à un morceau (API publique de Deezer)
└── ui/                interface egui : platines, mixer, bibliothèque, transitions, raccourcis
```

À côté : `assets/` (logo : `logo.svg` source, icônes PNG et `.ico`), `build.rs` (icône de
l'exécutable Windows), `.cargo/config.toml` (runtime C lié statiquement : l'exécutable
Windows ne dépend d'aucun « Visual C++ Redistributable »), `packaging/windows/`
(installateur Inno Setup : `iscc /DAppVersion=0.2.0 packaging/windows/rustymix.iss`, après
`cargo build --release`), `packaging/linux/` (raccourci `.desktop` et `install.sh` ; le
`.deb` est décrit dans `Cargo.toml` pour `cargo deb`) et `.github/workflows/release.yml`
(compilation, tests et paquets Windows / Ubuntu, publiés pour chaque étiquette `v*`).

Chaîne de signal par platine : buffer (mix, ou stems pondérés : mix × instruments +
Σ stem × (volume − instruments) ; boucle éventuelle) → tempo / key lock / transposition →
EQ → filtre → gain de canal → crossfader (puissance constante) → somme → master →
limiteur doux → sortie.

Le thread audio ne prend aucun verrou et n'alloue jamais : l'interface lui envoie des
commandes par un canal sans attente et des paramètres par atomiques (lissés sur quelques
millisecondes côté moteur) ; il publie en retour la position et les niveaux par atomiques.
Les morceaux sont entièrement décodés en mémoire, à la fréquence du périphérique de sortie.

L'EQ est un isolateur 3 bandes (filtres Linkwitz-Riley d'ordre 4 à 200 Hz et 3 kHz) : le
gain Low agit comme un low-shelf, Mid comme une cloche large, High comme un high-shelf,
la réponse est plate quand tout est à 0 dB, et la position minimale coupe réellement la
bande (−∞ dB).

Le filtre DJ enchaîne un passe-bas (de 21 kHz à 70 Hz) et un passe-haut (de 16 Hz à
9 kHz), toujours actifs pour que la coupure glisse sans clic d'un côté à l'autre ; au
centre, le signal passe tel quel.

## Configuration

Les dossiers de la bibliothèque sont enregistrés dans `config.toml` :

- Windows : `%APPDATA%\Rustymix\config\config.toml`
- Linux : `~/.config/rustymix/config.toml`

La variable d'environnement `RUSTYMIX_CONFIG` permet d'utiliser un autre fichier.

## Tests

```bash
cargo test
```

## Licence

Rustymix est distribué sous licence GNU GPL v3 (voir [LICENSE](LICENSE)).
