# Contributing to Delta-V

You do not need to write code to help. A reproducible bug report, a confusing instruction, or a screenshot of a layout problem is useful too.

For new features, dependencies, or changes to the app's structure, open an issue before starting. Small fixes and documentation corrections can go straight to a pull request. The first release is focused on macOS and Claude and Codex subscription usage; see the [roadmap](README.md#roadmap) for what is planned.

## Reporting a problem

Check existing issues first. If the problem has not been reported, open an issue with:

- Your macOS version, whether your Mac uses Apple silicon or Intel, and the Delta-V version or source revision.
- Which provider is affected and, for sign-in problems, the Claude Code or Codex CLI version.
- The steps that reproduce it, what you expected, and what happened instead.
- The error text or a screenshot, if it helps. For display problems, mention whether you chose Remaining or Used.

Remove personal details from screenshots. Do not post access tokens, credential files, Keychain exports, request headers, or raw session logs.

## Running the project

Fork the repository to your GitHub account, clone your fork to your Mac, and create a branch for your change. Open Terminal in the folder containing `package.json`.

You need Node.js 22.12 or newer, Rust, and the Xcode command line tools. The [README](README.md#install) links to their installation instructions. Delta-V uses Tauri v2 and Rust for the native app, with React and TypeScript for the usage panel.

Quit any other running copy of Delta-V first. Install the JavaScript dependencies, then start the app:

```sh
npm ci
npm run tauri -- dev
```

The development app appears in the menu bar. It uses your normal Delta-V settings and saved CLI sign-ins, and contacts the providers' usage services. If you have enabled history, it also uses your normal history folder. Pause recording before a development session if you do not want to add readings. Follow [Connect your accounts](README.md#connect-your-accounts) if needed. An Apple Developer account is not required to run it locally.

For interface work without an account, run this instead:

```sh
npm run dev
```

Open `http://127.0.0.1:1420` in your browser. This preview uses sample data and keeps settings in memory. It does not read credentials or fetch usage. Native tray behaviour, Keychain access, and panel positioning still need to be checked in the macOS app.

For connection help, use `?issue=credential_access&provider=both` or `?issue=configuration&provider=both`. Add `&view=settings&section=accounts` to check the same errors in Settings. Check the help links with Tab and Enter in both appearances and provider layouts. Browser preview uses ordinary links; check native browser opening, failure feedback and retry separately in the macOS app. Do not change real credentials to create an error state.

For sign-in focus, use `?issue=sign_in&provider=both` and `?issue=authentication&provider=both`, including in Settings with `&view=settings&section=accounts`. Open each provider's confirmation with Enter or Space: Cancel receives focus, and Cancel or Escape returns it to the same sign-in button without closing the surrounding view. Continue moves focus to that provider's connection status while the existing sign-in action starts. Check healthy **Sign in again** in Accounts too. Ordinary updates must not move focus. Preview sign-in actions are simulated; verify native focus separately without changing accounts.

For history work, open `http://127.0.0.1:1420/?history=ready`, then click **History**. Other `history` values are `empty`, `paused`, `waiting`, `sparse`, `error`, and `accounts`. These use synthetic readings, including gaps and an earlier account. Recording, retention, and clear controls affect only the in-memory preview; reloading the page restores the selected example.

For update checks, open **Settings → App → Updates** in the browser preview. The default starts unchecked and **Check for updates** produces a sample result without contacting GitHub. Use `?updates=available` for a newer release; other values are `unchecked`, `current`, `ahead`, `network`, `rate_limited`, `invalid_response`, `service`, and `stale`. The `stale` example shows a failed check with an earlier successful result. Set `updates_result` to choose the result of a button click, for example `?updates=current&updates_result=network`. Preview results stay in memory. Check native browser opening and actual GitHub responses separately in the macOS app.

## Making changes

Keep each pull request focused on one problem. A few conventions matter here:

- Keep credentials, polling, and parsing in Rust. Keep the frontend concerned with display and interaction, and macOS calls in the `platform` module.
- Preserve where each number came from. Label estimates, leave missing readings unavailable, and keep API billing separate from subscription allowances.
- Use typed Rust errors. Avoid `unwrap()` and `expect()` outside tests and startup code. TypeScript stays strict, without `any` or `as unknown as`.
- Add a regression test when fixing parsing, reset calculations, or percentage and threshold logic. React render snapshots are not required.
- Write comments that explain why a choice was made. Keep user-facing text plain, without emoji or em dashes.

Parser fixtures live in `src-tauri/tests/fixtures/`. If you change a parser, review the input fixture and expected output together. Do not replace expected output just to make a test pass. Follow the [fixture notes](src-tauri/tests/fixtures/README.md) when adding a response: remove credentials and identifying fields while preserving the structure needed to reproduce the problem.

### Usage polling

Routine Claude requests must start at least 300 seconds apart within an app run. Enforce this in the shared scheduling gate so Check now, unlock, provider selection, connection changes, and quota resets cannot bypass it. Longer service cooldowns still apply. Keep Codex's timing and explicit sign-in recovery unchanged. Freshness and notification checks must use the effective cadence for each provider. Use synthetic clocks and responses to test these rules without contacting a usage service.

### History calculations

History queries read saved files only. Keep each provider, account, quota ID, window length, and provenance separate. A daily summary is the highest usage recorded that day, which is also the lowest recorded remaining allowance. It does not measure tokens spent or work completed. Days follow the Mac's current time zone, captured once per query, including daylight saving changes. The 7-day and 30-day periods include today.

Today and single-day queries return individual readings. Longer periods return daily summaries, with empty dates where no readings were saved. Intraday points mark breaks when readings are more than 30 minutes apart, a reset changes or is crossed, usage decreases, or the selected quota disappears from an intervening snapshot. Those markers belong to the original readings, not to connections between daily peaks. Never fill gaps with zeroes or treat a falling percentage as proof of a reset. Observations report recorded extrema, days with readings, and days with a reading below the saved low allowance threshold.

Provider reset timestamps can wobble by one second between readings. Compare them with the first reset timestamp in each continuous segment so that this jitter does not split the line, but cumulative drift still does. Keep the original timestamps in stored readings and CSV exports.

CSV exports use the same account and quota filters but retain original readings for every range. UTC timestamps use RFC 3339, fractions keep their stored precision, and `break_before` contains semicolon-separated reasons. Cells are quoted and potentially executable spreadsheet text is escaped. Exports stop at 100,000 readings per provider or 32 MiB rather than silently truncating data. Date and time-zone changes during the Save dialog require a fresh selection.

PNG exports reuse the chart geometry, with fixed dimensions and no account keys. They include the selected windows, dates, units, provenance and coverage, plus a small Delta-V mark. Check both appearance modes and sparse history. Native Save, overwrite confirmation, Cancel, Copy image and Show history folder need a macOS check, including while pinned. The browser preview downloads synthetic files with a `Sample-` filename prefix and does not access the clipboard or Finder.

### Notifications

`src-tauri/src/notifications.rs` contains the in-memory low-allowance evaluator. The runtime feeds it accepted successful responses, an independently verified opaque account key, and the current polling cadence's freshness allowance. The evaluator does not read credentials, request identity, write files, or depend on history recording.

The first eligible reading is a silent baseline, even if already low. A later reading must cross from at or above the threshold to below it. Compare remaining allowance at the same precision as highlighting, not rounded display percentages. Evaluate each enabled official quota independently; exclude expired, reset-less, invalid, positional, and ambiguous duplicate limits. Errors must not replay cached readings as new observations.

Keep duplicate prevention per provider, account and quota in memory. A period can produce one crossing candidate. Reset timestamp corrections do not rearm it before the recorded boundary has elapsed, and a new period still needs a fresh future reset and a silent first reading. Missing or invalid limits preserve prior valid state. Account changes, lost identity, recovery and notification policy changes require a fresh baseline while preserving already-emitted periods. Restarting the app starts new in-memory state; there is no guarantee of deduplication across restarts. A crossing candidate does not confirm delivery.

`notification_delivery.rs` checks macOS permission without prompting, evaluates under the runtime lock, then delivers outside that lock. Tickets reject obsolete readings after policy, connection or recovery changes. Notifications start off and monitor both connected providers when enabled, including a provider hidden by the panel picker. Claude's verified account lookup works independently of history recording; enabling notifications does not enable history writes.

`platform/notifications.rs` uses UserNotifications for local alerts without sound or badges. Only an explicit enable command from the Settings window can request permission. Permission reads and automatic checks never prompt. A delivery failure is reported and is not retried with an old reading. Clicking an alert shows usage without discarding a Settings draft. The notification includes the provider, quota labels and remaining threshold, never account identifiers or credentials.

**Settings → Notifications** supports permission status, enabling/disabling, a test notification and opening macOS notification settings. The saved **Display & usage → Low allowance threshold** controls highlighting and enabled alerts; an unsaved draft affects neither. It always refers to remaining allowance, regardless of the percentage display mode, and uses the existing `threshold` configuration key. Notification choices apply immediately and must survive Save, Discard, section changes and closing Settings. Test notifications require an enabled preference and existing alert permission. macOS accepting a request does not prove a banner appeared; Focus and system presentation settings can suppress it.

In the browser preview, use `?view=settings&section=notifications&notifications=enabled`. Other `notifications` fixtures are `not_determined`, `off`, `denied`, `alerts_disabled`, `unavailable`, `read_error`, and `delivery_error`. They simulate notification states without requesting permission or sending an alert. Use `notifications_enable_result`, `notifications_disable_result`, `notifications_test_result`, `notifications_open_result`, or `notifications_read_result` to exercise an action's result. Reloading restores the fixture.

Ordinary `tauri dev` executables report notifications as unavailable because they lack an app bundle identity. Native smoke tests need a built `Delta-V.app`. Check first enable, denied permission, permission changes in System Settings, test delivery, foreground presentation and clicking an alert while Settings has unsaved changes. The System Settings link is best effort; the manual route is **System Settings > Notifications > Delta-V**. Automated tests use synthetic readings and permission values, without requesting permission or sending real notifications.

Build a local debug app for these checks:

```sh
APPLE_SIGNING_IDENTITY=- npm run tauri -- build --debug --bundles app -- --locked
```

Quit any running copy, then open `src-tauri/target/debug/bundle/macos/Delta-V.app`. This ad-hoc signed app uses your normal Delta-V settings and saved CLI sign-ins. It is not notarized.

## Checking your work

For code changes, run these from the project root:

```sh
npm run build
npm test
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
```

Normal tests use local fixtures and synthetic inputs. They do not sign in, renew tokens, or need a provider account. For documentation-only changes, check the wording, links, and Markdown instead.

For interface changes, check light and dark appearance, both the single-provider and side-by-side layouts, and attach screenshots to the pull request. If you change panel positioning or focus, also check opening below the icon, closing on an outside click, and placement on a second monitor. Mention anything you could not check.

The two live provider tests are optional. They read your saved CLI sign-ins and make real usage requests, so leave them alone during a provider cooldown:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked live_ -- --ignored --test-threads=1
```

## Building a disk image

To check the installer locally, run:

```sh
APPLE_SIGNING_IDENTITY=- npm run tauri -- build -- --locked
```

This builds for your Mac's architecture and produces both `Delta-V.app` and a `.dmg` in `src-tauri/target/release/bundle/`, under `macos/` and `dmg/` respectively. Open the disk image in Finder to check the app icon and Applications shortcut.

This local build uses an ad-hoc signature and is not notarized. Public downloads need Developer ID signing and Apple notarization before release.

## Publishing a release

For maintainers, the release order is:

1. Run the checks above and choose a version that matches the changes being released. Confirm that `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` use the same version. Match the top-level and root-package versions in `package-lock.json` and the `delta-v` package version in `src-tauri/Cargo.lock` too; dependency and test-fixture versions are separate. Prepare any README changes and draft the release description for GitHub, then commit the changes intended for release. Keep release notes on GitHub, not in a repository file.
2. Build for the intended architecture. The first download targets Apple silicon (`aarch64-apple-darwin`). Sign the app and DMG with a **Developer ID Application** certificate, then submit the DMG to Apple for notarization. Follow the [Tauri signing guide](https://v2.tauri.app/distribute/sign/macos/) for signing setup. Keep signing credentials outside the repository.
3. Check Apple's notarization log, staple the accepted ticket to the DMG, and verify the signatures and Gatekeeper acceptance of both the DMG and its enclosed app. Generate the `.dmg.sha256` checksum after stapling, since stapling changes the file.
4. Install from that DMG and check normal launch, account connections, saved settings, and launch at login after logging out and back in. Check replacing an older app too. Verify Settings returns to the panel and protects unsaved changes, then repeat the native notification checks above with the release app. A separate Mac or macOS account helps test a fresh setup. If application code changes, rebuild and repeat signing and verification.
5. Push the release changes and create a draft GitHub release with a matching tag, such as `v0.1.0`. The tag must contain the application source used for the build. Attach the DMG and checksum under **Attach binaries**, then add release notes with installation steps, supported Macs, and known limitations. Keep build files out of source commits.
6. Download the attached DMG and checksum through a browser, verify the checksum, and test installation from that downloaded copy. Publish the draft after these checks pass, then mark the downloadable release complete in the README roadmap.

## Sending a pull request

1. Review your changes and remove unrelated edits or generated build files.
2. Commit the change with a short, descriptive message, such as `Handle expired usage windows`.
3. Push your branch to your fork and open a pull request against Delta-V's `main` branch.
4. Explain the problem, what changed, and how you checked it. Link the issue if there is one.

A draft pull request is fine if you want feedback before finishing. Keep follow-up changes in the same pull request so the discussion stays together.
