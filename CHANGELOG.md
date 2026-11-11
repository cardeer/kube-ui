# Changelog

All notable changes to Kube UI are listed here. Versions follow `version` in `Cargo.toml`
and match the `v<version>` git tags.

## [Unreleased]

## [0.3.0] - 2026-11-11

### Added
- "What's new" window with the release notes. Open it from the version number in the top
  bar or from Settings. It also opens once after the app updates to a new version.

### Changed
- API errors are now short, readable messages instead of nested debug output. For example, a
  403 now says "No permission to list pods in namespace "x". Signed in as …" in place of the
  raw `ApiError … Status { … }` dump. 401, 404, 409 and 422 responses also get plain wording.
- List errors appear in a tinted callout with one row per failed namespace, and the extra
  "Nothing here." line no longer shows under an error.

## [0.2.1] - 2026-11-11

### Fixed
- In-app updates no longer hang on "Installing".

## [0.2.0] - 2026-11-11

### Added
- "Check for updates" button in the top bar.
- Install/update and release scripts (`scripts/install.sh`, `scripts/release-macos.sh`).

## [0.1.0] - 2026-11-10

First tagged release.

### Added
- Native Kubernetes desktop client: kubeconfig, context and namespace pickers, workload,
  network and port-forward views.
- Pod shell, and creating resources from templates.
- ANSI colours in log lines.
- Regex filtering for tables and logs.
- Find in the YAML editor, and search for environment variables.

### Fixed
- Dropdown sizing.

[Unreleased]: ../../compare/v0.3.0...HEAD
[0.3.0]: ../../compare/v0.2.1...v0.3.0
[0.2.1]: ../../compare/v0.2.0...v0.2.1
[0.2.0]: ../../compare/v0.1.0...v0.2.0
[0.1.0]: ../../tree/v0.1.0
