package ui

import "strings"

func (m Model) renderTree() string {
	width := m.leftWidth()
	height := m.bodyHeight()

	if m.treeErr != nil {
		return paneStyle.Width(width).Height(height).Render(errorStyle.Render("Erreur: " + m.treeErr.Error()))
	}
	if !m.treeLoaded {
		return paneStyle.Width(width).Height(height).Render(dimStyle.Render("Chargement de l'arborescence..."))
	}

	end := m.treeScroll + height
	if end > len(m.rows) {
		end = len(m.rows)
	}

	var b strings.Builder
	for i := m.treeScroll; i < end; i++ {
		if i > m.treeScroll {
			b.WriteString("\n")
		}
		b.WriteString(renderTreeLine(m.rows[i], i == m.cursor))
	}
	return paneStyle.Width(width).Height(height).Render(b.String())
}

func renderTreeLine(row visibleRow, selected bool) string {
	indent := strings.Repeat("  ", row.depth)
	name := row.n.name
	style := fileStyle
	icon := "  "

	if row.n.isDir {
		style = dirStyle
		name += "/"
		if row.n.expanded {
			icon = "v "
		} else {
			icon = "> "
		}
	}

	line := indent + icon + name
	if selected {
		return selStyle.Render(line)
	}
	return style.Render(line)
}
