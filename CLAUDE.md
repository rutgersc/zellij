# Zellij fork

Long-lived fork of upstream zellij with additional features and fixes. See `git log` for specifics.

## Commits

Two orthogonal concerns: **fork vs upstream** (where did this commit come from?) and **Claude traceability** (which conversation produced it?). Don't conflate them.

### Fork vs upstream — encoded in the branch name

**Topology invariant:** the branch is always exactly one linear chain of fork commits stacked on top of one upstream commit.

    <upstream tip> ──► fork-commit-1 ──► fork-commit-2 ──► … ──► HEAD

We never cherry-pick, never merge upstream into the middle, never interleave. When upstream advances, we rebase the whole fork onto the new tip — that's the only way upstream commits enter the branch.

Because the topology is always this shape, **branches encode the upstream commit they sit on top of as a prefix**:

    <upstream-short-sha>-<branch-name>

e.g. `abc1234-latest`, `abc1234-feat-something`. Reading rule: every commit reachable from `abc1234` is upstream; everything past it on the branch is fork-original. No per-commit marker needed.

When you rebase onto a new upstream tip `def5678`, rename the branch to `def5678-latest`. The branch name *is* the merge-base pointer.

    git log $(git symbolic-ref --short HEAD | cut -d- -f1)..HEAD    # fork-original commits on this branch

### Claude traceability

**Any commit Claude makes in this repo must carry a `Claude-Session-Id:` trailer** pointing back to the conversation that produced it. This is a separate concern from fork-vs-upstream: manual commits (Rutger) don't need it.

Get the current session ID with:

    ls -t ~/.claude/projects/*/*.jsonl 2>/dev/null | head -1 | xargs -I{} basename {} .jsonl

Append as a trailer in the commit message:

    fix(sessions): delete-all-sessions reaps DEAD registry entries

    Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
    Claude-Session-Id: 6abd795e-a3a6-4caf-8439-dbd616524e69

`git log --grep='Claude-Session-Id:'` lists every Claude-driven commit; the inverse is the set of manual ones.

## Fork maintenance — minimize edits to existing files

Every line we change in an upstream-owned file is a potential merge conflict on the next rebase. The rule for new fork work:

**Default:** put fork-only *logic* in a new file. Touch existing upstream files only with the minimum hook needed to wire it in (a `mod foo;` declaration, a single call line, a one-method accessor).

- Prefer `impl ExistingType { fn fork_thing(&mut self) { … } }` in a new file over inlining the body in the original file. Rust allows split `impl` blocks across files in the same crate. With `#[path]` you can declare the new file as a *child* module of the upstream-owned file, which gives the impl access to private fields without changing their visibility anywhere.
- If the fork addition needs internal access and a split `impl` won't work, prefer adding *one* small accessor method that mirrors an existing one's shape over flipping field visibility. Methods are easier to read in diff and don't broaden the public surface elsewhere.
- This rule trades some code-quality conventions (locality, dead-code visibility) for rebase ergonomics. When a fix lands upstream, the new file gets deleted and the rebase resolves cleanly.

Files that don't exist on `zellij-org/main` are the fork-only ones; `git log --diff-filter=A -- <path>` or comparing against the merge-base shows which ones we own.

## Build provenance — `zellij --version`

`zellij-utils/build.rs` stamps the git SHA into the binary at compile time. `zellij --version` self-identifies:

    zellij 0.45.0 (f6696d920000)

A `-dirty` suffix appears when the working tree had uncommitted changes at build time. `unknown` means the build happened outside a git checkout.
