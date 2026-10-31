# Testing parted

Partition-table software must prove refusal and recovery behavior, not only
successful edits.

| Layer | Evidence |
| --- | --- |
| Format parsers | MBR and GPT at 512/4096-byte sectors, CRCs, redundant headers, overlap, bounds, duplicate GUID and malformed-field rejection |
| Planning | Read-only creation, removal, resize, type/name/attribute changes, GPT-to-MBR cleanup and stale metadata detection |
| Transactions | Ordered, synced GPT phases; durable backups; failure before every write phase; exact restoration |
| Adversarial recovery | Whole-manifest checksum, redirected-LBA regression, overlapping records, wrong target identity and every truncation point |
| CLI | Human and JSON workflows, dry runs, restore, option conflicts and exact units |
| Independent interoperability | Reciprocal GPT checks with `sfdisk`, plus MBR output checked by `sfdisk` |

## Release gate

```sh
cargo fmt --check
cargo test --release --all-targets --locked
cargo clippy --all-targets -- -D warnings
cargo deny check
cargo package --locked
cargo build --release --locked
scripts/interop.sh
```

The interoperability script exits with status 77 when `sfdisk` is unavailable.
CI installs the required package, so a skipped interoperability run is a
failure there.

## Remaining hardware boundary

Image tests cannot emulate every Linux block-device state. Release candidates
still need disposable-device tests for mounts, swap, device-mapper/RAID holders,
exclusive-open contention, 4K logical sectors, kernel table rereads, I/O errors
and abrupt power loss. The repository documents this boundary instead of
turning missing hardware into a silent test skip.
