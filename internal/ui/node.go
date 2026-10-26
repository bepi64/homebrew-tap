package ui

import (
	"sort"
	"strings"

	"github.com/bepi64/homebrew-tap/internal/ghclient"
)

// node est un nœud de l'arbre de fichiers reconstruit à partir de la liste
// plate renvoyée par l'API GitHub (un chemin par entrée).
type node struct {
	name     string
	path     string
	isDir    bool
	size     int64
	children []*node
	expanded bool
}

// buildTree reconstruit la hiérarchie dossiers/fichiers à partir des entrées
// plates de l'arbre GitHub.
func buildTree(entries []ghclient.Entry) *node {
	root := &node{isDir: true, expanded: true}
	index := map[string]*node{"": root}

	for _, e := range entries {
		parts := strings.Split(e.Path, "/")
		cur := root
		for i, part := range parts {
			path := strings.Join(parts[:i+1], "/")
			child, ok := index[path]
			if !ok {
				child = &node{name: part, path: path}
				index[path] = child
				cur.children = append(cur.children, child)
			}
			if i == len(parts)-1 {
				child.isDir = e.Type == "tree"
				child.size = e.Size
			} else {
				child.isDir = true
			}
			cur = child
		}
	}

	sortChildren(root)
	return root
}

func sortChildren(n *node) {
	sort.Slice(n.children, func(i, j int) bool {
		a, b := n.children[i], n.children[j]
		if a.isDir != b.isDir {
			return a.isDir
		}
		return a.name < b.name
	})
	for _, c := range n.children {
		sortChildren(c)
	}
}

// visibleRow est une ligne affichable de l'arbre : un nœud et sa profondeur,
// en tenant compte des dossiers repliés (dont les enfants sont omis).
type visibleRow struct {
	n     *node
	depth int
}

func flatten(root *node) []visibleRow {
	var rows []visibleRow
	var walk func(n *node, depth int)
	walk = func(n *node, depth int) {
		for _, c := range n.children {
			rows = append(rows, visibleRow{n: c, depth: depth})
			if c.isDir && c.expanded {
				walk(c, depth+1)
			}
		}
	}
	walk(root, 0)
	return rows
}
