# Branch consolidation record

Date: 2026-10-20

## Result

The repository now uses `develop` for integration and `production` as the preserved production baseline. GitHub's default branch is `develop`; the only local and remote branches are `develop` and `production`.

`production` remains at `c5c2701dfead5c1bf51ddbb643c6c7f0c27a5c23`, the former `origin/main` tip. `develop` contains an explicit merge of that commit and `8f30bb1c037f7830faf0688e0973cf8dfd5524d7`, the former `codex/email-brand-logo` tip. The original local `main` commit `18d7a5ecc161c25c09fe94ff361a83727a0ca865` remains in both histories. The two source tips had identical file trees before integration; the merge preserves the feature branch's commit ancestry without changing the already integrated content.

The former remote `main` and `codex/email-brand-logo` refs and the local `main` ref were removed after the replacement branches were pushed and GitHub's default branch was switched. No other office-specify-app branch or registered worktree contained remaining feature work. No deployment was performed.

## Validation and deferred work

`cargo test -p domain` passed on the integrated tree: 69 passed, 0 failed. This includes tests for preserving the fixed email brand logo and rejecting untrusted image sources and attributes. No frontend files changed, so no frontend-specific test applied.

`cargo fmt --all -- --check` remains failing. Rustfmt reports formatting differences across multiple existing files, including a line-wrap change in `crates/domain/src/templates.rs`. Broad formatting cleanup was outside this consolidation and remains deferred. The test run was performed separately and passed.

No secret files were staged. Local environment files remain ignored and untouched. No GitHub Actions workflows, deployment environments, deployment records, or checked-in deployment configuration were found; external hosting integrations were not changed or audited.

There are no known unfinished feature-branch changes or merge conflicts. The remaining follow-up is to resolve or explicitly accept the repository-wide formatting baseline, then rerun `cargo fmt --all -- --check` when that cleanup is in scope.
