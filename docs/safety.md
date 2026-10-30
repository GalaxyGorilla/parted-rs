# Safety model

Partition-table edits can make all data on a device inaccessible. A recovery
backup protects partition-table sectors only; it does not back up partition
contents.

Writable Linux block devices are refused when the target or a child device is
mounted, and an advisory exclusive lock is held for planning and commit. This
reduces races but cannot control software that ignores advisory locks. Stop
storage managers, swap, RAID, encryption mappings, and other users before
editing.

Interactive writes require the exact response `yes`. `--script` authorizes an
unattended write. Use `--dry-run` to inspect exact LBA writes without opening
the target writable.

Do not delete the reported recovery file until the resulting table has been
checked. After an interrupted or failed commit, run:

```text
parted --script DEVICE restore BACKUP
```

Restore requires the original block device or image-file identity, byte size,
and logical sector size. This prevents a backup from being applied to another
same-sized target; moving or replacing an image file also changes its identity.
Restore writes metadata only. This tool never resizes a filesystem and never
moves partition contents; shrinking a partition can hide or destroy filesystem
data.
