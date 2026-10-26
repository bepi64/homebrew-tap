package ghclient

import (
	"context"
	"fmt"
)

// Entry est une entrée plate de l'arbre du dépôt : un fichier ("blob") ou un
// dossier ("tree"). Elle ne contient jamais le contenu d'un fichier.
type Entry struct {
	Path string
	Type string
	Size int64
}

// GetTree récupère la totalité de l'arbre du dépôt en un seul appel,
// sans le contenu des fichiers (endpoint /git/trees, pas /git/blobs).
func (c *Client) GetTree(ctx context.Context) ([]Entry, error) {
	branch, err := c.defaultBranch(ctx)
	if err != nil {
		return nil, fmt.Errorf("dépôt introuvable: %w", err)
	}

	tree, _, err := c.gh.Git.GetTree(ctx, c.Owner, c.Repo, branch, true)
	if err != nil {
		return nil, fmt.Errorf("lecture de l'arbre du dépôt: %w", err)
	}

	entries := make([]Entry, 0, len(tree.Entries))
	for _, e := range tree.Entries {
		entries = append(entries, Entry{
			Path: e.GetPath(),
			Type: e.GetType(),
			Size: int64(e.GetSize()),
		})
	}
	return entries, nil
}
