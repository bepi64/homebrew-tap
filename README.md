# git-tui

TUI en Go pour parcourir un dépôt GitHub distant sans le cloner : arbre des
fichiers + README chargés au démarrage, contenu d'un fichier téléchargé à la
volée seulement quand on l'ouvre.

## Installation

Via Homebrew (Homebrew ≥ 6.0.0, avec le nouveau système de tap trust) :

```sh
brew tap Bepi64/git-tui https://github.com/Bepi64/git-tui.git
brew trust Bepi64/git-tui
brew install git-tui
```

## Build depuis les sources

```sh
make build   # ou : go build -o bin/git-tui ./cmd/tui
```

## Usage

```sh
export GITHUB_TOKEN=ghp_...   # optionnel, mais passe de 60 à 5000 requêtes/heure
git-tui charmbracelet/bubbletea   # ou ./bin/git-tui si construit depuis les sources
```

Touches : `↑`/`↓` (ou `j`/`k`) naviguer, `entrée` ouvrir un fichier ou
déplier/replier un dossier, `pgup`/`pgdown` (ou `u`/`d`) défiler le contenu,
`esc` revenir au README, `q` quitter.

## Licence

Apache 2.0 — voir [LICENSE](LICENSE).
