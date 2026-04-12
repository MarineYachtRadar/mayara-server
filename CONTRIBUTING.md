# Contributing to Mayara Server

Thanks for wanting to contribute. This document describes the **workflow** for getting a change into Mayara Server — what happens from `git clone` to seeing your change in a release. For **authoring** rules (code style, testing expectations, PR scope, commit format), the source of truth is [`AGENTS.md`](AGENTS.md); this document points at it rather than duplicating it.

## Quick start

```bash
git clone https://github.com/MarineYachtRadar/mayara-server.git
cd mayara-server
cargo build --all
cargo test
```

Build instructions for each platform are in [`BUILDING.md`](BUILDING.md). Runtime options are in [`USAGE.md`](USAGE.md).

## Where the rules live

| Document | What it covers |
|---|---|
| [`AGENTS.md`](AGENTS.md) | Code quality principles, commit format, PR scope, rebase policy, "one logical change per PR" |
| [`BUILDING.md`](BUILDING.md) | Toolchain, cross-compile, platform-specific build notes |
| [`USAGE.md`](USAGE.md) | Runtime CLI flags and examples |
| [`README.md`](README.md) | Project overview, Docker, supported radars |
| This document | Branching, CI pipeline, CodeRabbit review, CHANGELOG generation, release flow |

Read `AGENTS.md` before opening your first PR. The rules there are enforced — both by maintainers and by any AI tools used during authoring.

## Branching and pull requests

### `main` is protected — all changes go through a pull request

Direct pushes to `main` are blocked by a repository ruleset — verified. Every change, including docs, chores, and small fixes, must go through a pull request.

Branch from the latest `main`:

```bash
git fetch origin
git checkout -b my-change origin/main
```

Use **hyphens** in branch names, not slashes (e.g. `furuno-drs4w-fix`, not `fix/drs4w`). This is a Signal K ecosystem convention that several tools in the pipeline prefer.

### Open the PR against `main`

Use `gh pr create` or the GitHub web UI. The PR must target `main` — there is no `develop` branch. If your change depends on another open PR, open it as a stacked PR against that PR's branch and note the dependency in the description.

**Draft PRs are not needed.** Open a regular PR and iterate in the open — maintainers would rather see work-in-progress than have you sit on a draft waiting for "readiness". The only use case for draft is if you specifically want to suppress CodeRabbit auto-review (see [`.coderabbit.yaml`](.coderabbit.yaml): `drafts: false`).

**Leave "Allow edits and access to secrets by maintainers" checked.** It is on by default for PRs from forks. Maintainers appreciate being able to push small fixes (a typo, a rebase, a missing `cargo fmt` hunk) directly onto your branch instead of asking you to do a round-trip. Nothing controversial ever lands that way — anything beyond a nit gets discussed.

## What happens after you open a PR

Three things run automatically:

1. **CodeRabbit review** — a GitHub App posts an automatic review within a minute or two. It summarizes your changes, flags issues, and suggests fixes. **Treat CodeRabbit as a first-pass review**: address its findings or explain why you are leaving them. The review profile is `assertive` and the project-specific rules (conventional-commit PR title, no manual CHANGELOG edits, no version bumps in contribution PRs, echo-comment and AI-attribution-footer blocks, etc.) are pinned in [`.coderabbit.yaml`](.coderabbit.yaml). CodeRabbit is advisory and non-blocking at the branch-protection level, but unresolved findings will slow down human review. If you want to catch findings before pushing, you can run `cr review --plain` locally against your branch — this is optional, not required; CodeRabbit runs on the PR either way. See [Before opening the PR](#before-opening-the-pr) below.
2. **Rust CI** (`.github/workflows/rust.yml`) — cross-builds `--release` for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, and `x86_64-pc-windows-gnu`, plus a native build on `windows-latest` that **also runs `cargo test --release`**. Note that tests only run on the Windows job; the Linux cross-build does not run tests, so your local `cargo test` is the primary quality gate on Linux-specific code. Run it before pushing.
3. **Copilot code review** — the ruleset enables Copilot code review on PRs (non-blocking, informational).

There is no Docker build on PRs — the Docker workflow only fires on push to `main` and on tag releases.

### Before opening the PR

Per [`AGENTS.md`](AGENTS.md#pull-request-guidelines) — this is the minimum:

- `cargo test` passes locally. CI runs `cargo test --release` only on the Windows native job; the Linux cross-build jobs are build-only. That makes your local `cargo test` the only pre-merge signal on Linux-specific behaviour.
- Rebase onto `origin/main`, squash fixup commits, leave only intentional commits in history
- Self-review the diff: the PR description should explain **why**, not **what**

**Running `cr review --plain` locally is optional, not required.** CodeRabbit runs automatically on every PR via the GitHub App, so you will get its feedback either way. The local CLI just shortens one round-trip. Use it when your change is non-trivial code in a high-scrutiny area (protocol parsing, TLS, auth, anything radar-protocol-shaped) where catching findings before push is worth the few seconds. Skip it for docs-only PRs, formatting fixes, or one-liners — running it on every change is friction without payoff.

### PR title and description

The **PR title** must be a conventional commit (see [Commit conventions](#commit-conventions) below). With squash merge, the PR title becomes the commit message on `main` and directly feeds the changelog. Treat it as the changelog entry that users will read.

The **PR description** should be succinct: motivation and approach. Do not pad it with a mechanical listing of what changed — the diff shows that. If you include a `## Tested` section with checkboxes, every box must be ticked before review (unticked boxes mean unfinished work). Do not include speculative test plans.

Do not include AI-generated footers (`🤖 Generated with Claude Code`, `Co-Authored-By: Claude`, etc.) in commits or PR descriptions.

## Commit conventions

Mayara Server uses **Angular-style conventional commits**:

```text
<type>(<scope>): <subject>

<optional body — 72-char wrap, explain why>

<optional footer — "closes #123">
```

- **Subject**: ≤ 50 chars, imperative mood ("add" not "added"), no period
- **Scope**: radar brand or subsystem (`furuno`, `navico`, `signalk`, `stream`, `navdata`, etc.)
- **One logical change per commit**, one logical change per PR

### Types that feed the changelog

`cliff.toml` maps commit types into changelog groups. The practical effect:

| Type | Changelog group | Notes |
|---|---|---|
| `feat` | **Added** | User-visible new feature |
| `fix` | **Fixed** | Bug fix |
| `refactor` | **Changed** | Internal rework with user-visible effect |
| `perf` | **Changed** | Performance improvement |
| `docs` | **Changed** | Documentation (note: goes in changelog) |
| `style` | — | Skipped from changelog |
| `test` | — | Skipped from changelog |
| `chore` | — | Skipped from changelog |
| `ci` | — | Skipped from changelog |

A handful of special cases are also skipped by `cliff.toml`: `chore(release)`, `chore(deps)`, `docs(changelog): update CHANGELOG`, and any commit matching `address.*CR.*findings` or `address.*CodeRabbit` — so the conventional way to name a fixup commit that addresses CodeRabbit feedback is `fix(furuno): address CodeRabbit findings`, and it will not appear in the changelog.

The full list of accepted types (per [`AGENTS.md`](AGENTS.md#git-commit-conventions)) is `feat | fix | docs | style | refactor | test | chore | perf | ci`.

## CHANGELOG is auto-generated — do not edit

[`CHANGELOG.md`](CHANGELOG.md) is regenerated from commit history by [git-cliff](https://git-cliff.org/) via two workflows:

- `.github/workflows/changelog.yml` — runs on every push to `main`, generates the current changelog, opens a `docs(changelog): update CHANGELOG.md` PR, and auto-merges it.
- `.github/workflows/release.yml` — runs on every `v*` tag push, generates release notes from the range since the previous tag, creates a GitHub Release, and opens an auto-merged changelog PR for the tagged version.

**Never edit `CHANGELOG.md` manually.** If your PR touches it, a maintainer will ask you to remove the hunk. The only way to change what appears in the changelog is to change your **commit message** — that's why conventional commits matter. Historical entries before the git-cliff migration live in [`CHANGELOG.manual.md`](CHANGELOG.manual.md) and are appended to every generated changelog.

## Version numbers are maintainer-managed

**Never change the version in `Cargo.toml`** in a contribution PR. Version bumps happen in release-preparation commits controlled by maintainers. If you believe a release is warranted, say so in your PR description and let a maintainer handle it.

## Release flow (maintainer reference)

1. A maintainer bumps the version in `Cargo.toml`, commits as `chore(release): vX.Y.Z`, pushes to `main`.
2. Maintainer tags that commit with `git tag vX.Y.Z && git push --tags`.
3. `release.yml` runs: cross-builds Linux (x86_64 + arm64 musl) and Windows binaries, generates release notes from commits since the previous non-skipped tag, creates a GitHub Release, uploads binaries, and opens an auto-merged `docs(changelog): update CHANGELOG.md for vX.Y.Z` PR.
4. `docker.yml` runs (triggered by `release.yml`), builds and pushes `ghcr.io/marineyachtradar/mayara-server:latest` and `:vX.Y.Z` for `linux/amd64` and `linux/arm64`.

Contributors do not need to do anything for a release beyond writing good conventional commits.

## Reporting bugs

Open an issue using the bug report template at [`.github/ISSUE_TEMPLATE/bug_report.yml`](.github/ISSUE_TEMPLATE/bug_report.yml). Include:

- Mayara Server version and platform
- Radar model and brand
- Log output at `--verbose` (or `-v -v` for trace)
- Steps to reproduce

For security issues, do not open a public issue — contact the maintainers directly.

## Questions

General discussion happens in the Signal K community Discord. For Mayara-specific design questions, open a GitHub Discussion or draft PR and tag a maintainer.
