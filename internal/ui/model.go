package ui

import (
	"context"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/bepi64/homebrew-tap/internal/ghclient"
)

// Model est l'état complet du TUI (architecture Elm-like de Bubble Tea).
type Model struct {
	client *ghclient.Client

	treeLoaded bool
	treeErr    error
	root       *node
	rows       []visibleRow
	cursor     int
	treeScroll int

	readmeLoaded bool
	readmeErr    error
	readmeText   string

	// activePath est le chemin affiché dans le panneau de droite ; vide
	// signifie "README". loadingFile est le chemin en cours de
	// téléchargement, le cas échéant.
	activePath  string
	loadingFile string
	fileCache   map[string]string
	fileErr     map[string]error
	fileScroll  int

	width, height int
}

// NewModel construit le modèle initial. Le chargement de l'arbre et du
// README démarre dès l'appel à Init.
func NewModel(client *ghclient.Client) Model {
	return Model{
		client:    client,
		fileCache: make(map[string]string),
		fileErr:   make(map[string]error),
	}
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(loadTreeCmd(m.client), loadReadmeCmd(m.client))
}

type treeLoadedMsg struct {
	entries []ghclient.Entry
	err     error
}

type readmeLoadedMsg struct {
	content string
	err     error
}

type fileLoadedMsg struct {
	path    string
	content string
	err     error
}

func loadTreeCmd(c *ghclient.Client) tea.Cmd {
	return func() tea.Msg {
		entries, err := c.GetTree(context.Background())
		return treeLoadedMsg{entries: entries, err: err}
	}
}

func loadReadmeCmd(c *ghclient.Client) tea.Cmd {
	return func() tea.Msg {
		content, err := c.GetReadme(context.Background())
		return readmeLoadedMsg{content: content, err: err}
	}
}

func loadFileCmd(c *ghclient.Client, path string) tea.Cmd {
	return func() tea.Msg {
		content, err := c.GetFileContent(context.Background(), path)
		return fileLoadedMsg{path: path, content: content, err: err}
	}
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
		return m, nil

	case tea.KeyMsg:
		return m.handleKey(msg)

	case treeLoadedMsg:
		if msg.err != nil {
			m.treeErr = msg.err
			return m, nil
		}
		m.root = buildTree(msg.entries)
		m.rows = flatten(m.root)
		m.treeLoaded = true
		return m, nil

	case readmeLoadedMsg:
		m.readmeLoaded = true
		if msg.err != nil {
			m.readmeErr = msg.err
			return m, nil
		}
		m.readmeText = msg.content
		return m, nil

	case fileLoadedMsg:
		if m.loadingFile == msg.path {
			m.loadingFile = ""
		}
		if msg.err != nil {
			m.fileErr[msg.path] = msg.err
			return m, nil
		}
		m.fileCache[msg.path] = msg.content
		return m, nil
	}

	return m, nil
}
