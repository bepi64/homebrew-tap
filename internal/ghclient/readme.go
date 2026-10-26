package ghclient

import (
	"context"
	"fmt"

	"github.com/google/go-github/v90/github"
)

// GetReadme récupère le contenu texte du README du dépôt, déjà décodé.
func (c *Client) GetReadme(ctx context.Context) (string, error) {
	branch, err := c.defaultBranch(ctx)
	if err != nil {
		return "", fmt.Errorf("dépôt introuvable: %w", err)
	}

	readme, _, err := c.gh.Repositories.GetReadme(ctx, c.Owner, c.Repo, &github.RepositoryContentGetOptions{
		Ref: branch,
	})
	if err != nil {
		return "", fmt.Errorf("lecture du README: %w", err)
	}

	content, err := readme.GetContent()
	if err != nil {
		return "", fmt.Errorf("décodage du README: %w", err)
	}
	return content, nil
}
