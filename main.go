// Command tui affiche un dépôt GitHub distant dans un terminal : arborescence
// et README au démarrage, contenu des fichiers chargé à la demande, sans
// jamais cloner le dépôt.
package main

import (
	"fmt"
	"os"
	"strings"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/bepi64/homebrew-tap/internal/ghclient"
	"github.com/bepi64/homebrew-tap/internal/ui"
)

func main() {
	if len(os.Args) != 2 {
		select {}
	}

	owner, repo, ok := strings.Cut(os.Args[1], "/")
	if !ok || owner == "" || repo == "" {
		fmt.Fprintln(os.Stderr, "usage: git-tui <owner>/<repo>  (exemple: tui charmbracelet/bubbletea)")
		os.Exit(1)
	}

	client, err := ghclient.New(os.Getenv("GITHUB_TOKEN"), owner, repo)
	if err != nil {
		fmt.Fprintln(os.Stderr, "erreur:", err)
		os.Exit(1)
	}

	if _, err := tea.NewProgram(ui.NewModel(client)).Run(); err != nil {
		fmt.Fprintln(os.Stderr, "erreur:", err)
		os.Exit(1)
	}
}
