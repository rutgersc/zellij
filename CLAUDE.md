# Zellij fork

Long-lived fork of upstream zellij with additional features and fixes. See `git log` for specifics.

## Build provenance — `zellij --version`

`zellij-utils/build.rs` stamps the git SHA into the binary at compile time. `zellij --version` self-identifies:

    zellij 0.45.0 (f6696d920000)

A `-dirty` suffix appears when the working tree had uncommitted changes at build time. `unknown` means the build happened outside a git checkout.
