---
version: 1
slug: "src-app-rs"
primary_target: "src/app.rs"
related_targets: ["src/app/workspace.rs","src/app/logs.rs"]
---

# Main window

Scope: the whole app window (Operate mode). Audience and job are in PRODUCT.md. Constraint: layout regions stay where they are.

## Direction contract

THESIS: The category standard, done properly: a dark Kubernetes desktop tool at Lens/OpenLens craft, where every control is drawn for this app. It refuses stock egui widgets, default grey frames and toy radii.

OWN-WORLD: Cool near-black neutrals in three layers (chrome #101216, content #15171c, raised #1a1d23), hairline borders, one blue accent (#4c8dff) for selection, focus and primary action only; green/amber/red only for state. Inter for UI with a semibold step for titles, JetBrains Mono for names, logs and data. 4-6px radii, flat fills, soft offset shadows on floating layers only.

STORY: Glance across the desk and see what is failing or restarting; click through to the drawer for why; tail logs underneath; never wonder where you are.

FIRST VIEWPORT: Cluster tabs in the chrome strip, the active one joined to the content below; picker row of quiet fields; left nav with accent-tinted selection; Pods title with count and filter; dense 30px rows with state dot, restart ramp, muted age/node; drawer at right with underline tabs and one primary action; logs at the bottom on a darker well.

FORM: Category standard (canon), chosen by the user; seed key e3c9314f.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
