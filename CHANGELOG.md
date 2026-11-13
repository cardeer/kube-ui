# Changelog

All notable changes to Kube UI are listed here. Versions follow `version` in `Cargo.toml`
and match the `v<version>` git tags.

## [Unreleased]

### Added
- Log lines can be selected with the mouse: drag across them, double-click a word,
  triple-click a line, ⌘A to select them all, and ⌘C to copy. The Copy button takes the
  selection when there is one, and all the visible lines otherwise.
- Terminal selection is easier to see and use: drag to select, double-click a word,
  triple-click a line, ⌘A to select the screen, and ⌘C copies it (with a "Copied"
  notification).

### Changed
- The terminal has more padding, and a scrollback bar in its own gutter on the right, so
  nothing draws over the text. Log lines keep room for their scrollbars too, so the last
  line is no longer half-hidden under one.

## [1.1.0] - 2026-11-13

### Added
- Bottom panel tabs have a right-click menu: Close, Close others, Close tabs to the
  left / right, and Close all. ⌘W closes the selected one while the panel has focus
  (you last clicked in it, or just opened a tab there); ⇧⌘W (File > Close Logs/Terminal
  Tab) closes it from anywhere.
- The YAML editor has undo and redo (⌘Z, ⇧⌘Z or ⌘Y). Brackets and quotes close
  themselves, step over their closing partner, and wrap a selection; Backspace removes an
  empty pair at once, and one indent level in leading spaces.
- More YAML editor shortcuts: ⌘L select line, ⇧⌘D duplicate line, ⇧⌘K delete line,
  ⌥↑ / ⌥↓ move lines, ⌘] / ⌘[ indent and outdent, ⌘↩ new line below, ⌥⌫ / ⌘⌫ delete
  a word / to the line start, ⌥⌦ delete the next word, ⌘↑ / ⌘↓ go to the start / end.

### Changed
- Scrolling in the YAML editor moves along one axis at a time: a swipe keeps to the
  direction it started in, so it no longer drifts sideways while scrolling down.
- At most three notifications show at once; older ones make way for new ones. A repeated
  message shows once with a count (×3), each has a × to dismiss it, and long errors are
  cut short.

### Fixed
- Typing in the YAML editor no longer jumps back to the top when the YAML becomes invalid
  (for example after typing an opening quote) or a notice appears above it.

## [1.0.1] - 2026-11-12

### Changed
- More breathing room in the detail drawer: wider gaps between sections, details rows,
  conditions and label chips, so they're easier to read.
- Table rows are taller, with more padding inside each cell.

### Added
- Copying something (a label, an image, logs, the terminal screen) shows a short
  "Copied" notification.

## [1.0.0] - 2026-11-12

### Changed
- The interface is rebuilt on iced 0.14 (from egui). Everything works as before; long lists,
  logs and the terminal draw only what's on screen.
- On macOS, "Check for updates" and the version link moved from the top bar to the Kube UI
  menu. The top bar still shows Update, Installing and Restart when one applies.
- The Containers tab is easier to read: state and restarts are badges next to the container
  name, the image shows its `name:tag` prominently under the registry path (click to copy),
  image, pull policy, working dir and ports line up in one grid, resources are a small table,
  and environment variables are a striped two-column table of name and value.
- Table cells stay on one line and cut long text with an ellipsis; hover for the full value.
  Port Forwards uses the same table as the other pages.
- Sidebar labels no longer wrap.

### Added
- Native macOS menu bar. The Kube UI menu has About, Check for Updates…, What's New and
  Settings… (⌘,). There's also a Window menu and a Help menu with links to GitHub and the
  update log.
- Keyboard shortcuts:
  - ⌘T new tab, ⌘O open a kubeconfig.
  - ⌘W closes the YAML window, then the detail drawer, then the cluster tab.
  - ⌘R reload, ⌘J show or hide logs and terminals.
  - ⌥⌘1–5 go to Overview, Pods, Deployments, Services and Port Forwards.
  - ⌘⇧] and ⌘⇧[ switch cluster tabs. ⌘1–8 pick a tab and ⌘9 the last one.
- Resizable table columns: drag a column's edge in the header, double-click it to reset.
  Widths are remembered per page.
- Indent guides in the YAML editor, with the guide around the cursor highlighted.

### Fixed
- Namespace and context pickers no longer stay cut off after first opening while the
  list was still loading or short.
- The YAML viewer and editor scroll with the mouse wheel and trackpad again.
- Following logs no longer resets the sideways scroll position.

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
