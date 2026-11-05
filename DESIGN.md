---
name: Kube UI
description: A native dark desktop tool for the everyday kubectl loop, built to Lens/OpenLens craft.
colors:
  chrome: "#101216"
  content: "#15171c"
  raised: "#1b1e25"
  well: "#0e1014"
  field: "#1f232b"
  field-hover: "#272c36"
  field-active: "#2e3441"
  stripe: "#191c22"
  border: "#262a33"
  border-strong: "#343a47"
  text: "#e6e8ee"
  text-muted: "#9aa1b1"
  text-faint: "#7c8496"
  accent: "#4c8dff"
  accent-hover: "#669fff"
  accent-bg: "#1d2a45"
  accent-text: "#cfdfff"
  state-ok: "#3ecf8e"
  state-warn: "#f5a524"
  state-bad: "#f2555a"
  state-idle: "#6b7282"
  danger-fill: "#c93a40"
typography:
  display:
    fontFamily: "Inter SemiBold"
    fontSize: "19px"
    fontWeight: 600
  headline:
    fontFamily: "Inter SemiBold"
    fontSize: "16.5px"
    fontWeight: 600
  title:
    fontFamily: "Inter SemiBold"
    fontSize: "13.5px"
    fontWeight: 600
  body:
    fontFamily: "Inter"
    fontSize: "13.5px"
    fontWeight: 400
  label:
    fontFamily: "Inter SemiBold"
    fontSize: "12px"
    fontWeight: 600
  small:
    fontFamily: "Inter"
    fontSize: "11.5px"
    fontWeight: 400
  mono:
    fontFamily: "JetBrains Mono"
    fontSize: "12.5px"
    fontWeight: 400
rounded:
  check: "3px"
  control: "4px"
  item: "6px"
  tab: "7px"
  float: "8px"
  window: "10px"
spacing:
  hair: "2px"
  xs: "4px"
  sm: "6px"
  md: "8px"
  lg: "14px"
  xl: "16px"
  xxl: "20px"
  row: "30px"
  header-row: "26px"
  control-height: "26px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "#ffffff"
    typography: "{typography.title}"
    rounded: "{rounded.control}"
    padding: "5px 10px"
    height: "{spacing.control-height}"
  button-danger:
    backgroundColor: "{colors.danger-fill}"
    textColor: "#ffffff"
    typography: "{typography.title}"
    rounded: "{rounded.control}"
    padding: "5px 10px"
  button-default:
    backgroundColor: "{colors.field}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "5px 10px"
    height: "{spacing.control-height}"
  button-default-hover:
    backgroundColor: "{colors.field-hover}"
  button-default-active:
    backgroundColor: "{colors.field-active}"
  icon-button:
    textColor: "{colors.text-muted}"
    rounded: "{rounded.control}"
    size: "24px"
  icon-button-hover:
    backgroundColor: "{colors.field-hover}"
    textColor: "{colors.text}"
  input-field:
    backgroundColor: "{colors.chrome}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "5px 8px"
  nav-item:
    textColor: "{colors.text-muted}"
    typography: "{typography.body}"
    rounded: "{rounded.item}"
    height: "32px"
  nav-item-hover:
    backgroundColor: "{colors.field}"
    textColor: "{colors.text}"
  nav-item-selected:
    backgroundColor: "{colors.accent-bg}"
    textColor: "{colors.accent-text}"
    typography: "{typography.title}"
  table-row:
    textColor: "{colors.text}"
    typography: "{typography.mono}"
    height: "{spacing.row}"
  table-row-stripe:
    backgroundColor: "{colors.stripe}"
  table-row-selected:
    backgroundColor: "{colors.accent-bg}"
  badge-tinted:
    textColor: "{colors.state-warn}"
    typography: "{typography.small}"
    rounded: "{rounded.control}"
    padding: "2px 6px"
  badge-filled:
    backgroundColor: "{colors.state-bad}"
    textColor: "#ffffff"
    typography: "{typography.small}"
    rounded: "{rounded.control}"
    padding: "2px 6px"
  tag:
    backgroundColor: "{colors.stripe}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "2px 6px"
  cluster-tab-active:
    backgroundColor: "{colors.content}"
    textColor: "{colors.text}"
    typography: "{typography.title}"
    rounded: "{rounded.tab}"
    padding: "7px 6px 8px 12px"
  log-tab-selected:
    backgroundColor: "{colors.field}"
    textColor: "{colors.text}"
    typography: "{typography.title}"
    rounded: "{rounded.item}"
    padding: "3px 4px 3px 10px"
  card-toast:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.text}"
    rounded: "{rounded.float}"
    padding: "10px 14px"
---

# Design System: Kube UI

## Overview

**Creative North Star: "The Night Shift Console"**

A dark Kubernetes desktop tool in the category standard, executed at Lens/OpenLens craft: conventions embraced, every control drawn for this app rather than borrowed from egui's stock set. The window is built from four cool near-black grounds stacked by luminance (well below content, content below raised, chrome framing the edges), separated by hairlines rather than boxes. Nothing is decorative; the screen is dense with rows and log lines and calm everywhere a problem is not.

Colour carries meaning, not mood. One blue accent marks where you are and what you will do (selection, focus, the single primary action). Green, amber and red appear only when they report state, so a failing pod reads from across the desk before any text is read. Inter sets the interface; JetBrains Mono sets everything that is data (resource names, ages, IPs, logs), which also gives tabular figures in every table.

Controls are quiet and made in-house: frameless icon buttons that fill only on hover, a hand-drawn checkbox and caret, underline tabs, small 4px radii. Floating layers (menus, dialogs, toasts) are the only things that lift, on soft offset shadows.

**Key Characteristics:**
- Four tonal grounds (chrome, content, raised, well) plus three field steps; depth by luminance, not boxes.
- One blue accent, used only for selection, focus and the primary action.
- State colours reserved for state; production clusters get no special treatment.
- Inter for UI with a single semibold step; JetBrains Mono for every data value.
- Dense 30px table rows, 26px controls, 4-6px radii.
- Shadows only on floating layers.

## Colors

Cool, blue-tinted near-blacks at very low chroma, one saturated blue, and a three-colour state set.

### Primary
- **Signal Blue** (accent): the filled primary button, the drawer's active underline tab, checkbox fill, active-widget borders. Hover step is **Lit Signal Blue** (accent-hover), which also colours hyperlinks, the text cursor and the selected nav icon.
- **Selection Navy** (accent-bg): fill of the selected table row, selected nav item and text selection.
- **Pale Signal** (accent-text): text on Selection Navy, and the hairline of text selection and focused fields.

### Tertiary (state)
- **Running Green** (state-ok): Running/Succeeded/Completed pods, Connected, live log indicator, success toasts.
- **Pending Amber** (state-warn): pending, creating, terminating, connecting; restart counts 1-4 as a tinted badge.
- **Failing Red** (state-bad): failed and errored states, restart counts of 5+ as a filled badge, error toasts. **Delete Red** (danger-fill) is the darker fill of the destructive confirm button, bordered in Failing Red.
- **Idle Slate** (state-idle): a cluster tab that has not connected.

### Neutral
- **Chrome Black** (chrome): top bar, nav rail, and the inset ground of text fields.
- **Content Graphite** (content): the main list ground; also the active cluster tab and picker row, which share it.
- **Raised Graphite** (raised): detail drawer, dialogs, menus, toasts.
- **Well Black** (well): the log panel, a step below content.
- **Field / Field Hover / Field Active** (field, field-hover, field-active): resting, hovered and pressed fills of buttons and combo boxes; the selected log tab.
- **Stripe** (stripe): alternate table rows and tag fills.
- **Hairline / Strong Hairline** (border, border-strong): dividers and resting control borders; hovered controls, windows and toasts.
- **Ink / Muted Ink / Faint Ink** (text, text-muted, text-faint): primary text; labels, inactive tabs, nav and column headers; hints, zero restart counts and stale log tabs. Faint Ink holds at least 4.5:1 on Content Graphite.

### Named Rules
**The One Blue Rule.** The accent marks selection, focus and the one primary action in a group. It never decorates, never colours a heading, and never appears twice as a filled button in the same group.

**The State Only Rule.** Green, amber and red mean running, pending and failing. They never brand a cluster, an environment or a section; production is drawn exactly like dev.

## Typography

**Display Font:** Inter SemiBold (embedded; egui fallbacks behind it)
**Body Font:** Inter Regular
**Label/Mono Font:** JetBrains Mono Regular

**Character:** A neutral grotesk for the interface with one semibold step for emphasis, set against a monospace for anything copied from the cluster. egui has no font weights, so "semibold" is a separate embedded family, not a weight axis.

### Hierarchy
- **Display** (semibold, 19px): page titles ("Pods", "Port Forwards", "Settings"), followed by a muted count.
- **Headline** (semibold, 16.5px): the resource name at the top of the drawer, wrapping beside its icon buttons.
- **Title** (semibold, 13.5px): the selected state of nav items, tabs and log tabs; primary and danger button labels. Selection is shown by weight as well as colour.
- **Body** (regular, 13.5px): all interface text, field values, inactive tabs, field labels in the picker row.
- **Label** (semibold, 12px, Muted Ink, sentence case): table column headers and drawer section labels ("Details").
- **Small** (regular, 11.5px): badges (semibold), tags, hints and secondary lines.
- **Mono** (regular, 12.5px): resource names, ages, ready counts, IPs, detail values, YAML and log lines.

### Named Rules
**The Data Is Mono Rule.** Any value that came from the cluster (names, ages, counts, addresses, logs) is set in JetBrains Mono; any word the app wrote is set in Inter.

**The Middle Ellipsis Rule.** Long resource names shorten from the middle and keep the tail (the ReplicaSet hash and pod suffix); the full name is on hover.

## Layout

Fixed regions, as built: cluster tabs and a picker row in the top chrome; a 224px nav rail on the left, with collapsible groups (Workloads, Network) above the app pages; the list on the content ground; a resizable detail drawer on the right (default 460px, minimum 340px); logs in a resizable bottom panel (minimum 160px, collapsible to 40px, expandable to almost full height).

Density is high and rhythmic. Table rows are exactly 30px with no extra item spacing; headers are 26px; controls are 26px tall with 5px/10px padding. The base item gap is 8px horizontal by 6px vertical. Content insets are 20px left/right and 16px top; the drawer insets 18px by 14px; chrome and the log well inset 14px. Nav items are 32px tall with 2px between them and a 14px gap separating resource pages from app pages. Detail grids use 16px by 7px spacing with a 110px key column. Section labels sit 16px below the previous block and 4px above their content.

## Elevation & Depth

Hybrid: the docked window is flat and conveys depth by ground luminance (well, chrome, content, raised), with 1px hairlines between regions. Only floating layers cast shadows, and those shadows are soft, offset straight down, and black at roughly half opacity.

### Shadow Vocabulary
- **Window** (offset 0 10px, blur 28px, black at alpha 130): dialogs and floating windows, with a Strong Hairline border.
- **Popup** (offset 0 6px, blur 18px, black at alpha 120): menus and combo-box popups.
- **Card** (offset 0 8px, blur 22px, black at alpha 140): toasts.

### Named Rules
**The Docked Is Flat Rule.** Panels, rows, tabs and buttons never cast shadows. If it is docked into the window, depth comes from its ground colour.

## Shapes

Small, firm corners that grow slightly with the size of the element: 3px on the 15px checkbox, 4px on buttons, fields, badges and tags (6px on a 15px box reads as a radio button), 6px on nav items and log tabs, 7px on the top corners only of cluster tabs, 8px on menus and toasts, 10px on dialogs. Borders are always 1px hairlines; the one 2px line is the accent underline of the active drawer tab. Status dots are 8px circles. Nothing is pill-shaped or bubbly.

## Components

### Buttons
Quiet by default, with one loud action per group.
- **Shape:** gently squared (4px).
- **Primary:** Signal Blue fill, white semibold label, 1px Lit Signal Blue border. One per group (the drawer's "Logs", a dialog's confirm).
- **Default:** Field fill with a Hairline border; hover steps to Field Hover with Strong Hairline; press steps to Field Active with an accent border.
- **Danger:** Delete Red fill, white semibold label, Failing Red border; used only for destructive confirmation.
- **Icon button:** 24px frameless square, Phosphor glyph at 14px in Muted Ink; on hover it fills with Field Hover and the glyph turns Ink. Always carries a hover hint. Opens a menu when used as an icon menu (180px minimum width).

### Chips
- **Badge:** small semibold text in a 4px-radius pill, either tinted (state colour text on a 16% wash of itself) or filled (state colour fill, white text). Restarts use the ramp: 0 as Faint Ink mono, 1-4 tinted amber, 5+ filled red.
- **Tag:** small mono text on Stripe with a Hairline border, 4px radius, 2px/6px padding; hover lifts to Field Hover; click copies.

### Cards / Containers
- **Corner Style:** 8px on toasts, 10px on dialogs; docked panels are square.
- **Background:** Raised Graphite for drawer, dialogs, menus and toasts; Well Black for logs.
- **Shadow Strategy:** floating layers only (see Elevation & Depth).
- **Border:** Strong Hairline on floating layers.
- **Internal Padding:** toasts 10px/14px; drawer 14px/18px; dialogs 16px.

### Inputs / Fields
- **Style:** text fields sit inset on Chrome Black with a Hairline border, 4px radius, 5px/8px margin. The search field leads with a Phosphor magnifier inside Faint Ink hint text. Combo boxes use the Field fills and a Phosphor caret (up when open) in place of egui's triangle.
- **Focus:** border shifts to the Pale Signal hairline; the text cursor is a 1.5px Lit Signal Blue line.
- **Checkbox:** a 15px square at 3px radius; off is Field with a Strong Hairline, on is Signal Blue with a white check. The label is Muted Ink when off and Ink when on.

### Navigation
- **Nav rail:** 32px items on Chrome Black: 16px glyph at 12px inset, label at 38px, optional right-aligned count. Resting is Muted Ink; hover fills Field and turns Ink; selected fills Selection Navy, the label goes semibold Pale Signal and the glyph Lit Signal Blue.
- **YAML editor:** JetBrains Mono on Content Graphite in a hairline frame, with no soft wrap (long lines scroll sideways). The line-number gutter sits on Well with a Border hairline; numbers are Faint Ink, Ink on the cursor line, Bad on a YAML error line (which also gets a faint Bad wash). The cursor line fills Field. Indent guides are 1px Border Strong lines every two columns. Syntax: keys in soft Signal Blue (syntax-key), strings and block scalars sage (syntax-string), numbers, booleans and null amber (syntax-literal), comments Faint Ink, and `:`/`-` punctuation Muted Ink.
- **Nav groups:** a 32px header (12px caret, 16px glyph, semibold label) folds its pages away; a closed group over the current page fills Selection Navy. Pages inside a group are 30px, text only, hung off a 1px Border Strong guide under the group glyph; the selected page fills Selection Navy from the guide inward and the guide beside it turns a 2px Signal Blue line. Folded groups are remembered.
- **Cluster tabs:** a status dot and the cluster title in the chrome strip; the active tab fills with Content Graphite, rounds only its top corners and opens seamlessly into the picker row below.
- **Underline tabs:** drawer sections spaced 18px apart; active is semibold Ink over a 2px accent underline, inactive is Muted Ink; a Hairline rule runs under the strip.
- **Log tabs:** 6px-radius frames with a close button; selected is Field with Strong Hairline, live tabs are Muted Ink, stopped tabs Faint Ink.

### Data Table
The core surface. Striped rows (Stripe on alternates), 30px tall, click-to-select into Selection Navy. Column order puts state first after the name (status, restarts, ready) so it survives the drawer opening. Status cells are an 8px state dot plus the status word in its state colour when not Running; names and data are mono, secondary columns (age, namespace, node) Muted Ink mono.

### Merged Log View
Logs render in mono on the Well. When several pods merge into one timeline, each line is tagged with its pod in one of eight fixed, readable-on-dark source colours (see sidecar), distinct from the state colours.

## Do's and Don'ts

### Do:
- **Do** draw every control in the app's vocabulary: Phosphor glyphs, the custom caret and checkbox, frameless icon buttons with hover fills.
- **Do** separate regions by ground colour (chrome #101216, content #15171c, raised #1b1e25, well #0e1014) and 1px hairlines.
- **Do** keep one filled Signal Blue button per action group; everything else is Default or an icon button.
- **Do** set every cluster-derived value in JetBrains Mono and every app-written word in Inter.
- **Do** keep table rows at 30px and controls at 26px.
- **Do** give every icon-only control a hover hint.

### Don't:
- **Don't** use stock egui frames, the filled combo triangle, or egui's default grey; use the theme's frames and the Idle Slate or Muted Ink tokens.
- **Don't** use green, amber or red for anything except state, and don't give production clusters a distinct colour or treatment.
- **Don't** use radii above 10px, pill shapes or playful styling.
- **Don't** put shadows on docked panels, rows, tabs or buttons.
- **Don't** add uppercase eyebrow or kicker labels; section and column labels are sentence-case 12px semibold Muted Ink.
