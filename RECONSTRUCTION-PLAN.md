# Branch reconstruction plan

Rebuild the fork as the **smallest set of commits that is byte-identical at the tip** to
`backup/atomize-pre-cleanup`, with every commit owning its files.

- **Base:** `e9173cba` — *feat: PWA support for the web client (#5184)*, the merge-base with `zellij-org/main`
- **Original:** `backup/atomize-target` — **66 commits**, 100 files, **57 files touched more than once**
- **Target:** ~33 commits, **0 files touched more than once** apart from the deliberate exceptions in §6

> Updated after commits 64–66 landed. They removed dead code and hoisted a probe,
> which turned three items into **churn** (added *and* deleted inside the range) — see
> §3.1. The diff target is now the 66-commit tip, not the 63-commit one.

## 1. The contract

**Strict diff-equivalence.** `git diff backup/atomize-target <new-tip>` must be **empty**.
Nothing is dropped, nothing is rewritten, no "while I'm here" fixes. This is a pure
re-partition of the same bytes across fewer commits.

Anything that would change content is out of scope and belongs in §8 as a follow-up,
*after* the reconstruction is verified empty.

## 2. How this was derived

```bash
python C:/Projects/foam/scripts/git-overlap/overlap.py "e9173cba..HEAD"
git log --reverse --no-merges --format='%h|%an|%s' --name-status e9173cba..atomize
git show --format= --unified=0 <sha> -- <file> | grep '^@@'     # per-hunk regions
git log -S '<symbol>' --oneline e9173cba..atomize -- <file>     # add/delete pairs
```

## 3. The finding that shapes everything

**The overlap is temporal, not conceptual.** 56 files are touched repeatedly, but almost
always by commits serving *one* feature that was built incrementally over weeks. In only
**four** commits does a single commit mix two unrelated concerns, and in each of those the
mixing is at **file** granularity — never inside a hunk.

So the work is overwhelmingly *fold and reorder*, not *split*. True hunk-level surgery is
needed nowhere. That makes strict diff-equivalence realistic.

The one file that genuinely mixes four unrelated concerns is `zellij-utils/src/consts.rs`,
and even there the hunks sit in disjoint line regions (§6.1).

## 3.1 Churn — added and deleted inside the range

Commits 64–66 deleted code that earlier commits in this same range introduced. Against the
**new** tip those add/delete pairs are pure noise: the minimal branch simply never adds
them, and commits 64 and 65 disappear along with them.

| item | added by | deleted by | in the minimal branch |
|---|---|---|---|
| `SessionEntry.pid` (+ kdl parse/serialize, both server writers, 4 test sites) | `ab663eba` **#1** | `e066f12a` **#65** | S1 never adds it |
| `SessionRegistry::exited_sessions()` | `ab663eba` **#1** | `01925df3` **#64** | S1 never adds it |
| `reap_stale_running_entries()` | `8e202143` **#2** | `01925df3` **#64** | S2 never adds it |
| per-call pipe enumeration in `check_session_state` | `a216b2e1` **#62** | `041e3edc` **#66** | S5 introduces `LivenessProbe` directly |

`git-overlap` reports **0 churn** because it detects churn at *file* granularity and none of
these files were deleted. Function-level churn has to be found with `git log -S`.

**This is why the diff target had to move.** Against the old 63-commit tip these three items
were live code and had to be reproduced; against the 66-commit tip they must not exist. Same
reconstruction, different contract — pin the target ref and don't let it drift.

## 4. Target commit set

Ordered. Foundational first, feature commits after, artifacts last.

| # | commit | folds |
|---|---|---|
| **S1** | `sessions: registry (sessions.kdl), decouple names from sockets` | 1, 52, 58 — **minus** `pid` and `exited_sessions` (§3.1) |
| **S2** | `sessions: 3-state liveness (Alive/Stuck/Dead) in ls` | 2 — **minus** `reap_stale_running_entries` (§3.1) |
| **S3** | `sessions: reap DEAD registry entries` | 3, 39 |
| **S4** | `sessions: case-insensitive attach, reject overlapping/non-ASCII names` | 31 |
| **S5** | `sessions: derive Dead from the pipe namespace via LivenessProbe` | 62, 66 — snapshot built in from the start |
| **S6** | `sessions: drop dead sessions from the plugin session list` | 63 *(server half)* |
| **W1** | `windows: session switching — zombie servers, pipe resolution, DACL` | 7, 26 |
| **W2** | `windows: VT reader + ANSI mouse sequences` | 10 |
| **W3** | `windows: WIN32_INPUT_MODE key encoding` | 25 |
| **W4** | `windows: sideload conpty.dll` | 24 |
| **W5** | `client: time-bound connection, break recv loop on dead socket` | 5, 43 |
| **C1** | `copymode: CopyMode with visual selection and motions` | 17, 32, 33, 34, 35, 36, 37 |
| **A1** | `plugins: agent-bar` | 20, 30, 38, 42, 44, 48, 49, 53, 56, 57, 60, 61, 63 *(plugin half)* |
| **N1** | `pane-nav: browser-style cross-session pane jumplist` | 46, 47 *(pane_nav half)* |
| **G1** | `plugins: auto-tab-name + PowerShell snippet` | 22, 47 *(ps1 half)* |
| **P1** | `tab-bar: prefix tabnames` | 16 |
| **P2** | `compact-bar: drop brand prefix` | 21 |
| **TH1** | `themes: follow Windows app theme, keep palette across reload` | 54, 55 |
| **U1** | `fix: route cli action to any client when none active` | 4 |
| **U2** | `fix: graceful exit when attaching to current session` | 6 |
| **U3** | `fix: flush session layout to disk on clean exit` *(divens)* | 8 |
| **U4** | `fix: respect --name when creating and renaming panes` *(divens)* | 9 |
| **U5** | `fix: kill by tab` | 13 |
| **U6** | `fix: restore prior float visibility when transient float closes` | 19 |
| **U7** | `fix: strip fork markers from KDL before parse` | 18 |
| **U8** | `fix(plugins): keep last-client metadata so reattach restores bars` | 23 |
| **U9** | `feat: session_name_from_cwd, picking the live session among duplicates` | 12, 28 |
| **U10** | `feat(scroll): forward keyboard scroll in the alternate screen` | 59 |
| **U11** | `debug: logging for floating panes` | 14 — see §8 |
| **B1** | `chore: install / copy / build scripts` | 11, 40, 50, 41 *(scripts half)* |
| **B2** | `feat: zellijctl, slim client-only dispatcher` | 41 *(crate half)* |
| **B3** | `chore: embed git SHA in --version, fork CLAUDE.md` | 15, 51 |
| **B4** | `chore: pre-commit wasm-rebake hook + .cargo config` | 27 |
| **ART** | `chore: rebake default-plugin wasm` | 29, 45, and every wasm rider |

66 → 33 named commits, of which 24 carry real logic. Commits **64** and **65** have no
destination — they only delete what §3.1 says never to add.

## 5. Full inventory

`R` = Rutger, `D` = divens. Position is branch order, **not** date — the branch was
reordered during earlier squashing, so author dates are misleading.

| # | sha | src | tag | → |
|---|---|---|---|---|
| 1 | `ab663eba` | R | SESSION | **S1** anchor |
| 2 | `8e202143` | R | SESSION | **S2** |
| 3 | `75e499aa` | R | SESSION | **S3** anchor |
| 4 | `07a6faac` | R | SERVER | **U1** |
| 5 | `e46ca68f` | R | CLIENT | **W5** anchor |
| 6 | `a8a3eeae` | R | CLI | **U2** |
| 7 | `08fe52d8` | R | WIN-IPC | **W1** anchor |
| 8 | `622412ad` | **D** | SERVER | **U3** |
| 9 | `26e167b6` | **D** | SERVER | **U4** |
| 10 | `93aa673b` | R | WIN-INPUT | **W2** |
| 11 | `fd7cf0ec` | R | BUILD | **B1** anchor |
| 12 | `c546c95c` | R | CLI | **U9** anchor |
| 13 | `e2a3327b` | R | CLIENT | **U5** — subject `fix kill by tab`, needs a real message |
| 14 | `488ca8ff` | R | DEBUG | **U11** — see §8 |
| 15 | `020e44a7` | R | BUILD | **B3** anchor |
| 16 | `69333ba3` | R | TABBAR | **P1** |
| 17 | `47397c9d` | R | COPYMODE | **C1** anchor |
| 18 | `facd749f` | R | KDL | **U7** |
| 19 | `bb1f0a29` | R | TAB | **U6** |
| 20 | `af4ca253` | R | AGENTBAR | **A1** anchor |
| 21 | `84059ec0` | R | TABBAR | **P2** |
| 22 | `fed9e383` | R | TABNAME | **G1** anchor |
| 23 | `aa94f576` | R | PLUGINFIX | **U8** |
| 24 | `7048e7fe` | R | WIN-IPC | **W4** |
| 25 | `9e1b8c0a` | R | WIN-INPUT | **W3** |
| 26 | `7301f064` | R | WIN-IPC | fold → **W1** |
| 27 | `14a88834` | R | BUILD | **B4** |
| 28 | `04160671` | R | CLI | fold → **U9** *(patches #12's own block — §6.3)* |
| 29 | `569cc11b` | R | ARTIFACT | **ART** |
| 30 | `6a4c749d` | R | AGENTBAR | fold → **A1** — subject `agentbar loading indication` |
| 31 | `f0f91495` | R | SESSION | **S4** |
| 32 | `1fe694cf` | R | COPYMODE | fold → **C1** |
| 33 | `20bd423f` | R | COPYMODE | fold → **C1** |
| 34 | `11cb35dc` | R | COPYMODE | fold → **C1** |
| 35 | `09644dc5` | R | COPYMODE | `fixup!` → **C1** |
| 36 | `49d5f59e` | R | COPYMODE | `fixup!` → **C1** |
| 37 | `0d390294` | R | COPYMODE | `fixup!` → **C1** |
| 38 | `e0c7c1a5` | R | AGENTBAR | fold → **A1** — subject `wfwf` |
| 39 | `7de017ae` | R | SESSION | fold → **S3** — subject `clear stale sessions` |
| 40 | `ceb06ca6` | R | BUILD | fold → **B1** |
| 41 | `4e46323b` | R | BUILD | **split** → **B2** + **B1** *(§6.4)* |
| 42 | `c52daca1` | R | AGENTBAR | fold → **A1** — subject `c` |
| 43 | `962ac334` | R | CLIENT | fold → **W5** |
| 44 | `6a6bc2da` | R | AGENTBAR | fold → **A1** |
| 45 | `19d99af8` | R | ARTIFACT | **ART** |
| 46 | `c2aca9e4` | R | PANENAV | **N1** anchor |
| 47 | `6d95f98b` | R | mixed | **split** → **N1** + **G1** *(§6.4)* — subject `naav` |
| 48 | `397aa90a` | R | AGENTBAR | fold → **A1** |
| 49 | `a17317d5` | R | AGENTBAR | fold → **A1** |
| 50 | `a0aead0a` | R | BUILD | fold → **B1** |
| 51 | `fd128b17` | R | BUILD | fold → **B3** — subject `w` |
| 52 | `e9553649` | R | SESSION | `fixup!` → **S1** |
| 53 | `e3589643` | R | AGENTBAR | fold → **A1** — subject `bg agents` |
| 54 | `4509bfe5` | R | THEME | fold → **TH1** |
| 55 | `c8c15fe4` | R | THEME | **TH1** anchor — **verify order, §8** |
| 56 | `4b5e4e43` | R | AGENTBAR | fold → **A1** |
| 57 | `72989015` | R | AGENTBAR | fold → **A1** |
| 58 | `05a99dd8` | R | TESTFIX | fold → **S1** *(§6.2)* |
| 59 | `8a172c0f` | R | TAB | **U10** |
| 60 | `87485681` | R | AGENTBAR | fold → **A1** |
| 61 | `1a5b9d81` | R | AGENTBAR | fold → **A1** |
| 62 | `a216b2e1` | R | SESSION | **S5** |
| 63 | `6a31fade` | R | mixed | **split** → **S6** + **A1** *(§6.4)* |
| 64 | `01925df3` | R | CLEANUP | **dropped** — deletes #1's `exited_sessions` + #2's `reap_stale_running_entries` |
| 65 | `e066f12a` | R | CLEANUP | **dropped** — deletes #1's `pid` field |
| 66 | `041e3edc` | R | PERF | fold → **S5** — `LivenessProbe`, hoists the snapshot out of 8 loops |

## 6. Shared files

### 6.1 `zellij-utils/src/consts.rs` — 6 touches, 4 concerns, disjoint regions

The textbook case. Hunks never collide, so the split is mechanical:

| commit | region | purpose | → |
|---|---|---|---|
| 1 | `@@ +40,30` after `session_info_folder_for_session`, `@@ +139,2` in `lazy_static!` | `SESSION_ID_LENGTH`, `check_sock_dir_length`, `ZELLIJ_SESSIONS_KDL` | S1 |
| 7 | `ipc_connect`, `ipc_bind`, `ipc_bind_async`, `ipc_connect_reply` | marker file + `resolve_pipe_name` | W1 |
| 26 | `ipc_bind`, `ipc_bind_async`, `ipc_bind_reply` | security descriptor | W1 |
| 15 | `@@ +14,7` beside `VERSION` | git-SHA const | B3 |
| 20 | `@@ +201` in `mod not_wasm` | register `agent-bar.wasm` | A1 |
| 22 | `@@ +214` in `mod not_wasm` | register `auto-tab-name.wasm` | G1 |

7 and 26 both touch `ipc_bind` — they land in the same destination (**W1**), so the
interleave resolves itself. 20 and 22 are one line each in the same asset list but at
different offsets; they must stay in that order.

### 6.2 `zellij-server/src/unit/screen_tests.rs` — a hidden straggler

Touched by 1, 9, 58. **#58 (`pass session_id to Screen::new in remaining test call sites`)
is a late fixup for #1** — the registry changed `Screen::new`'s signature and #58 caught the
call sites that were missed. It is not independent work; fold into **S1**.

### 6.3 `src/commands.rs` — 6 touches, one non-obvious dependency

| commit | region | → |
|---|---|---|
| 2 | import block, `attach_with_session_name`, `watch_session` | S2 |
| 3 | `delete_all_sessions` | S3 |
| 6 | `start_client @@ -873` | U2 |
| 12 | `start_web_server`, `find_indexed_session`, `attach_with_session_*`, `start_client @@ +975,17` | U9 |
| 28 | `start_client @@ -978,3 +978,34` | **U9** |
| 31 | `attach_with_session_name @@ -644,4` | S4 |

**#28 edits lines 978–981, inside the block #12 added at 975–992.** It is a refinement of
the `session_name_from_cwd` feature, *not* of the session registry — despite its
`feat(sessions):` subject. This is the single mis-grouping in the branch, and it means #28
**cannot** be reordered before #12.

### 6.4 The four genuine splits — all file-level, no hunk surgery

| commit | files → destination |
|---|---|
| 41 `zellijctl` | `zellijctl/**`, `Cargo.toml`, `Cargo.lock` → **B2** · `install.ps1`, `copy.ps1` → **B1** |
| 47 `naav` | `zellij-server/src/pane_nav.rs` → **N1** · `auto-tab-name/shell/auto-tab-name.ps1` → **G1** |
| 63 | `zellij-server/src/background_jobs.rs` → **S6** · `default-plugins/agent-bar/src/main.rs` → **A1** |
| 17 `CopyMode` | 13 `.wasm` riders → **ART** |

`git-split-fixup.sh` handles exactly this shape — it groups a commit's files by each file's
most-recent prior commit and rewrites as `fixup!` commits.

### 6.5 Single-cluster hot files — fold only, no split

All touches serve one feature; the whole contribution goes to one destination.

| file | touches | → |
|---|---|---|
| `default-plugins/agent-bar/src/main.rs` | 13 | A1 |
| `zellij-server/src/panes/grid.rs` | 8 | C1 *(7)* + W3 *(#25)* |
| `zellij-utils/src/sessions.rs` | 9 | S1–S5 — 3 of the 9 are the §3.1 deletions and vanish |
| `zellij-utils/assets/config/default.kdl` | 7 | C1 *(6)* + G1 *(#22)* |
| `zellij-server/src/screen.rs` | 9 | S1, S4, C1, N1, TH1 — disjoint regions, verify at execution |
| `zellij-server/src/lib.rs` | 5 | S1 *(start_server/init_session)*, U1 *(SessionState)*, W1 *(+289 module)*, U3 *(Drop)*, N1 |
| `zellij-utils/src/input/actions.rs` | 7 | U4, C1, N1 |
| `zellij-server/src/route.rs` | 6 | U1, U4, C1, N1 |
| `zellij-utils/src/kdl/mod.rs` | 6 | U9, C1, U7, N1 |
| `zellij-utils/src/ipc/protobuf_conversion.rs` | 6 | U4, U9, C1, N1 |
| `zellij-utils/src/plugin_api/action.rs` | 5 | U4, C1, N1 |

### 6.6 Artifacts — the largest single source of noise

15 `.wasm` files account for **69 of the overlapping touches**: `agent-bar.wasm` alone is
rebaked 11 times. They are pure build output of `xtask ci build-release`.

**Policy: one `ART` commit at the tip**, holding the final state of every `.wasm`. Feature
commits carry no artifacts. This is the single biggest reduction in the matrix and cannot
affect diff-equivalence, since only the tip state is compared.

Same treatment for `zellij-utils/assets/prost*` (generated) — 3 touches.

`Cargo.lock` (6) and `Cargo.toml` (6) stay with their feature commits: each adds its own
dependency, and the hunks are naturally disjoint. Do **not** consolidate — a `Cargo.toml`
without its dep breaks the build at that commit.

## 7. Execution

Build forward from `e9173cba` rather than rewriting in place — with 33 targets out of 63
sources and 4 splits, `rebase -i` reordering is more error-prone than replay.

1. `git branch backup/atomize-pre-cleanup atomize` — **done**
2. Cherry-pick each destination's sources in order, `--no-commit`, then one commit per target
   — for S1, S2 and S5, take the **post-cleanup** shape from `atomize`, not the original commit
3. For the §6.4 splits, `git checkout <sha> -- <paths>` per destination
4. Skip all `.wasm` and `assets/prost*` throughout; add them once as **ART** at the tip
5. After each target: `cargo check` (fast) — the tree must stay buildable commit-by-commit

## 8. Verification — non-negotiable

```bash
git diff backup/atomize-target e9173cba-minimal        # MUST be empty
git diff --stat backup/atomize-target e9173cba-minimal # MUST print nothing
```

`backup/atomize-target` is pinned at `041e3edc`. `backup/atomize-pre-cleanup` (`6a31fade`)
is the older 63-commit tip — **not** a valid target any more, since §3.1's three items still
exist there.

A non-empty diff means content changed, not just history. Investigate before continuing —
do not "fix it up" at the tip.

Build checkpoints: **S1** (the registry, everything leans on it), **C1**, and the tip.

```bash
git worktree add -q --detach /tmp/wt <sha> && ( cd /tmp/wt && cargo check ) ; git worktree remove --force /tmp/wt
```

Keep both backup refs until the diff is verified empty.

## 9. Decisions deferred to after a verified-empty diff

Each of these changes content, so none belong in the reconstruction.

- **#14 `debug logging for floating panes`** — debug logging in `stdin_handler.rs`,
  `floating_panes/mod.rs`, `tab/mod.rs`. Behaviour-equivalent to drop, but *not*
  diff-equivalent. Listed as **U11** so the diff stays clean; drop it in a follow-up if the
  logging has served its purpose.
- **Junk subjects** — `wfwf`, `c`, `naav`, `w`, `bg agents`, `clear stale sessions`,
  `agentbar loading indication`, `fix kill by tab`. All fold into anchors, so most vanish;
  #13 survives as **U5** and needs a real message.
- **Collapse the DEAD machinery** — S2/S3 and part of S4 exist only because liveness was
  *recorded* rather than derived. With S5 in place, `Dead` becomes unrepresentable and
  ~350 lines could go. Content change: separate commit, after the rebase.
- **Registry / kdl schema reshape** — deliberately deferred. Old state can be wiped, so
  back-compat is not a constraint: `SessionState::Exited` degrades to a "don't bother
  probing" hint, and the legacy socket-file migration that manufactures Exited rows can go
  with it.
- **Marker file removal** — its path duplicates its own filename and its mtime duplicates the
  registry's `created_at`. *(Its PID field is already gone — commit 65.)*

**Done since the first draft** — struck from this list, now reflected in §3.1:

- ~~unreachable `reap_stale_running_entries` / `exited_sessions`~~ → `01925df3`
- ~~unread `SessionEntry.pid`~~ → `e066f12a`
- ~~`check_session_state` enumerating the namespace per call~~ → `041e3edc`
- **TH1 ordering** — #54 (*keep palette across reload*) precedes #55 (*follow Windows app
  theme*) in branch order, i.e. the fix lands before the feature it fixes. Confirm which
  order actually compiles before folding.
- **Rebase onto `0ed2edea`** — 40 upstream commits ahead; 24 real source conflicts by trial
  merge, none in the session stack. Independent of this reconstruction.
