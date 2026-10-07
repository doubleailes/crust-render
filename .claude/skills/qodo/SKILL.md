---
name: qodo
description: Address the Qodo review on the current branch's pull request — fix the valid findings with tests, decline the rest with a reason, push, reply in each thread, and wait for CI. Use when the user asks to check the PR, address Qodo comments, or fix review findings.
allowed-tools: Bash(gh:*), Bash(git:*), Bash(cargo:*)
---

Address the Qodo review on a crust-render pull request, end to end.

**Input**: optionally a PR number. Otherwise use the PR of the current branch
(`gh pr view --json number,headRefName,url`). If there is none, say so and stop.

## Steps

1. **Sync the branch first.**
   - `git fetch origin` and make sure the local branch is not behind its remote
     (`git status -sb`). If it is, `git pull --ff-only`; never force-push.
   - Check the branch still **rebases** cleanly onto `origin/main` (the repo
     rebase-merges, so a clean merge is not enough):
     `git merge-tree --write-tree origin/main HEAD` is a quick read-only probe; if
     it reports conflicts, tell the user before going further.

2. **Collect the findings.** Qodo posts in two places:
   - inline review comments: `gh api repos/{owner}/{repo}/pulls/<N>/comments --paginate`
     — keep those with `user.login == "qodo-code-review[bot]"` and `in_reply_to_id == null`;
   - the summary / "code review" issue comments:
     `gh api repos/{owner}/{repo}/issues/<N>/comments --paginate`.
   Skip any thread that already has a reply from the user or from a previous pass.
   Read every finding in full (the body holds the suggested fix and its reasoning).

3. **Classify each finding** against the code as it is now, not as Qodo saw it:
   - **valid** — a real bug, wrong number, misleading doc or missing test;
   - **declined** — wrong, already handled, or contrary to a rule in `CLAUDE.md`
     or the capability's `openspec/specs/<capability>/design.md`.
   Show the user a short table (finding → verdict → one-line reason) before editing.
   If a verdict is a judgment call that is the user's to make, ask.

4. **Fix the valid findings.**
   - Behaviour changes get a regression test that fails on the old code.
   - A change that could move pixels is checked with `scripts/check_images.sh check`
     (16 spp) — say so if it was not run. Do not launch long production-scene
     measurements (ALab, Moana) unless the user asks.
   - Docs that describe the changed behaviour (`docs/`, `site/`, `openspec/specs/`)
     are updated in the same commit.

5. **Gate locally**, exactly as CI does:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace --no-fail-fast
   ```

6. **Commit and push** (only if something changed): one commit describing what was
   fixed, ending with the attribution trailer from the session's instructions.
   `git push` — never `--force`.

7. **Reply in each thread**, once, briefly:
   - fixed → what changed, the **exact** short hash (copy it from `git rev-parse --short HEAD`
     after the push), and the test that pins it;
   - declined → one or two sentences of reasoning, citing the code or design record.
   Inline comments: `gh api repos/{owner}/{repo}/pulls/<N>/comments/<id>/replies -f body=...`.
   Do **not** post a summary comment, resolve threads, or reply to anything else
   unless the user asks.

8. **Wait for CI**: `gh pr checks <N> --watch` in the background; report the result.

## Report

End with a table — finding, verdict, commit / reason, reply link — plus the CI status
and anything skipped (e.g. golden check not run).
