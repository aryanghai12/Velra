# Golden snapshots

Byte-exact expected output, reviewed by humans and compared in CI.

| Directory | Contents |
|---|---|
| `capsule/` | `insta` snapshots of the Continuation Capsule for each fixture state (E1). Identical on macOS, Linux and Windows — tests run with `TZ=UTC` and render `\n` line endings and `/` path separators. |
| `settings/` | Expected `settings.json` bytes after `velra enable` for each input fixture in `tests/fixtures/settings/` (A2), including files with comments, trailing commas, tab indentation and pre-existing hooks. |

## Updating

A capsule change is a user-visible change to what the agent sees after
compaction. Review the diff before accepting:

```sh
cargo test --workspace                # writes *.snap.new on mismatch
cargo insta review                    # accept or reject each change
```

If a snapshot changes, say why in the commit message, and bump
`render_version` in `crates/velra-core/src/render.rs` when the capsule's
*structure* changes (not merely its content), so stored checkpoints stay
interpretable.
