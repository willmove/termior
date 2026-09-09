# GitHub Release Process

This runbook is the canonical procedure for publishing a Termior release to GitHub. It is adapted
from the Markion release process (`markion/docs/release-process.md`) and records the process
verified by the v0.1.4 release. A release is complete only after the tag workflow succeeds, all
installers are attached, **curated bilingual release notes are present**, and the repository is
synchronized with GitHub.

## 1. Defaults and prerequisites

- Publish from `main` with a clean worktree and a local branch synchronized with `origin/main`.
- Use the version explicitly requested by the maintainer. If no version is supplied, increment
  PATCH from the highest `vMAJOR.MINOR.PATCH` tag. Do not infer a major, minor, or prerelease
  version.
- Publish a stable, non-draft Release unless the maintainer explicitly requests otherwise.
- **Every release must carry curated release notes describing this version's features and fixes**
  (see §7). The workflow's auto-generated notes are only a seed, never the final description.
- Write final release notes bilingually by default — English first, followed by the corresponding
  Simplified Chinese version — unless another language arrangement is requested.
- Preserve public tags. Never delete, force-move, or recreate a published tag without explicit
  authorization.
- Required tools: stable Rust and Cargo, Git, GitHub CLI (`gh`), and an authenticated GitHub
  account with permission to push and publish Releases.

Check the operating context before editing anything:

```bash
gh auth status
git fetch --tags origin
git status --short --branch
git tag --sort=-version:refname
git log --oneline --decorate -10
```

Confirm all of the following:

- The repository is `willmove/termior` and the default branch is `main`.
- The worktree has no staged, unstaged, or untracked release-related changes.
- `main` is neither behind nor unexpectedly ahead of `origin/main`.
- The intended version does not already exist as a local tag, remote tag, or GitHub Release:

```bash
git tag --list vX.Y.Z
git ls-remote --tags origin vX.Y.Z
gh release view vX.Y.Z   # expected to return not-found for a new version
```

- The latest `main` CI state is green, **or** every red job is a failure that has been understood
  and documented before tagging. As of v0.1.4 the known pre-existing red state on `main` is: the
  `Core (ubuntu-latest)` Test step and the `Core coverage ≥ 80%` job time out on Ubuntu runners,
  while fmt/clippy/build on all platforms and all three `Desktop` jobs pass. The publication gate
  is the Release workflow (`.github/workflows/release.yml`), which builds and packages on all
  three platforms but does not run those tests.

## 2. Build the change summary

Use the previous stable tag as the comparison base. Review the complete commit set and diff,
including direct commits that GitHub's generated notes may omit:

```bash
git log --oneline <previous-tag>..HEAD
git diff --stat <previous-tag>..HEAD
```

Read the relevant specs and design docs under `docs/` when a change references them. Release
notes must describe user-visible behavior rather than copying commit subjects or listing internal
tooling changes.

Before publication, identify:

- Major user-visible features and improvements.
- Important bug fixes.
- Compatibility, migration, and known limitation information.
- Any platform support or installer changes.
- Verification evidence that can truthfully be reported.

## 3. Synchronize version metadata

Termior keeps a single source of version truth, `[workspace.package].version` in the root
`Cargo.toml`, inherited by every crate. Change it to `X.Y.Z` and let Cargo refresh the lockfile
(never blind-replace version strings in `Cargo.lock`; third-party packages can legitimately use
the same numbers):

```bash
# edit [workspace.package].version in Cargo.toml, then:
cargo update --workspace
cargo metadata --no-deps --format-version 1 | jq -r '.packages[].version' | sort -u
```

Confirm every Termior workspace package resolves to `X.Y.Z`. The expected diff is exactly
`Cargo.toml` (1 line) plus the workspace member version lines in `Cargo.lock`. The Release
workflow validates that the tag version matches `Cargo.toml` and builds with `--locked`, so a
stale lockfile fails the release.

## 4. Validate before publication

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude termior -- -D warnings
git diff --check
git diff -- Cargo.toml Cargo.lock
```

Stop before tagging if the diff includes unintended edits or the target tag/Release collides.
Treat red `main` CI jobs as blockers unless they are the documented pre-existing failures from
§1 (or newly understood and recorded here).

## 5. Commit, tag, and push

Create one dedicated version-bump commit and an annotated tag pointing at it (the v0.1.3
precedent):

```bash
git add -- Cargo.toml Cargo.lock
git commit -m "Bump workspace version to X.Y.Z"
git tag -a vX.Y.Z -m "Release Termior vX.Y.Z"
git push origin main vX.Y.Z
```

Only a `v*` tag ref executes the `Publish GitHub Release` job, so the tag run is the
authoritative publication run.

## 6. Monitor the tag workflow

```bash
gh run list --workflow release.yml --limit 5 --json databaseId,headBranch,event,status,conclusion,url
gh run watch <tag-run-id> --exit-status --interval 15
```

All of these jobs must succeed:

- `Package (linux)` — tar.gz + .deb + checksums.
- `Package (macos)` — tar.gz + .dmg + checksums.
- `Package (windows)` — zip + Inno Setup `-setup.exe` + checksums.
- `Publish GitHub Release` — attaches all artifacts and creates the Release with generated notes.

If the workflow fails, inspect it with `gh run view <tag-run-id> --log-failed`. Do not report the
release as complete. If the public tag already exists, preserve it and either fix forward or ask
the maintainer how to proceed.

## 7. Curate the release notes

The workflow creates the Release with generated notes. Treat those notes as a seed, not the
final description. Draft the curated notes in a file outside version control (conventionally
`dist/release-notes-vX.Y.Z.md`, which is gitignored), then replace the generated notes:

```bash
gh release edit vX.Y.Z --title "Termior vX.Y.Z" --notes-file dist/release-notes-vX.Y.Z.md
```

Use this structure unless the release content calls for a small adjustment:

```markdown
# Termior vX.Y.Z

One-sentence summary of the release.

## Highlights

### Feature or improvement area

- User-visible change and its practical effect.

### Fixes

- Important reliability or behavior fix.

## Compatibility

- State whether settings or persisted data require migration.
- State relevant platform limitations. Termior's Windows and macOS installers are unsigned, so
  SmartScreen or Gatekeeper bypass steps are normally required.

## Downloads

- Windows x64: Inno Setup installer (`termior-X.Y.Z-windows-x86_64-setup.exe`) and portable zip.
- macOS: `termior-X.Y.Z-macos-x86_64.dmg` and `.tar.gz`.
- Linux x86_64: `.deb` and `.tar.gz`.
- Every asset ships with a matching `.sha256` checksum.

## Verification

- Release workflow build/packaging result per platform.
- `main` CI status, including any known pre-existing failures.

**Full comparison**: https://github.com/willmove/termior/compare/<previous-tag>...vX.Y.Z

---

# Termior vX.Y.Z（中文说明）

一句话版本总结。

## 主要更新

### 功能或改进领域

- 用户可见的变化及其实际效果。

### 修复

- 重要的可靠性或行为修复。

## 兼容性

- 说明设置或持久化数据是否需要迁移。
- 说明相关的平台限制。Termior 的 Windows 与 macOS 安装包未签名，通常需要手动绕过
  SmartScreen 或 Gatekeeper。

## 下载

- Windows x64：Inno Setup 安装程序（`termior-X.Y.Z-windows-x86_64-setup.exe`）与便携版 zip。
- macOS：`termior-X.Y.Z-macos-x86_64.dmg` 与 `.tar.gz`。
- Linux x86_64：`.deb` 与 `.tar.gz`。
- 每个产物均附带对应的 `.sha256` 校验文件。

## 验证

- 发布工作流在各平台的构建与打包结果。
- `main` CI 状态，包括任何已知的预存失败。

**完整变更对比**: https://github.com/willmove/termior/compare/<previous-tag>...vX.Y.Z
```

Release-note rules:

- Derive claims from the actual tag-to-tag diff and commits.
- Cover direct commits as well as merged pull requests.
- Prefer user outcomes over implementation details; include technical detail only when it
  explains compatibility, safety, performance, or fidelity.
- State "no migration required" only after checking persisted settings and data formats.
- Do not claim a test, platform build, installer, or feature that was not verified.

## 8. Final verification

```bash
gh release view vX.Y.Z --json name,tagName,isDraft,isPrerelease,publishedAt,url,body,assets
git status --short --branch
git log -1 --oneline --decorate
git ls-remote --heads --tags origin main vX.Y.Z
```

Confirm that:

- The title is `Termior vX.Y.Z` and the tag is `vX.Y.Z`.
- The Release is published, not a draft, and not an unintended prerelease.
- The curated bilingual notes and the full comparison link are present.
- The assets include all 12 files: per platform (windows zip + setup.exe, macos tar.gz + dmg,
  linux tar.gz + deb), each with its `.sha256` sidecar.
- The tag workflow succeeded on all three platforms and in the publish job.
- Local `main`, `origin/main`, the version-bump commit, and the annotated tag resolve to the
  intended release state, and the local worktree is clean.

Only after every check passes should the release be reported as complete, with links to the
Release and the tag workflow run.
