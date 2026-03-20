# README CLI Fixtures

These `trycmd` fixtures validate canonical command examples documented in
`README.md`.

If a documented example changes, update both `README.md` and these fixtures in
the same commit.

For hand-crafted `.gcl` fixtures, compute `snapshot-hash` from the entry data
rather than using placeholder values. The test helper pattern in
`src/lib.rs` (`symlink_snapshot_hash`) shows the exact length-prefixed hashing
scheme used for symlink-only fixtures.
