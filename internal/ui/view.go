package ui

import (
	"fmt"

	"github.com/charmbracelet/lipgloss"
)

func (m Model) View() string {
	label, _ := m.contentBody()
	title := titleStyle.Render(fmt.Sprintf("%s/%s", m.client.Owner, m.client.Repo)) +
		dimStyle.Render("  —  "+label)

	body := lipgloss.JoinHorizontal(lipgloss.Top, m.renderTree(), m.renderContent())

	help := helpStyle.Render("↑/↓ naviguer · entrée ouvrir/déplier · pgup/pgdown défiler · esc README · q quitter")

	return title + "\n" + body + "\n" + help
}
