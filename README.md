# Kube UI

A native Kubernetes desktop UI written in Rust ([egui](https://github.com/emilk/egui) + [kube-rs](https://kube.rs)).

```bash
cargo run --release
```

## Install (macOS)

Builds `Kube UI.app` and copies it to `/Applications`. Run it again after pulling changes to update the app:

```bash
./scripts/bundle-macos.sh --install
```

Without `--install`, the bundle is only built, in `target/release/bundle/`. The app is ad-hoc signed for local use.

## Features

- **First-run setup**: pick a kubeconfig file (or use `$KUBECONFIG` / `~/.kube/config`). Add more files later in Settings.
- **Cluster tabs**: open several kubeconfig/context combos at once as tabs across the top (**+** lists every context of every kubeconfig). Each tab has its own namespace, lists and drawer, and tabs are restored on launch. Only the front tab watches the cluster.
- **Environment switcher**: switch a tab's kubeconfig file, context and namespaces with searchable pickers. In the namespace picker, click a name to show only that namespace, or tick checkboxes to combine several (listed in parallel and merged; a namespace you can't read shows a warning instead of hiding the rest). The last selection is remembered per tab and per context.
- **Workloads** (a collapsible sidebar group, like Lens): Overview, Pods, Deployments, Daemon Sets, Stateful Sets, Replica Sets, Replication Controllers, Jobs and Cron Jobs. **Network** holds Services. Click a row to open a detail drawer. Double-click a pod to open its logs. Workload drawers have logs for all their pods, YAML, events, and scale or rollout restart where the kind supports it.
- **Shell**: open a terminal into a running pod (**Shell** in the pod drawer, or per container on the Containers tab). It runs bash when the image has it, otherwise sh, in a tab in the bottom dock beside the logs. Full-screen programs (vim, top, less) work; the terminal resizes with the panel, the mouse wheel scrolls back through 5,000 lines, and you can drag to select and copy with ⌘C or paste with ⌘V. Ended sessions can be reconnected.
- **Create**: every list page has a **Create** button that opens the YAML editor on a template for that kind (pod, deployment, service, daemon set, stateful set, replica set, replication controller, job, cron job). The namespace defaults to the one you're viewing. After you create it, the new object opens in the drawer.
- **Sorting**: click any column header to sort ascending, again for descending, and a third time to go back to the default (namespace, then name). Names and values like `9/10` sort naturally (`pod-9` before `pod-10`), and Age, Duration and Last run sort by time. The sort is remembered per page and per cluster tab.
- **Actions**: every drawer's ⋯ menu has **Edit YAML** (a code editor with line numbers, syntax colours, indent guides, auto-indent on Enter, Tab/Shift+Tab to indent or outdent, ⌘/ to toggle comments, live YAML checking, and ⌘S to Apply; if someone else changed the object first, you get a conflict instead of overwriting their change), **View YAML** and **Delete** (with confirmation). Cron jobs have **Run now** (like `kubectl create job --from=cronjob/…`) and **Suspend/Resume**; jobs can be suspended and resumed; deployments can pause or resume a rollout; deployments, stateful sets and replica sets scale; deployments, daemon sets and stateful sets do a rollout restart.
- **Overview**: a card per workload kind with counts and a health bar (click one to open that list), plus a **Needs attention** list of failing pods, failed jobs and workloads short of ready. Click a row to jump to it.
  - **Pod drawer**:
    - **Overview**: status, node/IP, QoS, service account, conditions, ports, labels, annotations.
    - **Containers**: image, command and args, resources, environment variables, mounts, probes, last termination.
    - **Volumes**.
    - **Events**.
    - Actions: logs, port-forward, YAML, delete.
  - **Environment variables**: shows where each value comes from. Values from ConfigMaps are filled in. Values from Secrets stay masked until you click **Reveal**, and are hidden again when the drawer closes. **Copy as .env** copies all known values.
  - **Service drawer**: ports and targets, traffic policies, endpoints (click a pod to jump to it), events. Port-forward resolves the selector to a ready pod and maps `targetPort`.
  - **Deployment drawer**: scale, rollout restart, conditions, images, events.
- **Logs**: each log opens as a tab in the bottom panel (✕ on the tab or middle-click to close). The panel remembers its height; buttons at the right expand it to fill the window, collapse it to just the tab strip, and restore it. Live streaming with a container picker, timestamps, previous container, filter (plain text or regex via `.*`), ANSI colours and error/warn highlighting. Scroll to the top to load older lines (up to 100k kept).
  - **All pods in one view**: **Logs (all pods)** on a deployment or service, or **All replicas** on a pod, streams every matching pod into one timeline. Lines are merged by timestamp and tagged with a colored pod label, like `stern`. Click a pod chip to hide or show it; restart the tab to pick up pods from a new rollout.
- **Port forwards**: saved across restarts. Forwards that were running when you quit start again on launch, each using its own kubeconfig and context. Shows connection counts, with open-in-browser and copy.
- Each page's name filter matches name or namespace; toggle `.*` for a case-insensitive regex (e.g. `^api-|-worker$`). Filters are kept per cluster tab, including after a restart. The window reopens at its last size (and maximized, if it was).
- Lists update live through a Kubernetes watch (like `kubectl get -w`), and the open drawer refreshes every 5 seconds. Turn off Live updates in Settings for static lists that only change on Refresh.

Settings are stored in `~/Library/Application Support/kube-ui/settings.json` on macOS (the platform config dir elsewhere).

## Layout

- `src/kube_ops.rs`: Kubernetes API calls for lists, logs, actions and port-forward (no UI code)
- `src/details.rs`: full details for the drawer (env, resources, endpoints, events, …)
- `src/app.rs`: app shell: cluster tabs, sidebar, port forwards, dialogs; routes background results (sent over a channel) to the right tab
- `src/app/workspace.rs`: one cluster tab: connection, lists, detail drawer
- `src/app/logs.rs`: log tabs, including merged multi-pod streams
- `src/settings.rs`: persisted settings, including saved port forwards
- `scripts/bundle-macos.sh`: builds the `.app` bundle; `scripts/make-icon.swift` draws `assets/icon.png`, which is also embedded as the running app's Dock icon

Log streaming uses a separate HTTP/2-capable client because some API proxies (e.g. Rancher) never answer `follow=true` log requests over HTTP/1.1. Everything else, including port-forward (which needs an HTTP/1.1 upgrade), uses the default kube client.

When launched from Finder, the app reads your login shell's `PATH` so kubeconfig exec plugins (`aws`, `kubelogin`, …) can be found.

## Design

The visual system (tokens, fonts, controls) is in `src/app/theme.rs` and documented in `DESIGN.md`. For a quick visual check, `KUBE_UI_SCREENSHOT=out.png cargo run` waits for pods to load, opens the first pod's drawer and logs, saves a PNG of the window and quits.
