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

The development app appears in the menu bar. It uses your normal Delta-V settings and saved CLI sign-ins, and contacts the providers' usage services. Follow [Connect your accounts](README.md#connect-your-accounts) if needed. An Apple Developer account is not required to run it locally.

For interface work without an account, run this instead:

```sh
npm run dev
```

Open `http://127.0.0.1:1420` in your browser. This preview uses sample data and keeps settings in memory. It does not read credentials or fetch usage. Native tray behaviour, Keychain access, and panel positioning still need to be checked in the macOS app.

## Making changes

Keep each pull request focused on one problem. A few conventions matter here:

- Keep credentials, polling, and parsing in Rust. Keep the frontend concerned with display and interaction, and macOS calls in the `platform` module.
- Preserve where each number came from. Label estimates, leave missing readings unavailable, and keep API billing separate from subscription allowances.
- Use typed Rust errors. Avoid `unwrap()` and `expect()` outside tests and startup code. TypeScript stays strict, without `any` or `as unknown as`.
- Add a regression test when fixing parsing, reset calculations, or percentage and threshold logic. React render snapshots are not required.
- Write comments that explain why a choice was made. Keep user-facing text plain, without emoji or em dashes.

Parser fixtures live in `src-tauri/tests/fixtures/`. If you change a parser, review the input fixture and expected output together. Do not replace expected output just to make a test pass. Follow the [fixture notes](src-tauri/tests/fixtures/README.md) when adding a response: remove credentials and identifying fields while preserving the structure needed to reproduce the problem.

## Checking your work

For code changes, run these from the project root:

```sh
npm run build
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

## Sending a pull request

1. Review your changes and remove unrelated edits or generated build files.
2. Commit the change with a short, descriptive message, such as `Handle expired usage windows`.
3. Push your branch to your fork and open a pull request against Delta-V's `main` branch.
4. Explain the problem, what changed, and how you checked it. Link the issue if there is one.

A draft pull request is fine if you want feedback before finishing. Keep follow-up changes in the same pull request so the discussion stays together.
