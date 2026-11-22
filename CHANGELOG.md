# Changelog

## Unreleased

- Remove page zoom entirely. `Ctrl` `+` / `-` / `0` no longer do anything, WebView2 zoom hotkeys are off, and the window always renders at 100%; `window.json` no longer stores a zoom value (an old file is still read, its `zoom` key ignored).
- Ship the WebView2 bootstrapper inside the installer (`webviewInstallMode: embedBootstrapper`). The default download used `NSISdl`, which speaks plain HTTP only, so on a machine without WebView2 the installer could only fail; the template now refuses to build that mode at all. A bootstrapper that still fails points at the manual download page instead of telling the user to restart the installer.
- Upgrading no longer loses the opt-in login-start entry. The reinstall page defaults to installing in place, as README already promised, and the "uninstall first" choice now runs the old uninstaller with `/UPDATE` so its pre-uninstall hook keeps the `Run` value (and the shortcuts and app data that an in-place upgrade keeps).
- The startup-failure dialog appears only when no window is showing the reason (a login launch), and no longer runs on the thread that consumes the tray's Restart and Quit: a modal box there made both wait until it was dismissed.
- Port probing during startup reads the listening-socket table instead of binding the port, so it cannot make the backend it is waiting for fail with `EADDRINUSE`.
- Window geometry is written by one serialised path: the debounce tick and the quit flush no longer share `window.json.tmp`, and `flush` waits for a write in flight instead of returning while it is still landing.
- An unexpected backend exit is now checked and recorded under the same lock the restart path takes, so a stale exit cannot clear a freshly started instance; the liveness probe re-checks its generation before reporting.
- A startup failure clears the card's `aria-busy` and puts the reason in a live region, so a screen reader reads the failure instead of staying in a "loading" state that never ends.

## 0.1.3 — Plain startup screen, no embedded font

- Restore the startup screen that 0.1.2 replaced: the card, the spinner, the indeterminate progress bar and the three-step list are back, and Rust fills the status line, the loopback address and the current step again through the same `eval` phases (no IPC permission). A startup failure stops the spinner, the bar and the step list instead of leaving them implying progress.
- Drop the Ciallo animation and the embedded serif subset with it: `ui/fonts/`, `scripts/build-startup-font.py` and the Vite licence block are gone, the page ships no webfont, and `app.security.csp` no longer declares `font-src`. Font attribution is removed from README, `THIRD_PARTY_NOTICES.md`, the maintenance notes and the generated frontend licence bundle.
- The context menu and middle-click suppression that came with the animation go with it; the window behaves like the 0.1.1 startup card again.
- Edge specs cover the restored card, the phase payload and the failure layout.

## 0.1.2 — Window memory, silent login start, recoverable backend

**升级方式**：从 0.1.x（Tauri 版）直接覆盖安装，不需要卸载；只有旧 Electron 客户端仍需先手动卸载。

- Remember the main window's position, logical size, maximized state and zoom in `window.json`; restore by clamping into a current work area so an unplugged or rearranged display cannot leave the window off-screen.
- Register login startup as `"<exe>" --tray` so a login launch goes to the tray without opening a window. The argument-less form written by 0.1.x is still recognised and removable, and the NSIS pre-uninstall hook deletes either spelling of this installation's entry only.
- Keep the launcher running when the backend exits unexpectedly or fails to start: the main window returns to the bundled page with the exit code and log path, the tray reports 已停止, and a new 重启本地服务 action stops only the launcher-owned instance before starting a fresh one (so a configuration change no longer needs a full restart).
- Probe the backend every 15 s; two consecutive misses show 无响应 in the tray instead of a green badge on a hung process.
- Trim the tray menu to one header line plus the actions: drop the status subtitle and the address/version row. The badge alone reports 启动中/运行中/无响应/已停止, with uptime and the reason in its tooltip; the address stays reachable through 复制 Web 地址.
- Replace the startup card with the Ciallo animation: full-window scrolling boards and a bouncing title, and nothing that reacts to a click. The context menu and middle-click autoscroll are suppressed, since the window is an application rather than a page. Silent by design, no text selection, and no status line — the animation is the only progress signal. Startup failures still land on the same screen, with the reason and log path kept selectable.
- Embed a 5 KB subset of Source Han Serif SC (思源宋体, Adobe, SIL OFL 1.1) for that title, so a machine without a Song face no longer falls back to SimSun. A subset is a Modified Version under the OFL, so the family is renamed `Ciallo Serif` and the copyright/trademark statements inside the font are kept verbatim; the licence ships as `ui/fonts/OFL.txt` and inside the bundled third-party notices, and the subset stays under the OFL rather than this project's MIT. Attribution is recorded in README and `THIRD_PARTY_NOTICES.md`.
- Paint the window in the startup page's paper colour in every Windows theme, so the first frame matches the page instead of flashing dark-then-pink. The loading page no longer reports the loopback address or a three-step progress list.
- Document the remembered window file, the silent login launch, restart and recovery behaviour; extend the Rust, component and Edge layout tests accordingly.
- Replace the handwritten tray controls with Vue 3, PrimeVue ToggleSwitch/Button/Divider and locally bundled Aura themes using Vite.
- Measure intrinsic content and resize the native tray window in bounded logical pixels before its first display; respect monitor work areas, fit normal menus without scrollbars, and keep a scrolling fallback for exceptionally small screens or enlarged fonts.
- Replay native state after Vue mounts. Keep verified startup state and click suppression, including Edge's canceled-checkbox default-action rollback race.
- Reserve identical button borders across normal/hover/active states so pointer movement cannot add 2px to the menu or cause native height jumps.
- Add real Vue component tests and isolated Edge layout/interaction tests for light/dark themes, fractional DPI, enlarged fonts and small work areas. Include frontend compilation before Rust checks in CI.
- Bundle complete locked frontend dependency license notices. Generated UI, screenshots and browser test results remain ignored.

## 0.1.1 — Tray and documentation polish

- Remove zoom controls from the tray menu; retain main-window keyboard zoom.
- Show login startup as a right-aligned switch without a visible explanatory footnote. Startup remains opt-in and disabled by default; preserve a user's previously enabled registration and report failures through the tooltip/accessibility status.
- Reduce the tray window height and fit the card to its contents instead of leaving empty space below Quit.
- Correct README configuration examples for custom data directories and existing settings; clarify safe Electron migration and backend build recovery.
- Update official GitHub Actions to Node 24-runtime releases while retaining Node 22 for project checks/builds. Keep manual packaging and opt-in publication, deriving installer/tag versions from synchronized application metadata.

These are source changes; Windows tray appearance and installer acceptance still require desktop verification.

## 0.1.0 — Tauri / WebView2 migration

### 必须手动卸载旧 Electron 客户端 / REQUIRED migration from the old Electron client

**先退出所有旧 Pi Launcher（包括托盘），手动卸载旧安装器安装的客户端，再安装新版。保留启动器配置和 Pi 凭据、会话；不自动迁移 Electron 浏览器数据。**

**Exit ALL old Pi Launcher instances, including the tray. Manually uninstall the OLD installer/client, THEN install the new Tauri version. There is NO automatic upgrade from the old installer. Never run both clients together.**

**Preserve `%APPDATA%\Pi Launcher` configuration (especially `launcher-config.json`) and `.pi` credentials/sessions, including custom data directories. Do not delete these directories during uninstall.**
Electron browser local storage/profile is **not promised to migrate** to WebView2. Browser-only preferences or sign-in state may need to be set again; preserving Pi credentials/sessions is not browser-profile migration.

### Changes

- Replace the Electron desktop runtime and assets with Tauri 2, Rust and the Windows WebView2 runtime. Keep Pi Launcher identity, `local.pi.launcher`, JSON configuration and the external Pi Web backend model.
- Use native Windows process/ownership handling, a loading page and a custom tray popover; keep backend installation and updates explicit and separate from startup.
- Add the tray “开机自启” switch for current-user Windows logon startup. Registration changes only on explicit clicks; show errors and remove only this installation's matching startup entry on uninstall.
- Remove obsolete Electron packaging dependencies, smoke/integration/runtime tests and scripts. Do not claim their historical results as Tauri evidence.
- Retain the optional explicit fork backend builder, manifests, Windows ESM build-test adapters and portable artifact-delivery regression tests without retaining the Electron runtime.
- Separate portable Node release/backend tooling tests from native Windows Rust fmt/test/clippy checks.
- Add read-only version synchronization checks for npm package/lock, Cargo package/lock and explicit Tauri configuration version.
- Keep manual-only fixed-SHA Windows NSIS builds, stable ASCII installer names and SHA-256 checksums. Publish only through a separately opted-in write-permission job; refuse existing tags/releases and preserve failures for manual inspection.
- Retain MIT and third-party notices in packaged license resources. Node.js >=22.19 and the Pi Web backend remain external prerequisites; source builds additionally need Rust >=1.90/MSVC.

This changelog describes source changes, not completed Windows GUI, installation, uninstallation or release acceptance.

## 0.0.2 — Electron

- Rename the product and repository to Pi Launcher / pi-launcher, with application ID `local.pi.launcher`, data directory `Pi Launcher` and environment prefix `PI_LAUNCHER_`.
- Add a tray action to open the active configuration directory, including custom data directories.
- Keep existing user data untouched; no automatic migration or legacy environment-variable fallback.

## 0.0.1 — initial public snapshot

- Prepare Pi Launcher as a standalone MIT project and start public versioning at 0.0.1.
- Move machine-specific runtime options into user JSON with safe defaults.
- Separate self-contained units from optional Windows/backend integration tests.
- Attribute the unmodified Pi project logo in THIRD_PARTY_NOTICES.md.
- Document explicit backend builds, configuration and release evidence boundaries.
- Offer manual Windows x64 builds and opt-in GitHub Release publishing with checksums and no existing-version overwrite.
