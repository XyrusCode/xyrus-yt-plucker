# Extension ↔ Desktop Integration Spec

This doc is the source of truth for the protocol between the **browser extension**
(`Xyrus-YT-Plucker-Extension`) and the **Tauri v2 desktop app** (`video-plucker`).
Both repos have a copy — keep them in sync.

---

## Architecture

```
Browser Extension (Chrome/Edge)          Desktop App (Tauri v2)
┌──────────────────────────────┐        ┌──────────────────────────┐
│  right-click → "Pluck This"  │        │                          │
│  popup → "Open in Desktop"   │        │  yt-plucker:// URL       │
│              │               │        │       │                  │
│              ▼               │        │       ▼                  │
│  background.js builds URL    │──OS──▶│  single-instance argv    │
│  yt-plucker://analyze?...    │        │       │                  │
│                              │        │       ▼                  │
│                              │        │  handle_deep_link_argv() │
│                              │        │       │                  │
│                              │        │       ├─ emit event      │
│                              │        │       └─ PendingDeepLink │
│                              │        │              │           │
│                              │        │              ▼           │
│                              │        │  frontend main.js        │
│                              │        │  handleDeepLink(payload) │
└──────────────────────────────┘        └──────────────────────────┘
```

## Protocol URL Format

```
yt-plucker://{action}?url={encoded_url}&quality={encoded_quality}
```

| Param    | Required | Description                                     |
|----------|----------|-------------------------------------------------|
| `action` | yes      | `analyze` (fetch metadata) or `pluck` (download)|
| `url`    | yes      | URL-encoded video/page URL                      |
| `quality`| no       | Format code (e.g. `bestvideo+bestaudio`)         |

### Examples

```
yt-plucker://analyze?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3DUMnizvKZpSc
yt-plucker://pluck?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3DUMnizvKZpSc&quality=22
```

## Extension → Desktop Flow

### 1. Context Menu ("Pluck This Video")

`background.js` detects a right-click on a supported video page, builds the
protocol URL, and opens it:

```js
// Chrome blocks chrome.tabs.create with custom schemes, so we use a
// data: URL redirect — the page-initiated navigation is allowed.
chrome.tabs.create({
  url: `data:text/html;charset=utf-8,<script>location.href='${protocolUrl}'</script>`
});
```

### 2. Popup ("Open in Desktop App")

The extension popup sends a `launchDesktopApp` message to the background
service worker, which launches the protocol URL the same way.

## Desktop Deep-Link Handling

### Startup (single-instance callback)

```rust
tauri_plugin_single_instance::init(|app, argv, _cwd| {
    show_main_window(app);
    handle_deep_link_argv(app, &argv);
})
```

`handle_deep_link_argv` parses every `yt-plucker://` arg with `url::Url`,
builds a `DeepLinkPayload`, and:
1. Emits `deep-link-received` event to the frontend
2. Stores the payload in `PendingDeepLink` state (fallback for startup race)

### Race Condition Fix

The frontend webview may not have loaded when the single-instance callback
fires. `app.emit()` silently drops events with no listeners.  To handle this:

- `PendingDeepLink` (`Mutex<Option<DeepLinkPayload>>`) stores the last payload
- `consume_deep_link` Tauri command returns and clears the stored payload
- Frontend calls `invoke("consume_deep_link")` on startup *after* registering
  the `listen("deep-link-received")` handler

Both the event listener and the startup poll share `handleDeepLink()`:

```js
async function handleDeepLink({ action, url, quality }) {
  showView("download");
  urlInput.value = url;
  if (action === "analyze") await analyze();
  else if (action === "pluck") { /* analyze + pluck */ }
}

listen("deep-link-received", (event) => handleDeepLink(event.payload));

// Startup fallback
invoke("consume_deep_link").then(payload => {
  if (payload) handleDeepLink(payload);
});
```

## Protocol Registration

The `tauri_plugin_deep_link` plugin handles OS-level protocol registration
cross-platform:

```json
// tauri.conf.json
"plugins": {
  "deep-link": {
    "desktop": {
      "schemes": ["yt-plucker"]
    }
  }
}
```

## Supported Sites

The extension detects these platforms and builds appropriate `yt-plucker://` URLs:

| Platform  | URL Pattern                   |
|-----------|-------------------------------|
| YouTube   | `youtube.com/watch?v=...`     |
| X/Twitter | `x.com/*/status/*`            |
| TikTok    | `tiktok.com/@*/*`             |

## Version Compatibility

| Extension | Desktop | Protocol |
|-----------|---------|----------|
| 1.1.0     | 4.3.0   | `yt-plucker://analyze?...` / `yt-plucker://pluck?...` |
| 1.2.5     | 4.4.0   | Same protocol; cookies.txt export feeds the desktop Cookie Manager |
| 1.2.5     | 4.5.1   | Same protocol; failure UX overhaul (Queue-for-later, issue reporting, self-healing yt-dlp) |

## Key Files

| Repo      | File                              | Role                              |
|-----------|-----------------------------------|-----------------------------------|
| Extension | `extension/background.js`         | Build & launch protocol URLs      |
| Extension | `extension/popup/popup.js`        | "Open in Desktop App" button      |
| Desktop   | `src-tauri/src/lib.rs`            | Parse argv, emit event, state mgmt|
| Desktop   | `src/main.js`                     | Frontend handler + startup poll   |
| Desktop   | `src-tauri/tauri.conf.json`       | deep-link plugin config           |
| Both      | `docs/EXTENSION_DESKTOP_INTEGRATION.md` | This doc                   |

---

*Last updated: v4.13.1 (ChromeCookieUnlock plugin actually loaded; x.com cookie alias)*
