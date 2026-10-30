# Clean-room policy

Implementation work uses public format specifications, public command
documentation, and independently created test images and observations.

Do not inspect, copy, translate, or adapt GNU Parted implementation source.
Do not import source-derived tests or fixtures without a separate provenance
and license review.

Primary references will include:

- UEFI specifications for GPT;
- public IBM PC/AT and Microsoft documentation for MBR/EBR layouts;
- Linux kernel userspace ABI documentation for block-device operations; and
- public command manuals solely for externally visible compatibility.

Every compatibility test should record its public evidence and use an
independently generated input.

## Compatibility observations

- 2026-10-30: GNU Parted 3.6 was invoked only as a command-line black box
  against independently generated 64 MiB images. A GPT partition requested as
  `1MiB 32MiB` occupied LBAs 2048 through 65535 on 512-byte sectors. The Rust
  implementation uses the same exact `[START, END)` byte-boundary convention.
  Reciprocal GPT and primary-MBR images were accepted and reported with
  matching LBAs by both tools. No GNU Parted source or source-derived fixture
  was inspected.
