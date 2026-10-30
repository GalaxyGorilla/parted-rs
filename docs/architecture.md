# Architecture

The project will separate disk formats from operating-system access:

- `device`: bounds-checked image and block-device I/O, locking, size discovery,
  mounted-device detection, syncing, and kernel partition-table refresh.
- `label`: disk-label detection and a common validated partition model.
- `mbr`: strict handling of the four primary entries while preserving bootstrap
  code and disk identity. Extended/logical entries are rejected.
- `gpt`: protective MBR, primary/backup headers, entry arrays, and CRC32.
- `plan`: deterministic label, create, delete, and end-only resize plans.
- `transaction`: metadata backup, ordered writes, interruption recovery, and
  post-commit verification.
- `cli`: human-readable and JSON interfaces over the library.

Parsing remains read-only. Mutation requires an explicit plan whose bounds and
preconditions can be reviewed before opening the target writable.

## Commit protocol

The writable device is exclusively locked and refused when it or a child
partition appears in `/proc/self/mountinfo`. Before the first device write,
every logical sector belonging to the current table or touched by the new plan
is copied into a geometry-bound, CRC32-protected recovery file. Both that file
and its parent directory are synced. The backup also records the target's Linux
device identity (device/inode for images or block-device number), so restore
refuses another same-sized target.

GPT plans order the backup entry array and backup header first, then the
protective MBR, primary array, and primary header. Each phase is synced. All
planned bytes are read back before requesting a Linux kernel partition-table
reread. `restore` validates recovery-file geometry and checksums before writing.

Recovery files are versioned operational artifacts bound to the target's
identity and geometry. Keep the executable that created a backup until that
backup is no longer needed.
