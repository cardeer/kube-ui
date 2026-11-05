# Product

<!-- impeccable:product-schema 1 -->

## Platform

native macOS desktop (Rust + egui immediate-mode GUI)

<!-- Not a web page and no iOS/Android design language: no HTML/CSS, so web rules and the
     HTML design detector do not apply. Styling is egui Visuals/Style in src/app/theme.rs. -->

## Users

One developer (the author), working on the NCS platform's Kubernetes clusters (dev, staging, prod, reached through a Rancher proxy). The app is open all day, on a second screen or behind the editor: glanced at constantly, with log tails and port forwards running in the background.

## Product Purpose

A native desktop replacement for the everyday `kubectl` loop: see pods, services and deployments; read and follow logs; inspect a pod's env, config, probes and events; and keep port forwards alive. Success is never reaching for the terminal for these jobs, and finding the answer at a glance.

## Positioning

A personal, fast, native tool tuned to one user's clusters and habits rather than a general cluster dashboard: several clusters open side by side as tabs, merged multi-pod logs (like `stern`), saved port forwards that resume on launch, and remembered selections, filters and layout.

## Operating Context

- Several cluster tabs at once (separate kubeconfig files and contexts); RBAC is namespace-scoped, so cluster-wide listing is often forbidden.
- Long-running log tabs and port forwards in the background; frequent glances to check pod status, restarts and errors.
- macOS, launched from /Applications or with `cargo run`.

## Capabilities and Constraints

- Pods, Services, Deployments lists with per-page filters; multi-namespace selection; detail drawer (overview, containers/env, volumes, endpoints, events); YAML; delete pod, scale, rollout restart.
- Logs: tabs, multi-pod merged timeline, paging back through history, resizable/expandable/collapsible panel.
- Port forwards: saved, auto-resume, open in browser.
- Immediate-mode GUI (egui 0.33): no CSS, no web fonts loading at runtime; fonts must be embedded; icons come from Phosphor.
- Production clusters get no special visual treatment (confirmed): all clusters are presented the same way.

## Product Principles

- Glanceable first: state (running, failing, restarting, live) readable from across the desk without reading text.
- Dense but calm: lots of rows and lines on screen, with nothing competing for attention that isn't a problem.
- Remember everything: the app reopens exactly as it was left.
- Never block: slow or forbidden API calls degrade one area, never the whole window.

## Brand Commitments

- Visual direction is the category standard, by choice: a dark developer tool, conventions embraced and executed at full craft. Quality bar: Lens / OpenLens.
- Controls must look made for this app, never like a stock component library; nothing toy-like (no bubbly radii or playful styling).
- Layout stays as built: cluster tabs on top, navigation on the left, detail drawer on the right, logs at the bottom.
