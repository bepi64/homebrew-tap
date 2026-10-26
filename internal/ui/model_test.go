package ui

import (
	"testing"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/bepi64/homebrew-tap/internal/ghclient"
)

// TestNavigationAndLazyFileLoad vérifie, sans réseau ni terminal, le
// comportement central du TUI : navigation dans l'arbre, dépliage d'un
// dossier, et chargement paresseux d'un fichier seulement à l'ouverture.
func TestNavigationAndLazyFileLoad(t *testing.T) {
	entries := []ghclient.Entry{
		{Path: "dir1", Type: "tree"},
		{Path: "dir1/inner.go", Type: "blob", Size: 10},
		{Path: "README.md", Type: "blob", Size: 5},
		{Path: "main.go", Type: "blob", Size: 20},
	}

	m := NewModel(nil)
	updated, _ := m.Update(treeLoadedMsg{entries: entries})
	m = updated.(Model)

	// Tri attendu : dossiers avant fichiers, alphabétique ensuite.
	// dir1/ (replié) ; README.md ; main.go
	if got := m.rows[0].n.path; got != "dir1" {
		t.Fatalf("row 0 = %q, want dir1", got)
	}
	if got := m.rows[0].n.isDir; !got {
		t.Fatalf("dir1 should be a directory")
	}

	// Descendre sur README.md et l'ouvrir ne doit rien télécharger avant
	// l'appui sur entrée.
	updated, _ = m.Update(tea.KeyMsg{Type: tea.KeyDown})
	m = updated.(Model)
	if got := m.rows[m.cursor].n.path; got != "README.md" {
		t.Fatalf("cursor path = %q, want README.md", got)
	}

	updated, cmd := m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	m = updated.(Model)
	if m.activePath != "README.md" {
		t.Fatalf("activePath = %q, want README.md", m.activePath)
	}
	if cmd == nil {
		t.Fatalf("expected a load command on first open of README.md")
	}
	if m.loadingFile != "README.md" {
		t.Fatalf("loadingFile = %q, want README.md", m.loadingFile)
	}
	if _, cached := m.fileCache["README.md"]; cached {
		t.Fatalf("file must not be cached before the async result arrives")
	}

	// Le résultat asynchrone arrive : il doit remplir le cache et lever
	// l'indicateur de chargement.
	updated, _ = m.Update(fileLoadedMsg{path: "README.md", content: "hello readme"})
	m = updated.(Model)
	if m.fileCache["README.md"] != "hello readme" {
		t.Fatalf("fileCache[README.md] = %q, want %q", m.fileCache["README.md"], "hello readme")
	}
	if m.loadingFile != "" {
		t.Fatalf("loadingFile should be cleared once the file is loaded")
	}

	// Ré-ouvrir le même fichier ne doit plus déclencher de commande de
	// chargement (déjà en cache).
	_, cmd = m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	if cmd != nil {
		t.Fatalf("re-opening a cached file must not trigger a new load command")
	}

	// Remonter sur dir1 et le déplier doit faire apparaître son enfant sans
	// déclencher aucun chargement de fichier.
	updated, _ = m.Update(tea.KeyMsg{Type: tea.KeyUp})
	m = updated.(Model)
	updated, cmd = m.Update(tea.KeyMsg{Type: tea.KeyEnter})
	m = updated.(Model)
	if cmd != nil {
		t.Fatalf("expanding a directory must not trigger a load command")
	}
	if !m.root.children[0].expanded {
		t.Fatalf("dir1 should be expanded")
	}
	if len(m.rows) != 4 {
		t.Fatalf("len(rows) = %d, want 4 after expanding dir1 (dir1, dir1/inner.go, README.md, main.go)", len(m.rows))
	}
}

// TestIsDisplayableGuardsAgainstLargeAndBinaryFiles vérifie que le
// garde-fou se base uniquement sur les métadonnées déjà connues (taille,
// extension), jamais sur un contenu qui n'est justement pas téléchargé.
func TestIsDisplayableGuardsAgainstLargeAndBinaryFiles(t *testing.T) {
	cases := []struct {
		path string
		size int64
		want bool
	}{
		{"main.go", 1024, true},
		{"assets/logo.png", 1024, false},
		{"vendor/huge.txt", maxDisplaySize + 1, false},
		{"README.md", maxDisplaySize - 1, true},
	}
	for _, c := range cases {
		if got := isDisplayable(c.path, c.size); got != c.want {
			t.Errorf("isDisplayable(%q, %d) = %v, want %v", c.path, c.size, got, c.want)
		}
	}
}
