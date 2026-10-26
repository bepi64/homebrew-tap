package ui

// layout.go centralise le calcul des dimensions des deux panneaux à partir
// de la taille du terminal (reçue via tea.WindowSizeMsg), avec des valeurs
// de repli tant qu'elle n'a pas encore été reçue.

const (
	fallbackWidth  = 80
	fallbackHeight = 24
	minLeftWidth   = 24
	maxLeftWidth   = 40
)

func (m Model) totalWidth() int {
	if m.width <= 0 {
		return fallbackWidth
	}
	return m.width
}

func (m Model) totalHeight() int {
	if m.height <= 0 {
		return fallbackHeight
	}
	return m.height
}

// leftWidth et rightWidth sont les largeurs de contenu (hors bordure) des
// deux panneaux, chaque bordure consommant 2 colonnes.
func (m Model) leftWidth() int {
	available := m.totalWidth() - 4
	w := available * 3 / 10
	if w < minLeftWidth {
		w = minLeftWidth
	}
	if w > maxLeftWidth {
		w = maxLeftWidth
	}
	return w
}

func (m Model) rightWidth() int {
	w := m.totalWidth() - 4 - m.leftWidth()
	if w < 20 {
		w = 20
	}
	return w
}

// bodyHeight est la hauteur de contenu (hors bordure) partagée par les deux
// panneaux ; 1 ligne est réservée au titre en haut et 1 à l'aide en bas.
func (m Model) bodyHeight() int {
	h := m.totalHeight() - 2 - 2
	if h < 3 {
		h = 3
	}
	return h
}
