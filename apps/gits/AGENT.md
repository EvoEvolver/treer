# Gits

Gits is the workspace-local Git service for Agents and humans. It hosts small
bare repositories over Git Smart HTTP. The human interface is at `/_human/`.

The default service URL is `http://gits.internal`. Keep Gits on the private
workspace network. Every Agent that can reach it can read and push every
repository; Gits does not implement repository-level accounts or permissions.

## Inspect repositories

```sh
curl -fsS http://gits.internal/v1/repos | jq
curl -fsS http://gits.internal/v1/repos/REPOSITORY | jq
```

## Create a repository

Repository names use lowercase letters, digits, dots, dashes, and underscores.

```sh
curl -fsS -X POST http://gits.internal/v1/repos \
  -H 'Content-Type: application/json' \
  -d '{"name":"example","description":"Shared Agent work"}' | jq
```

Creating a repository is a mutation. Do it only when the task requires a new
shared repository. Repeating the same name returns `409 repository_exists`.

## Clone and push

```sh
git clone http://gits.internal/git/example.git
cd example
printf '%s\n' '# Example' > README.md
git add README.md
git commit -m 'Initial commit'
git push origin HEAD:main
```

Use an ordinary Git remote for an existing checkout:

```sh
git remote add gits http://gits.internal/git/example.git
git push gits HEAD:main
```

## JSON API

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Service health |
| `GET` | `/v1/repos` | Repository collection |
| `POST` | `/v1/repos` | Create a bare repository |
| `GET` | `/v1/repos/REPOSITORY` | Branches and recent commits |

Git clients use `/git/REPOSITORY.git`. The JSON API has no delete operation;
repository deletion and storage recovery are explicit operator filesystem
tasks. Repositories may contain secrets committed by their users. Do not expose
Gits through public ingress, and do not push credentials into a repository.
