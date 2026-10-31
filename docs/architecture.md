# Architecture

The project separates disk formats from operating-system access:

- `device`: bounds-checked image and block-device I/O, locking, size discovery,
  mounted-device detection, syncing, and kernel partition-table refresh.
- `model`: the common validated disk, partition, and LBA-range model.
- `mbr`: strict handling of the four primary entries while preserving bootstrap
  code and disk identity. Extended/logical entries are rejected.
- `gpt`: protective MBR, primary/backup headers, entry arrays, and CRC32.
- `plan`: deterministic label, create, delete, and end-only resize plans.
- `transaction`: metadata backup, ordered writes, interruption recovery, and
  post-commit verification.
- `main`: human-readable and JSON command-line interfaces over the library.

Parsing remains non-mutating. For a real write, the target is opened and locked
before planning so another cooperating editor cannot enter between inspection
and commit. Mutation requires a sealed plan bound to the target identity,
geometry, and exact original bytes of every touched metadata range.

## Commit protocol

Writable block devices are opened with a Linux exclusive claim and also held
with an advisory lock. The target is refused when it or a child partition is
mounted, used as swap, or has kernel storage holders. These checks run at open
and again after confirmation and backup creation. Before the first device write,
every logical sector belonging to the current table or touched by the new plan
is copied into a geometry-bound, CRC32-protected recovery file. Both that file
and its parent directory are synced. The backup also records the target's Linux
device identity (device/inode for images or block-device number), so restore
refuses another same-sized target.

GPT plans order the backup entry array and backup header first, then the
protective MBR, primary array, and primary header. Each phase is synced. All
planned bytes are read back before requesting a Linux kernel partition-table
reread. A reread failure is reported as a committed result with a warning,
never as an ambiguous pre-commit failure. `restore` validates recovery-file
geometry and checksums, then syncs and verifies every ordered recovery phase.

Recovery files are versioned operational artifacts bound to the target's
identity and geometry. Keep the executable that created a backup until that
backup is no longer needed.
