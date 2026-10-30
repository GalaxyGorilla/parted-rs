use crate::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

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
        let file = OpenOptions::new().read(true).write(writable).open(&path)?;
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
        if writable && block_device && is_mounted(metadata.rdev())? {
            return Err(Error::Refused(
                "refusing to write a mounted device or a device with mounted partitions".into(),
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
