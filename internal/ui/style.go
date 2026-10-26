package ui

import "github.com/charmbracelet/lipgloss"

var (
	paneStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(lipgloss.Color("240")).
			Padding(0, 1)

	titleStyle = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("212"))
	dirStyle   = lipgloss.NewStyle().Foreground(lipgloss.Color("117")).Bold(true)
	fileStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("252"))
	selStyle   = lipgloss.NewStyle().Background(lipgloss.Color("237")).Foreground(lipgloss.Color("230")).Bold(true)
	dimStyle   = lipgloss.NewStyle().Foreground(lipgloss.Color("240"))
	errorStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("203")).Bold(true)
	helpStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("241"))
)
