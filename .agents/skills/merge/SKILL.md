---
name: merge
description: >-
  Merge a requested kadr change through a verified GitHub pull request. Invoke
  only when the user explicitly requests merging or landing, not for review,
  preparation, passing checks, or installation of this skill.
disable-model-invocation: true
metadata:
  delta-action: land
---

# Merge a kadr change

An explicit merge request, including `/merge`, authorizes this workflow:
prepare commits, publish the topic branch, create or update its PR, fix
in-scope failures, and merge. Proceed without asking for the same permission
again.

1. Inspect remotes, the current branch and upstream, staged and unstaged
   diffs, untracked files, and commits ahead of the destination. Verify the
   publication remote is GitHub `mkrdnk/kadr`, not a local checkout. Fetch
   current refs. Use the requested base, otherwise the verified GitHub default
   (historically `master`). Inspect applicable contribution instructions, PR
   templates, destination rules, permissions and allowed merge methods. Use
   authenticated GitHub tooling; do not assume the `gh` CLI is installed.
2. Use a topic branch for the change; if currently on the destination, branch
   from the change state without moving or publishing the destination. Review
   all new and modified files, including staged changes; commit all and only
   the requested work in logical units using non-interactive commits with
   English messages (`feat:`, `fix:`, `test:`, `docs:` as appropriate). Include
   applicable tests and documentation. Preserve unrelated work and the user's
   staging; do not commit credentials or generated build output. Ask if scope
   is unclear or changes cannot safely be separated. Inspect the entire diff
   against the base, not just the last commit.
3. Run checks proportional to the diff. The `fmt`, `check`, `clippy` and
   `test` targets in `Makefile` define `make fmt`, `make check`, `make clippy`
   and `make test` respectively. The workspace uses Rust edition 2024
   (`Cargo.toml`); CI uses stable Rust with rustfmt and Clippy. For engine or
   dependency-boundary changes, `.github/workflows/ci.yml` (Headless engine
   checks; Engine and presentation dependency boundary) defines
   `cargo test --locked -p kadr-engine`, `python3 scripts/check-architecture-test.py`
   and `python3 scripts/check-architecture.py`. If a local component, native
   runtime or second OS is unavailable, report the omitted local check and
   verify its CI counterpart; never describe it as a local pass.
4. Push only the topic branch without force. Create a GitHub PR to the
   verified base, or update the existing matching PR. Describe the actual
   change, tests and limitations; use the applicable template and link an
   issue when relevant. If synchronization is needed, merge the updated base
   into the topic branch non-interactively. Resolve clear conflicts while
   preserving both sides' intent; stop and identify the conflicting files and
   decision needed when intent is ambiguous or unrelated work is at risk.
   Validate and publish the resulting topic commit.
5. Monitor GitHub Actions for the current PR revision. Both `test` matrix
   entries in `.github/workflows/ci.yml` (Ubuntu 24.04 and macOS 15) must
   succeed, including their applicable native playback, rendering and
   packaging checks. The dispatch-only `candidates` job is not a PR check;
   do not trigger a release or artifact publication. On failure report the
   job and failing step, inspect logs, correct in-scope causes on the topic
   branch, run relevant local checks, push and monitor the new revision.
   Do not weaken checks, bypass rules, or retry away a reproducible failure.
6. Immediately before merging, verify base, PR head SHA, required approvals,
   resolved discussions where required, both CI jobs and **every required
   check** for the current candidate under the destination's rules. Pending,
   missing, stale, failing or unverifiable required results are not success.
   If the head changes, revalidate. Merge through GitHub with the verified
   head as a precondition, preferring a merge commit as in earlier kadr PRs;
   follow the destination's allowed strategy or required queue instead. If
   queued, wait for the queue's required candidate checks and actual merge.
7. Confirm GitHub marks the PR merged into the intended branch, fetch the
   destination and verify it contains the reported merge result. Report the
   PR, destination and merge SHA. If blocked before merge, state that the
   change has not merged and give the specific blocker. If the merge request
   was sent but its outcome cannot be verified, report that uncertainty
   rather than claiming success or submitting another merge blindly.
