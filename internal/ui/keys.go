package ui

import tea "github.com/charmbracelet/bubbletea"

func (m Model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "q", "ctrl+c":
		return m, tea.Quit
	}

	if !m.treeLoaded {
		return m, nil
	}

	switch msg.String() {
	case "up", "k":
		if m.cursor > 0 {
			m.cursor--
		}
		m.ensureCursorVisible()
	case "down", "j":
		if m.cursor < len(m.rows)-1 {
			m.cursor++
		}
		m.ensureCursorVisible()
	case "enter":
		return m.activateSelection()
	case "esc":
		m.activePath = ""
		m.fileScroll = 0
	case "pgdown", "d":
		m.fileScroll += m.bodyHeight()
	case "pgup", "u":
		m.fileScroll -= m.bodyHeight()
		if m.fileScroll < 0 {
			m.fileScroll = 0
		}
	}
	return m, nil
}

func (m *Model) ensureCursorVisible() {
	height := m.bodyHeight()
	if m.cursor < m.treeScroll {
		m.treeScroll = m.cursor
	}
	if m.cursor >= m.treeScroll+height {
		m.treeScroll = m.cursor - height + 1
	}
}

// activateSelection réagit à "entrée" sur la ligne sélectionnée : replie ou
// déplie un dossier, ou bascule le panneau de droite sur un fichier — en
// déclenchant son téléchargement seulement s'il n'est pas déjà en cache.
func (m Model) activateSelection() (tea.Model, tea.Cmd) {
	if m.cursor >= len(m.rows) {
		return m, nil
	}
	row := m.rows[m.cursor]

	if row.n.isDir {
		row.n.expanded = !row.n.expanded
		m.rows = flatten(m.root)
		return m, nil
	}

	m.activePath = row.n.path
	m.fileScroll = 0

	if _, cached := m.fileCache[row.n.path]; cached {
		return m, nil
	}
	if _, failed := m.fileErr[row.n.path]; failed {
		return m, nil
	}
	if !isDisplayable(row.n.path, row.n.size) {
		m.fileErr[row.n.path] = notDisplayableError(row.n.path, row.n.size)
		return m, nil
	}
	if m.loadingFile == row.n.path {
		return m, nil
	}

	m.loadingFile = row.n.path
	return m, loadFileCmd(m.client, row.n.path)
}
