use crate::device::Device;
use crate::model::{Disk, LabelKind, LbaRange, Partition};
use crate::{Error, Result};

const ENTRY_OFFSET: usize = 446;
const ENTRY_SIZE: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MbrEntry {
    pub bootable: bool,
    pub partition_type: u8,
    pub first_lba: u32,
    pub sectors: u32,
    pub chs_first: [u8; 3],
    pub chs_last: [u8; 3],
}

#[derive(Clone, Debug)]
pub struct Mbr {
    sector: [u8; 512],
    pub entries: [Option<MbrEntry>; 4],
}

impl Mbr {
    pub fn empty() -> Self {
        let mut sector = [0; 512];
        sector[510..].copy_from_slice(&[0x55, 0xaa]);
        Self {
            sector,
            entries: std::array::from_fn(|_| None),
        }
    }

    pub fn parse(sector: &[u8], device_blocks: u64) -> Result<Self> {
        if sector.len() < 512 || sector[510..512] != [0x55, 0xaa] {
            return Err(Error::Invalid("missing MBR signature".into()));
        }
        let mut raw = [0; 512];
        raw.copy_from_slice(&sector[..512]);
        let mut entries: [Option<MbrEntry>; 4] = std::array::from_fn(|_| None);
        let mut ranges = Vec::new();
        for (index, slot) in entries.iter_mut().enumerate() {
            let offset = ENTRY_OFFSET + index * ENTRY_SIZE;
            let e = &sector[offset..offset + ENTRY_SIZE];
            if e.iter().all(|b| *b == 0) {
                continue;
            }
            if !matches!(e[0], 0 | 0x80) {
                return Err(Error::Invalid(format!(
                    "MBR entry {} has an invalid boot flag",
                    index + 1
                )));
            }
            let partition_type = e[4];
            let first_lba = u32::from_le_bytes(e[8..12].try_into().unwrap());
            let sectors = u32::from_le_bytes(e[12..16].try_into().unwrap());
            if partition_type == 0 || sectors == 0 {
                return Err(Error::Invalid(format!(
                    "MBR entry {} is partially empty",
                    index + 1
                )));
            }
            if matches!(partition_type, 0x05 | 0x0f | 0x85) {
                return Err(Error::Invalid(
                    "extended/logical MBR partitions are not supported".into(),
                ));
            }
            let last = u64::from(first_lba)
                .checked_add(u64::from(sectors) - 1)
                .ok_or_else(|| Error::Invalid("MBR partition range overflow".into()))?;
            let range = LbaRange {
                first: u64::from(first_lba),
                last,
            };
            if range.first == 0 || range.last >= device_blocks {
                return Err(Error::Invalid(format!(
                    "MBR entry {} is outside the device",
                    index + 1
                )));
            }
            if ranges.iter().any(|other: &LbaRange| other.overlaps(range)) {
                return Err(Error::Invalid("MBR partitions overlap".into()));
            }
            ranges.push(range);
            *slot = Some(MbrEntry {
                bootable: e[0] == 0x80,
                partition_type,
                first_lba,
                sectors,
                chs_first: e[1..4].try_into().unwrap(),
                chs_last: e[5..8].try_into().unwrap(),
            });
        }
        Ok(Self {
            sector: raw,
            entries,
        })
    }

    pub fn read(device: &Device) -> Result<Self> {
        Self::parse(&device.read_lba(0, 1)?, device.blocks())
    }

    pub fn bytes(&self, device_blocks: u64) -> Result<[u8; 512]> {
        let mut bytes = self.sector;
        bytes[ENTRY_OFFSET..510].fill(0);
        for (index, entry) in self.entries.iter().enumerate() {
            let Some(entry) = entry else { continue };
            let last = u64::from(entry.first_lba)
                .checked_add(u64::from(entry.sectors).saturating_sub(1))
                .ok_or_else(|| Error::Invalid("MBR partition range overflow".into()))?;
            if entry.first_lba == 0 || entry.sectors == 0 || last >= device_blocks {
                return Err(Error::Invalid("MBR partition is outside the device".into()));
            }
            let offset = ENTRY_OFFSET + index * ENTRY_SIZE;
            bytes[offset] = if entry.bootable { 0x80 } else { 0 };
            bytes[offset + 1..offset + 4].copy_from_slice(&entry.chs_first);
            bytes[offset + 4] = entry.partition_type;
            bytes[offset + 5..offset + 8].copy_from_slice(&entry.chs_last);
            bytes[offset + 8..offset + 12].copy_from_slice(&entry.first_lba.to_le_bytes());
            bytes[offset + 12..offset + 16].copy_from_slice(&entry.sectors.to_le_bytes());
        }
        Self::parse(&bytes, device_blocks)?;
        Ok(bytes)
    }

    pub fn model(&self, block_size: u32, blocks: u64) -> Disk {
        let partitions = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry.as_ref().map(|e| Partition {
                    number: index as u32 + 1,
                    range: LbaRange {
                        first: u64::from(e.first_lba),
                        last: u64::from(e.first_lba) + u64::from(e.sectors) - 1,
                    },
                    type_id: format!("{:02x}", e.partition_type),
                    unique_id: None,
                    name: String::new(),
                    attributes: 0,
                    bootable: e.bootable,
                })
            })
            .collect();
        Disk {
            kind: LabelKind::Mbr,
            logical_block_size: block_size,
            blocks,
            usable: LbaRange {
                first: 1,
                last: blocks - 1,
            },
            partitions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_boot_code_and_disk_signature() {
        let mut bytes = Mbr::empty().sector;
        bytes[0] = 0xfa;
        bytes[440..444].copy_from_slice(&123_u32.to_le_bytes());
        let parsed = Mbr::parse(&bytes, 100).unwrap();
        let output = parsed.bytes(100).unwrap();
        assert_eq!(output[0], 0xfa);
        assert_eq!(&output[440..444], &123_u32.to_le_bytes());
    }

    #[test]
    fn rejects_overlap() {
        let mut mbr = Mbr::empty();
        for (i, first) in [1, 5].into_iter().enumerate() {
            mbr.entries[i] = Some(MbrEntry {
                bootable: false,
                partition_type: 0x83,
                first_lba: first,
                sectors: 10,
                chs_first: [0xff; 3],
                chs_last: [0xff; 3],
            });
        }
        assert!(mbr.bytes(100).is_err());
    }
}
