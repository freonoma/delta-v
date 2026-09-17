# Delta-V

See how much Claude Code and Codex usage you have left, from your Mac's menu bar. View either provider or both side by side, and choose which usage window appears beside `ΔV`.

The name comes from spaceflight: a finite budget until the next refill.

<img src="screenshots/main.png" alt="Delta-V showing Claude and Codex usage side by side below the menu bar" width="560">

## Install

Delta-V is still in development. There is no downloadable release or Homebrew package yet. For now, build the app from source on a Mac running macOS 13.3 or newer.

You need [Node.js](https://nodejs.org/en/download) 22.12 or newer, [Rust](https://rust-lang.org/tools/install/), and the [Xcode command line tools](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools) installed.

1. Download the source from this repository using **Code → Download ZIP**, then unzip it.
2. Open **Terminal**. Type `cd `, including the space, then drag the extracted folder into Terminal and press Return. Use the folder containing `package.json`.
3. Run these commands, one at a time:

```sh
npm ci
APPLE_SIGNING_IDENTITY=- npm run tauri -- build -- --locked
open src-tauri/target/release/bundle/macos
```

The first two commands build `Delta-V.app` for your Mac. The last command opens its folder in Finder. Quit any running copy of Delta-V, drag `Delta-V.app` into **Applications**, then double-click it there.

Look for **ΔV in the menu bar at the top of your screen**, near the clock. It does not open a regular window or appear in the Dock. You can close Terminal once it is running. To quit, click ΔV, then **Quit**. Open it again from Applications.

On a fresh install, the first time you click ΔV, it asks whether to launch at login. Choose **Enable** or **Not now**. You can change this later under **Settings → Startup**.

The `APPLE_SIGNING_IDENTITY=-` setting gives this local build an ad-hoc signature, which does not require an Apple Developer account. A Developer ID-signed and notarized download is still planned. To update a source build, rebuild it, quit the installed copy, and replace it in Applications.

## Connect your accounts

You can use Claude, Codex, or both. You only need an account for the provider you want to see.

Delta-V uses the sign-in saved by each provider's terminal app, also called a CLI. If you already use those tools on this Mac, your usage should appear when you open Delta-V.

| Provider | Account needed | Setup |
| --- | --- | --- |
| Claude | A Claude account with a subscription that includes Claude Code | Install [Claude Code](https://code.claude.com/docs/en/quickstart) 2.1.270 or newer. In Delta-V, choose **Sign in with Claude Code** and finish in your browser. |
| Codex | A ChatGPT account with access to Codex | Use the client bundled with the ChatGPT or Codex Mac app, or install [Codex CLI](https://learn.chatgpt.com/docs/codex/cli) 0.154 or newer. In Delta-V, choose **Sign in with Codex** and finish in your browser. |

Click ΔV and select **Claude**, **Codex**, or **Both**. If you need to connect, the sign-in button starts the official client. You never paste a password or token into Delta-V. macOS may ask for Keychain access to read the saved sign-in.

If the required app is missing or needs an update, choose **Claude Code setup** or **Codex setup** in the provider's card. This opens the provider's instructions in your browser. After installing or updating, return to Delta-V and choose **Sign in with Claude Code** or **Sign in with Codex**. You do not need to restart Delta-V. These controls are also available under **Settings → Accounts**, even for a provider hidden by the picker.

If you finish signing in directly in the terminal app, choose **Check now** in Delta-V to read the saved sign-in. No coding task or model prompt is needed.

Signing in on claude.ai or chatgpt.com alone does not connect Delta-V. Claude Desktop's Code tab also has a separate sign-in path, so Claude Code still needs to be installed. API-key, Claude Console, and third-party cloud-provider billing are not supported.

You can also sign in yourself in Terminal: run `claude auth login` for Claude, or `codex login` for Codex, then choose **Check now** in Delta-V. See the [Claude](https://code.claude.com/docs/en/authentication) and [Codex](https://learn.chatgpt.com/docs/auth) sign-in guides for account requirements.

**Settings → Accounts** has controls for each provider:

- **Disconnect from Delta-V** stops usage checks and clears its reading. It stays disconnected after restarting Delta-V. Claude Code or Codex remains signed in.
- **Connect** resumes checks using the CLI's saved sign-in.
- **Sign in again** opens the official sign-in flow, including for changing accounts. This also changes the account saved for Claude Code or Codex. Delta-V explains this before you continue.

Account actions take effect immediately, without **Save settings**. Cancelling the settings form does not undo them. A usage request already in progress may finish after disconnecting, but its reading is discarded.

## Using it

Click ΔV to open the usage panel. The **Claude / Codex / Both** picker chooses which providers you see. **Show more** reveals additional windows, credits, and provider details. Clicking outside closes the panel; the next opening starts compact again.

Under **Settings → Compact view**, choose a first and optional second window for each provider. For example, show Claude's five-hour window alongside its weekly model limit, and choose different windows for Codex. **Automatic** lets Delta-V choose. These settings control the compact rows. Show more always reveals all reported limits the app understands.

Percentages show **remaining** usage by default. A window at 55% used has 45% remaining. Under **Settings → Show percentages as**, choose **Used** if you prefer. This changes the menu bar, the large provider percentages, and the usage bars together.

By default, the menu bar tracks the most-used available window among the selected providers. **Settings → Menu bar tracks** lets you choose a particular five-hour, weekly, or model-specific window instead. Each provider's large percentage shows its most-used window, unless you track a specific one from that provider.

The percentages and bar fill change colour when less than 20% remains. When showing remaining usage, the bar is empty at 0%, so the warning colour appears on the numbers. You can change the threshold in Settings. It always refers to what remains, even when you choose to display the percentage used. The menu bar icon itself follows the normal macOS colour.

**Appearance** offers Light, Dark, or System, which follows your Mac's appearance. Changes preview immediately. **Save settings** keeps them; **Cancel** or closing the panel restores the saved appearance.

**Settings → Startup → Launch at login** opens Delta-V quietly in the menu bar when you log in to your Mac. This switch takes effect immediately. If macOS needs your approval, choose **Open Login Items** and allow Delta-V there. The app reads the macOS setting again when you return, including changes you make outside Delta-V.

<img src="screenshots/settings.png" alt="Delta-V settings for per-provider windows, menu bar tracking, percentage display, threshold, refresh interval, and appearance" width="560">

Usage updates automatically. **Check now** requests the latest reading; it cannot reset or replenish your allowance.

## FAQ

**Does checking usage spend tokens or use up my allowance?**

Delta-V asks the provider for your account's usage reading. It does not submit model prompts or generate responses. Usage checks and sign-in recovery are not model calls and are not expected to consume model tokens or subscription allowance. The usage services have their own request limits, so checking too often can make them ask the app to wait.

**Why does it say Stale or ask me to wait?**

Delta-V could not get a fresh reading, so it shows the last one with a stale label. If the provider asks it to wait, a countdown shows when it can try again. It retries automatically, and Check now respects the same wait. A `?` beside a menu bar percentage means that reading is stale; `?` on its own means no usable percentage is available.

**Why hasn't the percentage changed at the reset time?**

Delta-V waits for a new reading from the provider before showing a refill. A countdown reaching zero does not confirm that the provider has reset the window yet.

**Do Claude Code and Codex need to stay open?**

No. If a saved sign-in expires, choose **Reconnect** in Delta-V. It checks for a working sign-in first, then asks the official client to renew it if needed. If that fails, **Sign in again** opens the provider's browser sign-in. You can cancel either action.

Renewal stays with the official client. For Claude, Delta-V uses its built-in `/status` command on personal Pro and Max accounts. Managed accounts and settings may require you to reconnect in Claude Code yourself. Codex uses its account API. Delta-V verifies the result with a fresh usage request before showing a reading.

**Does it start when I log in to my Mac?**

Only if you enable **Launch at login**, either from the first-run prompt or in **Settings → Startup**. Run the installed app from **Applications** or your home folder's **Applications** folder to use this option. Turning it off prevents future login launches without quitting the running app.

## Where the numbers come from

Percentages, reset times, and credit balances come from Anthropic and OpenAI's account usage services. **Official** means the number was reported by the provider. It does not mean Delta-V is affiliated with either company.

Each usage window is shown separately. Missing values stay unavailable. A credit balance without a spending limit stays a balance. Fields the app cannot interpret are listed under **Show more → Provider details** and do not affect the menu bar percentage.

The providers have not published a stable interface for third-party apps to these services, or specified how quickly new usage appears in them. Readings can lag behind your activity. Delta-V checks less often while your Mac is idle and pauses while the screen is locked.

## Privacy

Delta-V runs on your Mac. There is no Delta-V server, telemetry, analytics, or separate account. Your credentials and usage data are not sent to the maintainer. The interface uses system fonts and loads no remote assets.

Routine usage checks contact these endpoints:

- [Anthropic usage](https://api.anthropic.com/api/oauth/usage)
- [OpenAI usage](https://chatgpt.com/backend-api/wham/usage)

Each provider receives its own saved access token with the request. OpenAI also receives the selected account ID. Credentials stay in the Rust backend and are never passed to the usage panel. Delta-V does not read browser cookies or write credentials.

When you choose **Reconnect** or **Sign in**, Delta-V starts the official client in a temporary folder. That client handles renewal, browser login, and saving its own credentials. It can contact the provider's authentication, configuration, and other startup services as well as the usage endpoints above. Delta-V disables optional telemetry and integrations for these helpers and does not send a model prompt. Recovery only runs when you ask for it.

Delta-V's usage reader does not read conversations, project files, or session logs. Usage readings stay in memory and are lost when you quit. Settings are saved locally.

<details>
<summary>Files and Keychain items Delta-V reads or writes</summary>

The app reads these locations:

| Location | Purpose |
| --- | --- |
| Keychain service `Claude Code-credentials`, account `$USER` | Claude access token |
| `~/.claude/.credentials.json` | Claude fallback when the Keychain item is missing |
| `~/.codex/config.toml` | Codex credential-storage setting |
| `~/.codex/auth.json` | Codex file-based sign-in |
| Keychain service `Codex Auth`, account `cli\|<hash>` | Codex sign-in when configured for `keyring` or `auto` |
| `~/.config/delta-v/config.toml` | Delta-V settings |
| `/Library/Application Support/ClaudeCode/managed-settings.json` and `managed-settings.d/` | Check for managed Claude settings before running renewal |
| `/Library/Managed Preferences/com.anthropic.claudecode.plist` and `/Library/Managed Preferences/<username>/com.anthropic.claudecode.plist` | Check for managed Claude preferences before running renewal |
| `~/.claude/remote-settings.json` | Check for cached Claude account policy before running renewal |
| `~/.claude/.oauth_refresh.lock` and `~/.claude.lock` | Check whether Claude is renewing credentials before stopping its helper |

Custom CLI directories change the credential paths above:

- Claude uses `CLAUDE_SECURESTORAGE_CONFIG_DIR` when set, then `CLAUDE_CONFIG_DIR`. An empty or absent effective override selects `~/.claude`. For a nonempty override, the Keychain service becomes `Claude Code-credentials-<hash>`. The suffix is the first eight SHA-256 characters of the override after Unicode normalization.
- Codex uses `CODEX_HOME`, or `~/.codex` by default. Its Keychain account suffix is the first sixteen SHA-256 characters of that directory's canonical path. Codex's storage setting determines which store is read. An access denial does not cause a fallback to another store.

Claude's policy cache follows `CLAUDE_CONFIG_DIR`; its refresh locks follow the credential directory. Delta-V checks only the presence of policy files and locks, without reading their contents. The directory lock uses the resolved credential directory followed by `.lock`.

Finder does not load environment variables from shell startup files. If you use a custom CLI directory, pass its absolute path when launching Delta-V from Terminal. For example, `CODEX_HOME="/absolute/path/to/codex" /Applications/Delta-V.app/Contents/MacOS/delta-v`. Quit any running copy first. The default directories work when opening the app from Finder.

Delta-V looks for the official clients in common installation locations. If yours is elsewhere, set `DELTA_V_CLAUDE_PATH` or `DELTA_V_CODEX_PATH` to the absolute executable path when launching Delta-V. A missing or outdated client does not prevent usage checks with a valid saved sign-in, but it cannot run the sign-in buttons.

Claude's sign-in actions require absolute custom directory paths. Remove empty directory overrides before using them.

Delta-V writes `~/.config/delta-v/config.toml`, using `~/.config/delta-v/config.toml.tmp` while saving. It does not write a usage history or a separate copy of your credentials.

Launch at login uses macOS's login-item service. Delta-V registers or unregisters its installed app only when you ask. macOS stores that setting; Delta-V saves only whether you dismissed the first-run prompt in its configuration file.

Sign-in helpers use a private `delta-v-auth-*` folder in the macOS temporary directory, removed when the action finishes. The official clients still use their own account storage and may update their own configuration, caches, and logs. Delta-V does not keep their terminal output.

</details>

## Configuration

All settings are available in the usage panel. You do not need to edit a file.

For manual configuration, quit Delta-V, edit `~/.config/delta-v/config.toml`, then restart it.

| Key | Default | Choices |
| --- | --- | --- |
| `providers` | `"both"` | `"claude"`, `"codex"`, or `"both"` |
| `claude_enabled` | `true` | Whether Claude is connected to Delta-V; `false` stops checks without signing out of Claude Code |
| `codex_enabled` | `true` | Whether Codex is connected to Delta-V; `false` stops checks without signing out of Codex |
| `tracked_limit` | `"auto"` | Most-used quota, or an available provider/limit ID chosen in Settings |
| `claude_windows` | `[]` | Up to two Claude limit IDs for the compact rows, in display order; empty means Automatic |
| `codex_windows` | `[]` | Up to two Codex limit IDs for the compact rows, in display order; empty means Automatic |
| `percentage_mode` | `"remaining"` | `"remaining"` or `"used"`, for the menu bar, provider summaries, and usage bars |
| `threshold` | `20` | Highlight when remaining usage falls below this percentage, from 0 to 100 |
| `refresh_seconds` | `60` | Base refresh interval, from 30 to 900 seconds; idle and error backoff still apply |
| `theme` | `"system"` | `"system"`, `"light"`, or `"dark"` |

Launch at login is managed by macOS, so it has no on/off value in this file. `launch_at_login_prompt_dismissed` records whether the first-run choice has been handled. It starts as `false` on new installs; existing configuration files without this field skip the prompt.

## Roadmap

- [x] Build and run from source on macOS.
- [ ] Offer a signed, notarized `.dmg` download that installs into Applications.
- [ ] Add a Homebrew cask for installation and updates.
- [x] Offer Launch at login during setup and in Settings.
- [ ] Show today's usage and the last seven days, with history stored on your Mac.

Under consideration: API usage and spending in a separate view. API billing would need its own data sources and account setup, and would stay separate from subscription allowances.

## Contributing

Bug reports, fixes, and clearer documentation are welcome. For a new feature, open an issue first so we can discuss how it fits.

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, testing, and how to send a pull request.

## License

[MIT](LICENSE)
