# Zellij fork

Long-lived fork of upstream zellij with additional features and fixes. See `git log` for specifics.

## Build provenance — `zellij --version`

`zellij-utils/build.rs` stamps the git SHA into the binary at compile time. `zellij --version` self-identifies:

    zellij 0.45.0 (f6696d920000)

A `-dirty` suffix appears when the working tree had uncommitted changes at build time. `unknown` means the build happened outside a git checkout.

## Plugin dev workflow

Two modes, signalled by the state of `config.kdl` in the foam vault (a separate repo):

- **Quick** — `config.kdl` has `<plugin> location="file:.../target/wasm32-wasip1/release/<plugin>.wasm"`. The dev wasm loads directly on plugin instance creation. Rebuild loop: `cargo build -p <plugin> --target wasm32-wasip1 --release` (~1.5min), then **kill the session and start a fresh one** — detaching alone doesn't reload, the server keeps the plugin instance in memory. The `file:` line shows up as an unstaged diff in the foam vault — *that diff is the signal we're in iter mode*.
- **Native** — alias is `zellij:<plugin>`. The wasm is `include_bytes!`'d from `zellij-utils/assets/plugins/<plugin>.wasm` into the zellij binary, so baking requires: copy dev wasm into `assets/plugins/`, `cargo build --release`, then `.\copy.ps1` (run from OUTSIDE zellij — it kills running zellij processes).

The pre-commit hook at `.githooks/pre-commit` blocks a commit if `default-plugins/<X>/` source is staged without the corresponding `zellij-utils/assets/plugins/<X>.wasm`. It also handles `agent-readmodel` (shared lib feeding agent-bar and compact-bar). Bypass with `--no-verify` only when the source change is a verified no-op (e.g. comment-only).

The hook file is tracked; the `core.hooksPath = .githooks` setting is not (it lives in `.git/config`, which git never versions). One-time per clone:

    git config core.hooksPath .githooks

Verify: `git config --get core.hooksPath` should print `.githooks`. If it prints nothing, the hook won't run.

When Claude is iterating on a plugin, the bake step is at commit time — do it then, not after every edit.

## Plugin ↔ shell snippet contract — `auto-tab-name`

The `auto-tab-name` plugin reads `PaneInfo.title` (set by OSC 0/2 escapes) to derive tab names. Apps that emit OSC 0 themselves (nvim, claude, lazygit, ssh) work without help. Plain shell panes won't — the shell has to emit OSC 0 from its prompt.

The shell-side snippets ship inside the plugin's own directory so the two pieces version together:

    default-plugins/auto-tab-name/shell/auto-tab-name.ps1   (PowerShell)
    default-plugins/auto-tab-name/shell/auto-tab-name.zsh   (zsh — TODO)

The user's `$PROFILE` / `~/.zshrc` (foam-vault-controlled) dot-sources these files by path. The path itself — `…/auto-tab-name/shell/…` — names the dependency, so future-you finds the link without a separate comment to maintain. The PowerShell loader is expected to `Write-Warning` if the file can't be found.

When changing the plugin's name-derivation rules (what counts as "boring", how `terminal_command`/`title` are parsed), check whether the shell snippet's emitted format still satisfies them. Both ends of the contract live here; don't drift them.
