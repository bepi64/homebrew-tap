// Package ghclient enveloppe l'API REST GitHub pour ne charger que ce qui
// est nécessaire : l'arbre des fichiers, le README, puis le contenu d'un
// fichier précis à la demande.
package ghclient

import (
	"context"
	"sync"

	"github.com/google/go-github/v90/github"
)

// Client accède à un unique dépôt GitHub en lecture seule.
type Client struct {
	gh    *github.Client
	Owner string
	Repo  string

	branchOnce sync.Once
	branch     string
	branchErr  error
}

// New crée un client pour le dépôt owner/repo. Un token vide fonctionne mais
// limite les appels à 60/heure côté GitHub ; un token porte cette limite à
// 5000/heure.
func New(token, owner, repo string) (*Client, error) {
	var opts []github.ClientOptionsFunc
	if token != "" {
		opts = append(opts, github.WithAuthToken(token))
	}

	gh, err := github.NewClient(opts...)
	if err != nil {
		return nil, err
	}
	return &Client{gh: gh, Owner: owner, Repo: repo}, nil
}

// defaultBranch résout la branche par défaut du dépôt une seule fois par
// session : elle sert de référence pour l'arbre, le README et les fichiers.
func (c *Client) defaultBranch(ctx context.Context) (string, error) {
	c.branchOnce.Do(func() {
		repoInfo, _, err := c.gh.Repositories.Get(ctx, c.Owner, c.Repo)
		if err != nil {
			c.branchErr = err
			return
		}
		c.branch = repoInfo.GetDefaultBranch()
	})
	return c.branch, c.branchErr
}
