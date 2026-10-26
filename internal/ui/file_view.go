package ui

import (
	"fmt"
	"path/filepath"
	"strings"
)

// maxDisplaySize évite de télécharger un fichier manifestement trop lourd
// pour être lu dans un terminal.
const maxDisplaySize = 200 * 1024

var binaryExtensions = map[string]bool{
	".png": true, ".jpg": true, ".jpeg": true, ".gif": true, ".ico": true,
	".webp": true, ".bmp": true,
	".zip": true, ".tar": true, ".gz": true, ".tgz": true, ".7z": true, ".rar": true,
	".exe": true, ".dll": true, ".so": true, ".dylib": true, ".bin": true,
	".woff": true, ".woff2": true, ".ttf": true, ".otf": true, ".eot": true,
	".mp3": true, ".mp4": true, ".wav": true, ".mov": true, ".avi": true,
	".pdf": true, ".class": true, ".jar": true, ".wasm": true,
}

// isDisplayable s'appuie sur les métadonnées déjà connues via l'arbre
// (taille, extension) pour éviter un téléchargement inutile — jamais sur le
// contenu du fichier, qui lui n'est justement pas encore téléchargé.
func isDisplayable(path string, size int64) bool {
	if size > maxDisplaySize {
		return false
	}
	return !binaryExtensions[strings.ToLower(filepath.Ext(path))]
}

func notDisplayableError(path string, size int64) error {
	if size > maxDisplaySize {
		return fmt.Errorf("fichier trop volumineux (%s), non téléchargé", humanSize(size))
	}
	return fmt.Errorf("type de fichier probablement binaire, non téléchargé")
}

func humanSize(n int64) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1f Mo", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%.1f Ko", float64(n)/(1<<10))
	default:
		return fmt.Sprintf("%d o", n)
	}
}

// contentBody résout ce que le panneau de droite doit afficher : le README
// par défaut, ou le fichier actif (chargé, en cours de chargement, ou en
// erreur).
func (m Model) contentBody() (label, body string) {
	if m.activePath == "" {
		switch {
		case m.readmeErr != nil:
			return "README", "Erreur: " + m.readmeErr.Error()
		case !m.readmeLoaded:
			return "README", "Chargement du README..."
		case m.readmeText == "":
			return "README", "(ce dépôt n'a pas de README)"
		default:
			return "README", m.readmeText
		}
	}

	if err, ok := m.fileErr[m.activePath]; ok {
		return m.activePath, "Erreur: " + err.Error()
	}
	if content, ok := m.fileCache[m.activePath]; ok {
		return m.activePath, content
	}
	if m.loadingFile == m.activePath {
		return m.activePath, "Chargement de " + m.activePath + "..."
	}
	return m.activePath, ""
}

func (m Model) renderContent() string {
	width := m.rightWidth()
	height := m.bodyHeight()

	_, body := m.contentBody()
	lines := strings.Split(body, "\n")

	start := m.fileScroll
	if start > len(lines) {
		start = len(lines)
	}
	end := start + height
	if end > len(lines) {
		end = len(lines)
	}

	return paneStyle.Width(width).Height(height).Render(strings.Join(lines[start:end], "\n"))
}
