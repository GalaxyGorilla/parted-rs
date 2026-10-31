# Safety model

Partition-table edits can make all data on a device inaccessible. A recovery
backup protects partition-table sectors only; it does not back up partition
contents.

Writable Linux block devices are opened with an exclusive kernel claim and an
advisory lock. The target and its child partitions are checked for mounts, swap,
and kernel storage holders at open and again immediately before writing.
Plans also record the exact original metadata and are refused if it changes
after planning. These layers substantially reduce races, but cannot prove that
unknown userspace software has stopped issuing raw I/O. Stop storage managers
and all other users before editing.

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
same-sized target. Renaming an image on the same filesystem preserves its
identity; copying it creates a new identity and is refused. Block-device
identity is the Linux device number, so replacing hardware behind the same
device node is not independently detectable.
Restore writes metadata only. This tool never resizes a filesystem and never
moves partition contents; shrinking a partition can hide or destroy filesystem
data.
