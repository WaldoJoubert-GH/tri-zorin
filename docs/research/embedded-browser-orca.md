# Embedded browser parity with Orca

Research date: 2026-09-22

## Executive summary

`tri-zorin` already has an embedded-browser foundation:

- macOS embeds live `WKWebView` pages beneath GPUI chrome.
- Linux runs WebKitGTK in a separate helper process and composites rendered frames in GPUI.
- Browser tabs have address normalization, navigation state, history, title updates, popup-to-new-tab handling, and explicit HTTP(S) policy.
- Windows now has a first embedded WebView2 backend; other unsupported platforms still fall back to opening the system browser.

The important distinction is scope:

1. **Embedded browser surface:** show and interact with a real page inside the app. This is already implemented on macOS and Linux, and the next obvious platform is Windows.
2. **Orca-like browser subsystem:** make every page a host-owned, stable, profile-aware resource that can be addressed by automation and managed consistently across tabs, worktrees, remote hosts, popups, downloads, and permissions.

The recommended path is to keep the existing native-webview approach for the first milestone, add a Windows WebView2 backend, and independently deepen the browser model around stable page identity and lifecycle. Do not switch the whole app to Electron solely to obtain Orca parity; Electron would be a large architectural migration and would not by itself solve remote routing or page-lifecycle design.

### Implementation status

The first implementation slice now exists in the working tree: `crates/ui/src/browser/windows.rs` hosts a child WebView2 page through Wry, with HTTP(S) navigation filtering, incognito storage, clipboard access, title/load state events, popup-to-new-tab routing, history, visibility, focus, and GPUI-bound geometry. The shared browser surface recognizes Windows as an embedded backend, and the browser fixture exercises Windows DOM navigation, history, same-document state, and native visibility without invoking Linux screenshot tools. A Windows-targeted `cargo check --locked -p zeron-ui --target x86_64-pc-windows-msvc --lib` passes. The corresponding fixture check reached compilation but could not complete because the drive was full and the MSVC linker reported `LNK1318`; no Windows runtime fixture has been executed yet.

## What Orca means by an embedded browser

Orca's browser is documented as a real Chromium browser embedded in a worktree pane. It provides address/history controls, DevTools, tabs, popups, downloads, profiles, remote-workspace routing, and agent automation.

Sources:

- [Orca browser overview](https://www.onorca.dev/docs/browser/overview)
- [Orca browser profiles](https://www.onorca.dev/docs/browser/profiles)
- [Orca browser source tree](https://github.com/stablyai/orca/tree/main/src/main/browser)

The source architecture has a useful invariant: the visible page and the page targeted by automation are the same host-owned browser object. Orca abstracts page creation behind renderer-hosted and offscreen/headless backends, but both register the resulting Electron `WebContents` in a central browser manager.

Relevant primary sources:

- [Orca browser backend](https://raw.githubusercontent.com/stablyai/orca/main/src/main/browser/browser-backend.ts)
- [Orca browser manager](https://raw.githubusercontent.com/stablyai/orca/main/src/main/browser/browser-manager.ts)
- [Orca page creation/lifecycle code](https://raw.githubusercontent.com/stablyai/orca/main/src/main/browser/browser-client-page-creation.ts)
- [Orca automation runtime](https://raw.githubusercontent.com/stablyai/orca/main/src/main/browser/browser-client-page-automation-runtime.ts)
- [Orca browser command bridge](https://raw.githubusercontent.com/stablyai/orca/main/src/main/browser/agent-browser-bridge-core-commands.ts)
- [PR: make embedded WebContents authoritative](https://github.com/stablyai/orca/pull/9633)
- [PR: browser automation/CDP bridge](https://github.com/stablyai/orca/pull/856)

The reusable design lesson is not specifically Electron. It is the ownership and identity relationship:

```text
UI tab / page handle
        |
        v
central browser registry
        |
        +--> native page backend
        +--> user input and rendering
        +--> automation
        +--> lifecycle, popup, download, permission policy
```

## Current `tri-zorin` architecture

### Shared browser model

`crates/ui/src/browser/mod.rs` owns the GPUI-facing `BrowserSurface`, browser chrome, URL draft, `PageState`, loading/error state, and browser events. `crates/ui/src/browser/model.rs` owns address normalization and the HTTP(S)-only navigation policy.

Current state is intentionally lightweight:

- `PageState` contains URL, title, loading, history availability, and error.
- `BrowserContext` represents an ephemeral website-data scope.
- Browser pages are owned by individual `BrowserSurface` entities.
- New-window requests become `BrowserEvent::NewTab` when they pass the navigation policy.
- Unsupported platforms call `cx.open_url` instead of embedding.

This is a good UI seam, but it is not yet a central browser/page registry. There is no durable or globally stable browser page ID, profile object, generation token, or automation target.

### macOS

`crates/ui/src/browser/macos.rs` uses Wry and native `WKWebView`/AppKit integration. It currently provides:

- live native page rendering below GPUI overlays;
- shared nonpersistent `WKWebsiteDataStore` behavior within a browser context;
- navigation/title/loading/history/error observation;
- navigation and popup URL validation;
- native popup creation denial in favor of app-level new tabs;
- explicit unsupported behavior for downloads and non-displayable responses.

Relevant platform APIs:

- [Apple `WKWebView`](https://developer.apple.com/documentation/webkit/wkwebview)
- [Apple `WKNavigationDelegate`](https://developer.apple.com/documentation/webkit/wknavigationdelegate)
- [Apple `WKWebsiteDataStore`](https://developer.apple.com/documentation/webkit/wkwebsitedatastore)
- [Wry documentation](https://docs.rs/wry/latest/wry/)
- [Wry source repository](https://github.com/tauri-apps/wry)

### Linux

`crates/ui/src/browser/linux/mod.rs` and `helper.c` run WebKitGTK in an isolated helper process. The helper sends page state and RGBA frames over a framed pipe; GPUI owns the visible surface and input routing.

This gives stronger process separation and works with the existing GPUI compositor, but the boundary currently carries rendered frames and browser commands rather than a browser object that an external automation runtime can directly target. WebKitGTK also does not provide the same Chromium/CDP compatibility as Orca's Electron backend.

Relevant platform APIs:

- [WebKitGTK `WebKitWebContext`](https://webkitgtk.org/reference/webkit2gtk/stable/class.WebContext.html)
- [WebKitGTK security manager](https://webkitgtk.org/reference/webkit2gtk/2.26.1/WebKitSecurityManager.html)
- [WebKitGTK sandbox configuration](https://webkitgtk.org/reference/webkit2gtk/2.34.4/WebKitWebContext.html#webkit-web-context-set-sandbox-enabled)

### Windows

The Windows backend is implemented in `crates/ui/src/browser/windows.rs` using WebView2 through Wry. The remaining Windows-specific work is runtime validation with the WebView2 Runtime, child-HWND overlay/clipping behavior, screenshot capture, and richer error/download/permission policy.

The natural Windows engine is Microsoft Edge WebView2:

- [WebView2 introduction](https://learn.microsoft.com/en-us/microsoft-edge/webview2/)
- [WebView2 API overview](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis)
- [WebView2 Win32 getting started](https://learn.microsoft.com/en-us/microsoft-edge/webview2/get-started/win32)
- [WebView2 API reference](https://learn.microsoft.com/en-us/microsoft-edge/webview2/webview2-api-reference)

WebView2 exposes native hosting, navigation/history, downloads, permissions, new-window handling, profiles/user data, composition hosting, input forwarding, and Chrome DevTools Protocol support. That maps well to the missing Windows backend, though its native integration will need a deliberate GPUI/window-handle seam rather than assuming the macOS AppKit implementation can be copied.

## Backend options

| Option | Advantages | Costs / risks | Fit |
| --- | --- | --- | --- |
| **Windows WebView2 + existing macOS/Linux backends** | Native on each OS, smallest change, uses existing browser model and GPUI chrome, Windows Chromium engine, WebView2 supports composition and CDP | Different engine behavior across OSes; Windows COM/threading/window integration; runtime distribution | **Best next step for embedded UI** |
| **Wry on all supported platforms** | One Rust-facing API and existing macOS dependency; WebView2 on Windows is possible through the platform webview stack | Would not remove the existing Linux helper/compositing problem; feature coverage and native ownership still need platform code; automation remains heterogeneous | Worth evaluating for a thin Windows adapter, not a full Orca solution |
| **CEF / Chromium sidecar** | Chromium consistency, DevTools/CDP, stronger path toward Orca-style automation and browser compatibility | Large binaries and packaging work, subprocess lifecycle, GPU/compositor/input integration, memory/startup cost, licensing/updates, new backend on every OS | Later milestone only if Chromium parity or shared automation is a hard requirement |
| **Electron migration** | Similar engine model to Orca and direct access to Electron APIs | Conflicts with the Rust/GPUI architecture, adds a second application runtime, larger distribution and security surface, high migration cost | Not justified for adding a browser |
| **External browser only** | No embedding or security/packaging cost | Does not satisfy the product goal | Fallback only |

The main decision should be driven by whether the product needs **Chromium/CDP parity**, not by the word “embedded.” The existing system-webview design is already the lower-cost answer for a native browser pane.

## Gaps between the current browser and Orca

### 1. Stable page identity and lifecycle

Current browser state is coupled to `BrowserSurface` entities. Add a stable `BrowserPageId` and a registry owned by the shell/window or a browser controller. Every native event and automation request should carry:

- page ID;
- owning profile/worktree/session scope;
- page generation or incarnation;
- backend kind;
- current native handle/route.

When a page closes or is recreated, old callbacks must be rejected by generation checks. This prevents stale navigation, favicon, frame, or automation results from updating a replacement tab.

### 2. One authoritative page for UI and automation

The visible tab, input routing, state updates, screenshots, and automation must resolve through the same page registry entry. Avoid a separate Playwright/Chromium page that merely happens to use the same URL; it will diverge in cookies, DOM state, navigation, and popups.

For WebView2, its page can be the authoritative object and CDP can attach to that instance. For WebKitGTK/WKWebView, automation would need a platform-specific bridge or a separate Chromium automation backend; CDP is not automatically portable across those engines.

### 3. Profiles and website data

`BrowserContext` currently uses ephemeral data on macOS/Linux. Orca exposes explicit browser profiles and worktree-scoped state. Decide and document:

- ephemeral versus persistent storage;
- profile identity and ownership;
- whether tabs in one session share cookies/local storage;
- whether profiles are per device, worktree, account, or session;
- deletion and migration behavior;
- whether credentials may ever cross local/synced workspace boundaries.

A first release can remain ephemeral, but the profile boundary should exist in the model before persistence is added.

### 4. Popups and new windows

The current HTTP(S) popup-to-new-tab behavior is a good baseline. Orca distinguishes ordinary unnamed popups from named/OAuth-style popups and can open some in separate windows. The next design should model popup intent explicitly rather than reducing every request to only a URL:

- opener page ID;
- requested URL and target name;
- user gesture;
- requested features/window dimensions;
- same-profile versus separate-profile policy;
- tab, managed window, or external-browser result.

### 5. Downloads, permissions, and certificates

Current code intentionally rejects or leaves several browser features unsupported. Orca-like parity requires host policy for:

- downloads and destination selection;
- geolocation, camera, microphone, notifications, clipboard, and other permissions;
- certificate errors and trust exceptions;
- external protocol links;
- fullscreen and print requests.

These must be explicit policy decisions, not accidental engine defaults.

### 6. Remote workspace network identity

The current research and implementation treat the browser as running on the UI device. In a remote session, `localhost` therefore means the UI device. Orca supports a stronger model where the page may render locally while HTTP(S), WebSocket, DNS, loopback, uploads, and downloads use the remote host.

Adding this requires a separate authenticated transport/proxy design. Existing device-room RPC is not an HTTP/WebSocket tunnel. The browser model should therefore include a network location such as `LocalDevice` or `WorkspaceHost`, but implementation should not be hidden behind a misleading localhost rewrite.

### 7. Automation

Orca's useful feature is that agent commands act on the same browser tabs the user sees. A staged approach is:

1. Define an internal `BrowserController` interface around page IDs, navigation, snapshot, click, fill, key input, and screenshot.
2. Implement it for the Windows WebView2 page first, using the page's CDP or native APIs.
3. Extend the Linux helper protocol with page-targeted automation commands if WebKitGTK remains the backend.
4. Add macOS automation only after selecting a supported WebKit automation mechanism; otherwise use a Chromium backend for automation-required pages.
5. Reject commands for closed or stale page generations.

Do not add an independent headless browser just for convenience unless it is explicitly a separate test/automation product.

## Recommended implementation plan

### Milestone 0: decide the target

Document whether the immediate goal is:

- **M0:** a browser pane on Windows;
- **M1:** complete native browser UX across macOS/Linux/Windows;
- **M2:** same-page agent automation;
- **M3:** persistent profiles and remote workspace traffic;
- **M4:** Chromium parity across all platforms.

The first three are compatible with the current architecture. M4 likely requires evaluating CEF or another Chromium-centered design.

### Milestone 1: Windows embedded page

Add a target-gated Windows backend with a small interface matching the existing native responsibilities:

- create/destroy page;
- navigate/reload/back/forward;
- set bounds and visibility;
- focus and input forwarding;
- receive URL/title/loading/history/error events;
- route new windows through the shell;
- explicit navigation, download, permission, and external-link policy.

Use WebView2 with a profile/user-data directory controlled by `BrowserContext`. Start with ephemeral storage to match current macOS/Linux semantics, while keeping the profile object explicit.

The main technical spike is not navigation—it is native composition and input ownership between GPUI and a WebView2 controller. Validate child-window hosting and WebView2 visual/composition hosting separately on Windows.

### Milestone 2: browser registry and stale-event safety

Refactor the shared layer so a browser tab is not identified only by a GPUI entity:

```text
BrowserPageId
  -> BrowserPageRecord
       scope/profile
       generation
       PageState
       backend handle
       automation capabilities
```

Move native event routing through this record. Add tests for close/reopen, rapid navigation, background tabs, popup creation, and late callbacks.

### Milestone 3: browser policy surface

Add host-owned handlers for downloads, permissions, certificates, popups, external URLs, and profile lifecycle. Keep defaults restrictive. Preserve the current HTTP(S)-only policy unless a specific feature requires another scheme.

### Milestone 4: same-page automation

Expose a browser controller to the agent/CLI layer. Every command must specify a `BrowserPageId`; the controller verifies page scope and generation before acting. Add snapshot, click, fill, key, navigate, and screenshot first. Add DevTools only if the selected backend provides a supported, secure attach path.

### Milestone 5: remote browser routing

Design an authenticated browser transport for remote workspaces. Specify where rendering, DNS, HTTP/WebSocket traffic, uploads, downloads, and credentials live. Treat this as a protocol feature, not a UI-only browser enhancement.

## Security requirements

Any implementation that displays arbitrary web content inside the app should treat the browser as an isolation boundary. Orca's Electron implementation follows the same general principles documented by Electron:

- [Electron security checklist](https://www.electronjs.org/docs/latest/tutorial/security)
- [Electron `WebContentsView`](https://www.electronjs.org/docs/latest/api/web-contents-view)
- [Electron `webContents`](https://www.electronjs.org/docs/latest/api/web-contents)

Required invariants for this project:

- never grant application/Node privileges to remote page content;
- keep navigation restricted to approved schemes and validate parsed URLs;
- require deliberate policy for popups and new windows;
- deny permissions by default until a user-facing flow exists;
- keep profiles and website data scoped and deletable;
- avoid passing secrets through page-to-engine bridges;
- reject stale page IDs and destroyed native handles;
- keep password/editor-sensitive data within the browser process boundary where possible;
- make external URL opening explicit and validate the scheme before handing it to the OS;
- keep the browser engine current and include its runtime/third-party notices in releases.

## Conclusion

Yes, `tri-zorin` can add an embedded browser comparable to Orca, but the work is not a single dependency addition.

- For **basic embedded-browser parity**, add a Windows WebView2 backend and retain the existing macOS/Linux implementations.
- For **Orca-like product behavior**, first introduce stable browser page identity, a central registry, explicit profiles, and host-owned policy. Then add same-page automation and remote network routing.
- For **identical Chromium behavior and CDP everywhere**, evaluate a Chromium backend such as CEF only after measuring the cost of startup time, memory, packaging, compositor integration, and lifecycle teardown. Electron should not be introduced just to embed one browser pane.

The highest-value next engineering task is a Windows WebView2 composition spike plus a design-level `BrowserPageId`/registry refactor. Those two decisions establish whether the current native-webview architecture can grow into the desired Orca-like browser without a rewrite.
