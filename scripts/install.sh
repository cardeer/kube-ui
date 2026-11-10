#!/usr/bin/env bash
# Installs or updates Kube UI from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/cardeer/kube-ui/main/scripts/install.sh | bash
#
# Options:
#   --version <tag>   install a specific release (e.g. v0.1.0) instead of the latest
#   --force           reinstall even when that version is already installed
#   --dir <path>      install somewhere other than /Applications (e.g. ~/Applications)
set -euo pipefail

REPO="cardeer/kube-ui"
APP_NAME="Kube UI"
BUNDLE_ID="dev.kube-ui.app"
DIR="/Applications"
TAG=""
FORCE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) TAG="${2:?--version needs a tag}"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --dir) DIR="${2:?--dir needs a path}"; shift 2 ;;
        -h|--help) echo "Usage: install.sh [--version <tag>] [--force] [--dir <path>]"; exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31mError:\033[0m %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "Kube UI releases are macOS apps."
ARCH="$(uname -m)"
[[ "$ARCH" == "arm64" ]] || die "Releases are built for Apple Silicon (arm64) only; this Mac is $ARCH. Build from source instead: ./scripts/bundle-macos.sh --install"

# Which release. The latest tag comes from where github.com's /releases/latest redirects,
# which (unlike the REST API) has no anonymous rate limit.
if [[ -z "$TAG" ]]; then
    LATEST="$(curl -fsSIL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
        || die "Could not reach GitHub."
    TAG="${LATEST##*/tag/}"
    [[ "$LATEST" == */tag/* && -n "$TAG" ]] || die "No release found for $REPO."
fi
# Asset names follow scripts/release-macos.sh: Kube-UI-<version>-macos-<arch>.zip
URL="https://github.com/$REPO/releases/download/$TAG/Kube-UI-${TAG#v}-macos-$ARCH.zip"
NEW_VERSION="${TAG#v}"

# What's installed now.
APP="$DIR/$APP_NAME.app"
OLD_VERSION=""
if [[ -f "$APP/Contents/Info.plist" ]]; then
    OLD_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || true)"
fi
if [[ "$OLD_VERSION" == "$NEW_VERSION" && $FORCE -eq 0 ]]; then
    say "$APP_NAME $NEW_VERSION is already installed and up to date (pass --force to reinstall)."
    exit 0
fi
if [[ "$OLD_VERSION" == "$NEW_VERSION" ]]; then
    say "Reinstalling $APP_NAME $NEW_VERSION"
elif [[ -n "$OLD_VERSION" ]]; then
    say "Updating $APP_NAME $OLD_VERSION → $NEW_VERSION"
else
    say "Installing $APP_NAME $NEW_VERSION"
fi

# Download and unpack.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
say "Downloading $(basename "$URL")"
curl -fL --progress-bar -o "$TMP/app.zip" "$URL" || die "Download failed: no macOS $ARCH build for $TAG at $URL"
ditto -x -k "$TMP/app.zip" "$TMP/unzipped"
NEW_APP="$TMP/unzipped/$APP_NAME.app"
[[ -d "$NEW_APP" ]] || die "The download doesn't contain $APP_NAME.app."
# The app is ad-hoc signed, not notarized: drop the quarantine flag so it opens normally.
xattr -dr com.apple.quarantine "$NEW_APP" 2>/dev/null || true

# Quit the running app (it reopens afterwards).
WAS_RUNNING=0
if pgrep -xq kube-ui; then
    WAS_RUNNING=1
    say "Quitting $APP_NAME"
    osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
    for _ in $(seq 20); do pgrep -xq kube-ui || break; sleep 0.5; done
    pkill -x kube-ui 2>/dev/null || true
fi

# Replace it. Use sudo only when the folder isn't writable.
SUDO=""
mkdir -p "$DIR" 2>/dev/null || true
if [[ ! -w "$DIR" ]] || { [[ -e "$APP" ]] && [[ ! -w "$APP" ]]; }; then
    say "$DIR needs administrator rights; you may be asked for your password."
    SUDO="sudo"
fi
$SUDO rm -rf "$APP"
$SUDO ditto "$NEW_APP" "$APP"
say "Installed $APP ($NEW_VERSION)"

if [[ $WAS_RUNNING -eq 1 ]]; then
    open "$APP"
fi
