# Compatibility

## Implemented

- Legacy MBR signature and four primary 16-byte entries.
- MBR boot flag, type byte, exact 32-bit start and sector count.
- UEFI GPT revision 1.0 primary and backup headers, header and entry-array
  CRC32, protective MBR, GUID byte ordering, attributes, and UTF-16 names.
- 512-byte and 4096-byte logical sectors.
- Regular images and Linux block devices.
- Exact partition creation, removal, end-only resize, type changes, GPT names,
  MBR boot flags, raw GPT attribute-bit changes, and free-space reporting.
- Inclusive sector positions and exact `[START, END)` byte-unit boundaries,
  interoperable with GNU Parted for tested primary MBR and GPT images.
- Human-readable and single-document JSON output.
- GPT disk GUIDs, partition unique GUIDs, attributes, and names in
  machine-readable output.

## Rejected instead of guessed

Malformed signatures, CRCs, header pairs, UTF-16, partially empty MBR entries,
overlap, out-of-device ranges, extended MBR types, hybrid GPT MBRs, non-exact
units, and unsupported logical sector sizes are errors. The tool does not
silently repair or normalize them.

Command compatibility covers the documented command set, not the complete GNU
Parted command language. In particular, this tool accepts one command per
invocation and uses exact `mkpart START END` positions. Unsupported commands,
layouts, approximate positions, and ambiguous inputs are rejected explicitly.

## GNU Parted compatibility at a glance

This project shares a binary name and several command names with GNU Parted,
but it is not currently a drop-in replacement. GNU Parted behavior also varies
by release; this table describes the commonly used interface rather than
promising compatibility with a particular version.

| Area | `parted-rs` | Common GNU Parted behavior |
| --- | --- | --- |
| Partition tables | MBR and GPT | MBR/GPT plus several legacy disk labels |
| MBR layout | Four primary entries | Primary, extended and logical partitions |
| Invocation model | One command per process | Multiple commands and an interactive shell |
| Partition creation | `mkpart START END` plus explicit options | Positional partition kind, name and filesystem hint |
| Positions | Exact sectors or binary byte units | Also percentages, negative positions and approximate ranges |
| Alignment | Caller supplies exact boundaries | Minimal/optimal alignment policies and `align-check` |
| Flags | MBR boot bit and raw GPT attribute bits | Semantic flags such as ESP, BIOS-GRUB, RAID, LVM and swap |
| Device discovery | One explicit target | Listing and selecting devices |
| Recovery | Pre-write metadata backup and exact restore | Lost-partition rescue scanning |
| Output | Project-specific human and JSON documents | GNU human, machine and version-dependent JSON formats |
| Filesystem knowledge | Partition metadata only | Filesystem probing and filesystem-type hints |

## Unsupported operations and syntax

The following are intentionally rejected rather than approximated:

- Extended/logical MBR partitions and hybrid GPT/MBR layouts.
- Percentage positions such as `100%`, negative positions relative to the end,
  decimal SI units, and fuzzy boundary selection.
- Automatic `minimal` or `optimal` alignment and alignment-check commands.
- Semantic partition flags. GPT attributes can be changed by raw bit number,
  but names such as `esp`, `bios_grub`, `raid`, `lvm` and `swap` are not mapped.
- Multiple commands in one invocation, an interactive command shell, device
  selection, device enumeration and lost-partition rescue.
- GNU Parted's exact text, machine-output and JSON schemas.
- Filesystem creation, movement, repair or resizing. `resizepart` changes only
  the partition end and never moves data.

Additional disk-label families, CHS-oriented behavior, and platform-specific
backends outside Linux are also outside the current scope.

## Script migration

Do not replace GNU Parted with this binary in an existing script without
reviewing every invocation and parsing rule. For example, this common shape is
not accepted:

```text
parted -s disk.img mkpart primary ext4 1MiB 100%
```

The explicit equivalent, when the intended end is exactly 32 MiB, is:

```text
parted --script disk.img mkpart 1MiB 32MiB --type TYPE-GUID --name data
```

Scripts should use `--json`, parse the documented project schema, provide exact
boundaries, and invoke one command at a time. Unsupported input fails before a
write; it is never silently rounded or translated.

## Compatibility priorities

If broader compatibility is added, the highest-value sequence is:

1. Read-only discovery of extended/logical MBR chains, followed by safe edits.
2. Percentage/end-relative positions with explicit alignment rules.
3. Semantic GPT/MBR flag names.
4. GNU-style `mkpart` aliases and command chaining.
5. Device listing, alignment checks and rescue workflows.

These are roadmap priorities, not claims about current behavior.
