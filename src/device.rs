use crate::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const O_EXCL: i32 = 0x80;
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;
const LOCK_UN: i32 = 8;
const BLKSSZGET: usize = 0x1268;
const BLKGETSIZE64: usize = 0x8008_1272;
const BLKRRPART: usize = 0x125f;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn ioctl(fd: i32, request: usize, ...) -> i32;
}

#[derive(Debug)]
pub struct Device {
    file: File,
    path: PathBuf,
    size: u64,
    logical_block_size: u32,
    block_device: bool,
    locked: bool,
    writable: bool,
}

impl Device {
    pub fn open(path: impl AsRef<Path>, writable: bool, image_sector: Option<u32>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let path_metadata = std::fs::metadata(&path)?;
        let block_device = path_metadata.file_type().is_block_device();
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        if writable && block_device {
            // Linux gives an O_EXCL block-device opener an exclusive claim. Unlike
            // flock, this also blocks mounting while this descriptor remains open.
            options.custom_flags(O_EXCL);
        }
        let file = options.open(&path).map_err(|error| {
            if writable && block_device {
                Error::Refused(format!(
                    "cannot open block device exclusively; it may be mounted or busy: {error}"
                ))
            } else {
                Error::Io(error)
            }
        })?;
        let metadata = file.metadata()?;
        let block_device = metadata.file_type().is_block_device();
        if !block_device && !metadata.file_type().is_file() {
            return Err(Error::Refused(
                "device must be a regular file or Linux block device".into(),
            ));
        }
        if block_device && image_sector.is_some() {
            return Err(Error::Invalid(
                "--sector-size is only valid for image files".into(),
            ));
        }

        let (size, logical_block_size) = if block_device {
            (
                block_size_bytes(file.as_raw_fd())?,
                logical_sector(file.as_raw_fd())?,
            )
        } else {
            (metadata.len(), image_sector.unwrap_or(512))
        };
        if !matches!(logical_block_size, 512 | 4096) {
            return Err(Error::Refused(format!(
                "unsupported logical block size {logical_block_size}"
            )));
        }
        if size == 0 || size % u64::from(logical_block_size) != 0 {
            return Err(Error::Invalid(
                "device size is zero or not a whole number of logical blocks".into(),
            ));
        }
        let mut device = Self {
            file,
            path,
            size,
            logical_block_size,
            block_device,
            locked: false,
            writable,
        };
        if writable {
            let result = unsafe { flock(device.file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if result != 0 {
                return Err(Error::Refused(format!(
                    "cannot acquire exclusive lock: {}",
                    io::Error::last_os_error()
                )));
            }
            device.locked = true;
            device.ensure_safe_to_write()?;
        }
        Ok(device)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn block_size(&self) -> u32 {
        self.logical_block_size
    }

    pub fn blocks(&self) -> u64 {
        self.size / u64::from(self.logical_block_size)
    }

    pub(crate) fn identity(&self) -> Result<(u64, u64)> {
        let metadata = self.file.metadata()?;
        if self.block_device {
            Ok((u64::MAX, metadata.rdev()))
        } else {
            Ok((metadata.dev(), metadata.ino()))
        }
    }

    pub fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        self.check_bounds(offset, buffer.len())?;
        self.file.read_exact_at(buffer, offset)?;
        Ok(())
    }

    pub fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::Refused("device was opened read-only".into()));
        }
        self.check_bounds(offset, buffer.len())?;
        self.file.write_all_at(buffer, offset)?;
        Ok(())
    }

    pub fn read_lba(&self, lba: u64, count: u64) -> Result<Vec<u8>> {
        let block = u64::from(self.logical_block_size);
        let offset = lba
            .checked_mul(block)
            .ok_or_else(|| Error::Invalid("LBA offset overflow".into()))?;
        let len = count
            .checked_mul(block)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| Error::Invalid("LBA length overflow".into()))?;
        let mut bytes = vec![0; len];
        self.read_at(offset, &mut bytes)?;
        Ok(bytes)
    }

    pub fn write_lba(&self, lba: u64, bytes: &[u8]) -> Result<()> {
        if !bytes.len().is_multiple_of(self.logical_block_size as usize) {
            return Err(Error::Invalid("write is not logical-block aligned".into()));
        }
        let offset = lba
            .checked_mul(u64::from(self.logical_block_size))
            .ok_or_else(|| Error::Invalid("LBA offset overflow".into()))?;
        self.write_at(offset, bytes)
    }

    pub fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    pub fn reread_partitions(&self) -> Result<()> {
        if self.block_device {
            let result = unsafe { ioctl(self.file.as_raw_fd(), BLKRRPART) };
            if result != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_safe_to_write(&self) -> Result<()> {
        if !self.writable {
            return Err(Error::Refused("device was opened read-only".into()));
        }
        if !self.block_device {
            return Ok(());
        }
        let dev = self.file.metadata()?.rdev();
        if is_mounted(dev)? {
            return Err(Error::Refused(
                "refusing to write a mounted device or a device with mounted partitions".into(),
            ));
        }
        if is_swap(dev)? {
            return Err(Error::Refused(
                "refusing to write a device used for swap".into(),
            ));
        }
        if has_holders(dev)? {
            return Err(Error::Refused(
                "refusing to write a device used by a kernel storage mapping".into(),
            ));
        }
        Ok(())
    }

    fn check_bounds(&self, offset: u64, len: usize) -> Result<()> {
        let end = offset
            .checked_add(len as u64)
            .ok_or_else(|| Error::Invalid("I/O range overflow".into()))?;
        if end > self.size {
            return Err(Error::Invalid(format!(
                "I/O range {offset}..{end} exceeds device size {}",
                self.size
            )));
        }
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.locked {
            unsafe {
                flock(self.file.as_raw_fd(), LOCK_UN);
            }
        }
    }
}

fn logical_sector(fd: RawFd) -> Result<u32> {
    let mut value = 0_u32;
    if unsafe { ioctl(fd, BLKSSZGET, &mut value) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(value)
}

fn block_size_bytes(fd: RawFd) -> Result<u64> {
    let mut value = 0_u64;
    if unsafe { ioctl(fd, BLKGETSIZE64, &mut value) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(value)
}

fn major_minor(dev: u64) -> (u64, u64) {
    let major = (dev >> 8) & 0xfff | ((dev >> 32) & 0xffff_f000);
    let minor = dev & 0xff | ((dev >> 12) & 0xffff_ff00);
    (major, minor)
}

fn is_mounted(dev: u64) -> Result<bool> {
    let (major, minor) = major_minor(dev);
    let sys = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    let target = std::fs::canonicalize(&sys).unwrap_or(sys);
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    for line in mounts.lines() {
        let Some(id) = line.split_whitespace().nth(2) else {
            continue;
        };
        let Some((a, b)) = id.split_once(':') else {
            continue;
        };
        let mount_sys = PathBuf::from(format!("/sys/dev/block/{a}:{b}"));
        if let Ok(mounted) = std::fs::canonicalize(mount_sys)
            && (mounted == target || mounted.starts_with(&target))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_swap(dev: u64) -> Result<bool> {
    let target = sysfs_device(dev);
    let swaps = match std::fs::read_to_string("/proc/swaps") {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for line in swaps.lines().skip(1) {
        let Some(path) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if metadata.file_type().is_block_device() {
            let swap = sysfs_device(metadata.rdev());
            if swap == target || swap.starts_with(&target) || target.starts_with(&swap) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn has_holders(dev: u64) -> Result<bool> {
    let target = sysfs_device(dev);
    if directory_has_entries(&target.join("holders"))? {
        return Ok(true);
    }
    // Whole-disk targets must also account for holders attached to a child
    // partition (for example LVM, dm-crypt, or MD RAID).
    let entries = match std::fs::read_dir(&target) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.join("partition").exists() && directory_has_entries(&path.join("holders"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn directory_has_entries(path: &Path) -> Result<bool> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().transpose()?.is_some()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sysfs_device(dev: u64) -> PathBuf {
    let (major, minor) = major_minor(dev);
    let path = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    std::fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_checked_image_io() {
        let path = std::env::temp_dir().join(format!("parted-device-{}", std::process::id()));
        std::fs::write(&path, vec![0; 1024]).unwrap();
        let device = Device::open(&path, true, None).unwrap();
        device.write_at(1023, &[7]).unwrap();
        assert!(device.write_at(1024, &[7]).is_err());
        drop(device);
        std::fs::remove_file(path).unwrap();
    }
}
