package ghclient

import (
	"context"
	"fmt"

	"github.com/google/go-github/v90/github"
)

// GetFileContent télécharge le contenu d'un seul fichier, identifié par son
// chemin dans le dépôt. Appelé uniquement quand l'utilisateur ouvre ce
// fichier dans l'interface — jamais au démarrage.
func (c *Client) GetFileContent(ctx context.Context, path string) (string, error) {
	branch, err := c.defaultBranch(ctx)
	if err != nil {
		return "", fmt.Errorf("dépôt introuvable: %w", err)
	}

	file, _, _, err := c.gh.Repositories.GetContents(ctx, c.Owner, c.Repo, path, &github.RepositoryContentGetOptions{
		Ref: branch,
	})
	if err != nil {
		return "", fmt.Errorf("lecture de %s: %w", path, err)
	}
	if file == nil {
		return "", fmt.Errorf("%s n'est pas un fichier", path)
	}

	content, err := file.GetContent()
	if err != nil {
		return "", fmt.Errorf("décodage de %s: %w", path, err)
	}
	return content, nil
}
