#!/usr/bin/env bash
# Applies the GitHub settings the workflows rely on, matching
# danthegoodman1/inodedb. Run it once after creating the repository, with the
# gh CLI logged in as an admin:
#
#   scripts/setup-github.sh [owner/repo]
#
# - The `main` ruleset (.github/ruleset-main.json): no deleting main or force
#   pushing to it; changes only through pull requests, merged or squashed,
#   whose CI checks pass. Deploy keys bypass it.
# - Fork pull requests from anyone outside the repository wait for a
#   maintainer's approval before their workflows run.
# - Workflows get a read-only GITHUB_TOKEN unless they ask for more.
# - The "Auto Release" deploy key with write access, whose private half is the
#   RELEASE_DEPLOY_KEY secret that bump.yml pushes release commits and tags with.
set -euo pipefail

repo="${1:-danthegoodman1/git-secure-env}"
here="$(cd "$(dirname "$0")/.." && pwd)"

gh api -X PUT "repos/$repo/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors >/dev/null
echo "fork pull requests from outside contributors need approval to run workflows"

gh api -X PUT "repos/$repo/actions/permissions/workflow" \
  -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false >/dev/null
echo "workflows default to a read-only token"

existing="$(gh api "repos/$repo/rulesets" --jq '.[] | select(.name == "main") | .id')"
if [ -n "$existing" ]; then
  gh api -X PUT "repos/$repo/rulesets/$existing" --input "$here/.github/ruleset-main.json" >/dev/null
  echo "updated the main ruleset"
else
  gh api -X POST "repos/$repo/rulesets" --input "$here/.github/ruleset-main.json" >/dev/null
  echo "created the main ruleset"
fi

if gh api "repos/$repo/keys" --jq '.[].title' | grep -qx "Auto Release"; then
  echo "the Auto Release deploy key exists"
else
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  ssh-keygen -q -t ed25519 -N "" -C "gse auto release" -f "$tmp/key"
  gh api -X POST "repos/$repo/keys" -f title="Auto Release" \
    -f key="$(cat "$tmp/key.pub")" -F read_only=false >/dev/null
  gh secret set RELEASE_DEPLOY_KEY --repo "$repo" < "$tmp/key"
  echo "created the Auto Release deploy key and the RELEASE_DEPLOY_KEY secret"
fi
