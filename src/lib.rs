//! Clean-room, std-only MBR and UEFI GPT partitioning primitives.

pub mod device;
pub mod gpt;
pub mod mbr;
pub mod model;
pub mod plan;
pub mod transaction;

mod util;

use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(String),
    Refused(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Invalid(s) | Self::Refused(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub const HELP: &str = "\
parted - exact, recoverable MBR/GPT partition table editor

Usage:
  parted [OPTIONS] DEVICE print [free]
  parted [OPTIONS] DEVICE mklabel mbr|gpt
  parted [OPTIONS] DEVICE mkpart START END [--type GUID] [--name NAME]
  parted [OPTIONS] DEVICE rm NUMBER
  parted [OPTIONS] DEVICE resizepart NUMBER END
  parted [OPTIONS] DEVICE name NUMBER NAME
  parted [OPTIONS] DEVICE type NUMBER TYPE
  parted [OPTIONS] DEVICE set NUMBER boot on|off
  parted [OPTIONS] DEVICE set NUMBER attribute BIT on|off
  parted [OPTIONS] DEVICE restore BACKUP

Global options:
  -s, --script       Never prompt (required for unattended writes)
      --json         JSON output
      --dry-run      Print the plan without writing
      --unit UNIT    b, s, KiB, MiB, GiB, TiB (default: s)
      --backup PATH  Recovery backup path for a write
      --sector-size  Image logical sector size: 512 or 4096 (default: 512)
  -h, --help         Display this help
  -V, --version      Display version information

Sector-unit ranges use inclusive LBAs. Byte-unit ranges use exact [START, END)
boundaries, so an END of 1MiB ends at LBA 2047 on 512-byte sectors. Bare
numbers use --unit. No filesystem is resized and partition contents are never
moved.
";
