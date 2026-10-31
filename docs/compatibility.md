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
