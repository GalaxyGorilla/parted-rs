# parted

A clean-room, Rust-only MBR/GPT partition editor licensed under MIT OR
Apache-2.0—a permissive alternative to GNU Parted.

## Capabilities

`parted` validates, plans, and transactionally edits image files and Linux
block devices. It provides exact byte/sector boundaries, recoverable writes,
machine-readable output, four-primary-entry MBR, and redundant UEFI GPT on
512-byte and 4096-byte logical sectors. Runtime code uses Rust's standard
library only.

The implementation is built from public disk-format specifications. GPL source
code was not inspected, copied, translated, or used as implementation guidance.

## Engineering guarantees

- Strict MBR/GPT parsing, validation, creation, and modification.
- Exact byte/sector sizing without approximate constraint solving.
- Bounds-checked image-file and Linux block-device I/O.
- Rust-only parsing and mutation logic.
- Exclusive Linux block-device claims plus explicit refusal of mounted, swap,
  kernel-mapped, malformed, overlapping, or out-of-bounds layouts.
- Sealed plans bound to the target identity, geometry, and exact metadata
  preimage; a table changed after planning is never overwritten.
- Recoverable write ordering, durable synchronization, and read-back
  verification for every commit.
- Reusable library and scriptable command-line interfaces.

## Commands

```text
parted [OPTIONS] DEVICE print [free]
parted [OPTIONS] DEVICE mklabel mbr|gpt
parted [OPTIONS] DEVICE mkpart START END [--type TYPE] [--name NAME]
parted [OPTIONS] DEVICE rm NUMBER
parted [OPTIONS] DEVICE resizepart NUMBER END
parted [OPTIONS] DEVICE name NUMBER NAME
parted [OPTIONS] DEVICE type NUMBER TYPE
parted [OPTIONS] DEVICE set NUMBER boot on|off
parted [OPTIONS] DEVICE set NUMBER attribute BIT on|off
parted [OPTIONS] DEVICE restore BACKUP
```

Sector-unit ranges use inclusive LBAs. Byte-unit ranges use exact
`[START, END)` boundaries: for example, `1MiB 32MiB` starts at LBA 2048 and
ends at LBA 65535 on 512-byte sectors, never one sector beyond 32 MiB.
`--unit` accepts `s`, `b`, `KiB`, `MiB`, `GiB`, or `TiB`; values must convert
to a whole logical sector exactly. Global options include `--script`, `--json`,
`--dry-run`, `--backup PATH`, and `--sector-size 512|4096` for images.
Interactive writes require typing `yes`; `--script` is the explicit
unattended-write authorization.

Every commit creates a new, checksummed recovery file before touching the
partition table. GPT backup metadata is committed before primary metadata.
Writes and restores are synced and read back by phase. Recovery files are bound to the
original block device or image-file identity as well as its geometry, preventing
accidental restore to another same-sized target. Keep the reported recovery
file until the new table has been independently validated.

If the on-disk commit succeeds but Linux cannot immediately reread the table,
the command reports that state distinctly, includes the recovery path, and
prints the kernel error. The disk is already changed; close users of the device
and reread the table or reboot before continuing.

## Safe first run

Use a disposable image before touching a block device:

```sh
truncate -s 64MiB demo.img
cargo run --release -- --script demo.img mklabel gpt
cargo run --release -- --script demo.img mkpart 1MiB 32MiB --name data
cargo run --release -- demo.img print free
```

Each write reports a recovery file. The second command can be reversed with
`parted --script demo.img restore PATH-REPORTED-BY-MKPART`.

## Supported scope

| Area | Behavior |
| --- | --- |
| Partition tables | Four-primary-entry MBR and UEFI GPT revision 1.0 |
| GPT layout | 128 entries of 128 bytes with primary and backup metadata |
| Logical sectors | 512 and 4096 bytes |
| Image files | 512 bytes by default; select 4096 with `--sector-size` |
| Block devices | Logical sector size discovered through the Linux kernel ABI |
| Filesystems | Partition-table editing only; filesystems are never resized or moved |
| Excluded layouts | Extended/logical MBR, hybrid GPT, and CHS geometry |
| Position syntax | Exact sectors or bytes; approximate percentages are rejected |
| GPT attributes | Raw attribute bits 0-63 can be displayed and changed explicitly |

If the kernel cannot reread a table because another process keeps the device
busy, the verified on-disk update and durable recovery backup remain intact.

## Rust-only policy

Production code must not link to GNU Parted, libfdisk, or other native
partitioning libraries. Dependencies, if introduced, must be implemented in
Rust and licensed under approved permissive terms. The policy is enforced by
`cargo-deny`.

Direct operating-system calls through Rust FFI are allowed when Rust's standard
library does not expose the required block-device operation.

## Building

```sh
cargo build --release --locked
cargo test --release --all-targets --locked
cargo clippy --all-targets -- -D warnings
cargo deny check
```

The production binary is `target/release/parted`. To install it into Cargo's
binary directory:

```sh
cargo install --path . --locked
```

## Documentation

- [Safety model](docs/safety.md)
- [Compatibility and deliberate limits](docs/compatibility.md)
- [Architecture and commit protocol](docs/architecture.md)
- [Clean-room development policy](docs/clean-room.md)
- [Security reporting](SECURITY.md)

## License

Licensed under either the Apache License, Version 2.0 or the MIT License, at
your option.
