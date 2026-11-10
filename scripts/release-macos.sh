#!/usr/bin/env bash
# Builds the app and publishes it as a GitHub release, tagged v<version from Cargo.toml>.
# The zip is named Kube-UI-<version>-macos-<arch>.zip, which scripts/install.sh relies on.
#
#   ./scripts/release-macos.sh                 # notes generated from commits
#   ./scripts/release-macos.sh notes.md        # notes from a file
#
# Bump `version` in Cargo.toml and commit before running. Needs the GitHub CLI (gh).
set -euo pipefail

cd "$(dirname "$0")/.."
NOTES="${1:-}"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
TAG="v$VERSION"
ARCH="$(uname -m)"
ZIP="target/release/bundle/Kube-UI-$VERSION-macos-$ARCH.zip"

command -v gh >/dev/null || { echo "Needs the GitHub CLI: brew install gh" >&2; exit 1; }
[[ -z "$(git status --porcelain)" ]] || { echo "Commit or stash your changes first." >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || gh release view "$TAG" >/dev/null 2>&1; then
    echo "$TAG already exists. Bump the version in Cargo.toml first." >&2
    exit 1
fi

./scripts/bundle-macos.sh
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "target/release/bundle/Kube UI.app" "$ZIP"

git tag "$TAG"
git push origin HEAD "$TAG"
if [[ -n "$NOTES" ]]; then
    gh release create "$TAG" "$ZIP" --verify-tag --title "Kube UI $VERSION" --notes-file "$NOTES"
else
    gh release create "$TAG" "$ZIP" --verify-tag --title "Kube UI $VERSION" --generate-notes
fi
