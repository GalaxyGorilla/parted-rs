use crate::device::Device;
use crate::model::{Disk, LabelKind, LbaRange, Partition};
use crate::util::{crc32, format_guid, le_u32, le_u64, unique_guid};
use crate::{Error, Result};

pub const ENTRY_SIZE: u32 = 128;
pub const DEFAULT_ENTRIES: u32 = 128;
const HEADER_SIZE: u32 = 92;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GptEntry {
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Gpt {
    pub disk_guid: [u8; 16],
    pub first_usable: u64,
    pub last_usable: u64,
    pub entries: Vec<Option<GptEntry>>,
    pub block_size: u32,
    pub blocks: u64,
}

#[derive(Clone, Debug)]
struct Header {
    current: u64,
    backup: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: [u8; 16],
    array_lba: u64,
    entry_count: u32,
    entry_size: u32,
    array_crc: u32,
}

impl Gpt {
    pub fn empty(block_size: u32, blocks: u64) -> Result<Self> {
        if !matches!(block_size, 512 | 4096) {
            return Err(Error::Invalid(
                "GPT requires 512 or 4096-byte blocks".into(),
            ));
        }
        let array_blocks = array_blocks(block_size, DEFAULT_ENTRIES, ENTRY_SIZE)?;
        let first_usable = 2 + array_blocks;
        let last_usable = blocks
            .checked_sub(array_blocks + 2)
            .ok_or_else(|| Error::Invalid("device is too small for GPT metadata".into()))?;
        if first_usable > last_usable {
            return Err(Error::Invalid("device is too small for GPT".into()));
        }
        Ok(Self {
            disk_guid: unique_guid()?,
            first_usable,
            last_usable,
            entries: vec![None; DEFAULT_ENTRIES as usize],
            block_size,
            blocks,
        })
    }

    pub fn read(device: &Device) -> Result<Self> {
        let block_size = device.block_size();
        let blocks = device.blocks();
        validate_protective_mbr(&device.read_lba(0, 1)?, blocks)?;
        let primary = parse_header(&device.read_lba(1, 1)?, block_size, blocks, 1)?;
        let backup = parse_header(
            &device.read_lba(blocks - 1, 1)?,
            block_size,
            blocks,
            blocks - 1,
        )?;
        validate_header_pair(&primary, &backup, blocks, block_size)?;
        let p_array = read_array(device, &primary)?;
        let b_array = read_array(device, &backup)?;
        if p_array != b_array {
            return Err(Error::Invalid(
                "primary and backup GPT entry arrays differ".into(),
            ));
        }
        let entries = parse_entries(
            &p_array,
            primary.entry_count,
            primary.entry_size,
            primary.first_usable,
            primary.last_usable,
        )?;
        Ok(Self {
            disk_guid: primary.disk_guid,
            first_usable: primary.first_usable,
            last_usable: primary.last_usable,
            entries,
            block_size,
            blocks,
        })
    }

    pub(crate) fn erase_writes(device: &Device) -> Result<Vec<(u64, Vec<u8>)>> {
        let block_size = device.block_size();
        let blocks = device.blocks();
        let primary = parse_header(&device.read_lba(1, 1)?, block_size, blocks, 1)?;
        let backup = parse_header(
            &device.read_lba(blocks - 1, 1)?,
            block_size,
            blocks,
            blocks - 1,
        )?;
        validate_header_pair(&primary, &backup, blocks, block_size)?;
        read_array(device, &primary)?;
        read_array(device, &backup)?;

        let primary_blocks = array_blocks(block_size, primary.entry_count, primary.entry_size)?;
        let backup_blocks = array_blocks(block_size, backup.entry_count, backup.entry_size)?;
        let zeroed = |count: u64| -> Result<Vec<u8>> {
            let len = count
                .checked_mul(u64::from(block_size))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| Error::Invalid("GPT metadata size overflow".into()))?;
            Ok(vec![0; len])
        };
        Ok(vec![
            (backup.array_lba, zeroed(backup_blocks)?),
            (backup.current, vec![0; block_size as usize]),
            (primary.array_lba, zeroed(primary_blocks)?),
            (primary.current, vec![0; block_size as usize]),
        ])
    }

    pub fn model(&self) -> Disk {
        let partitions = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry.as_ref().map(|e| Partition {
                    number: index as u32 + 1,
                    range: LbaRange {
                        first: e.first_lba,
                        last: e.last_lba,
                    },
                    type_id: format_guid(e.type_guid),
                    unique_id: Some(format_guid(e.unique_guid)),
                    name: e.name.clone(),
                    attributes: e.attributes,
                    bootable: false,
                })
            })
            .collect();
        Disk {
            kind: LabelKind::Gpt,
            logical_block_size: self.block_size,
            blocks: self.blocks,
            usable: LbaRange {
                first: self.first_usable,
                last: self.last_usable,
            },
            partitions,
        }
    }

    /// Returns complete aligned metadata writes in safe commit order:
    /// protective MBR, backup array/header, then primary array/header.
    pub fn writes(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        self.validate()?;
        let count = self.entries.len() as u32;
        let arrays = encode_entries(&self.entries)?;
        let array_blocks = array_blocks(self.block_size, count, ENTRY_SIZE)?;
        let backup_array_lba = self.blocks - 1 - array_blocks;
        let primary = Header {
            current: 1,
            backup: self.blocks - 1,
            first_usable: self.first_usable,
            last_usable: self.last_usable,
            disk_guid: self.disk_guid,
            array_lba: 2,
            entry_count: count,
            entry_size: ENTRY_SIZE,
            array_crc: crc32(&arrays[..count as usize * ENTRY_SIZE as usize]),
        };
        let backup = Header {
            current: self.blocks - 1,
            backup: 1,
            array_lba: backup_array_lba,
            ..primary.clone()
        };
        let padded = pad_array(arrays, self.block_size)?;
        Ok(vec![
            (backup_array_lba, padded.clone()),
            (
                self.blocks - 1,
                encode_header(&backup, self.block_size)?.to_vec(),
            ),
            (0, protective_mbr(self.block_size, self.blocks)),
            (2, padded),
            (1, encode_header(&primary, self.block_size)?.to_vec()),
        ])
    }

    pub fn validate(&self) -> Result<()> {
        if self.entries.len() != DEFAULT_ENTRIES as usize {
            return Err(Error::Invalid(
                "only 128-entry GPT tables are writable".into(),
            ));
        }
        let mut ranges = Vec::new();
        for entry in self.entries.iter().flatten() {
            if entry.type_guid == [0; 16] || entry.unique_guid == [0; 16] {
                return Err(Error::Invalid("GPT partition GUIDs must be nonzero".into()));
            }
            if entry.first_lba > entry.last_lba
                || entry.first_lba < self.first_usable
                || entry.last_lba > self.last_usable
            {
                return Err(Error::Invalid(
                    "GPT partition is outside usable space".into(),
                ));
            }
            let range = LbaRange {
                first: entry.first_lba,
                last: entry.last_lba,
            };
            if ranges.iter().any(|r: &LbaRange| r.overlaps(range)) {
                return Err(Error::Invalid("GPT partitions overlap".into()));
            }
            ranges.push(range);
            if entry.name.encode_utf16().count() > 36 {
                return Err(Error::Invalid(
                    "GPT partition name exceeds 36 UTF-16 units".into(),
                ));
            }
        }
        Ok(())
    }
}

fn parse_header(bytes: &[u8], block_size: u32, blocks: u64, expected: u64) -> Result<Header> {
    if bytes.len() != block_size as usize || &bytes[0..8] != b"EFI PART" {
        return Err(Error::Invalid("missing GPT header signature".into()));
    }
    if le_u32(&bytes[8..12]) != 0x0001_0000 {
        return Err(Error::Invalid("unsupported GPT revision".into()));
    }
    let size = le_u32(&bytes[12..16]);
    if !(HEADER_SIZE..=block_size).contains(&size) || bytes[20..24] != [0; 4] {
        return Err(Error::Invalid(
            "invalid GPT header size or reserved field".into(),
        ));
    }
    let mut checked = bytes[..size as usize].to_vec();
    let stored_crc = le_u32(&checked[16..20]);
    checked[16..20].fill(0);
    if crc32(&checked) != stored_crc {
        return Err(Error::Invalid("GPT header CRC mismatch".into()));
    }
    let current = le_u64(&bytes[24..32]);
    let backup = le_u64(&bytes[32..40]);
    let first_usable = le_u64(&bytes[40..48]);
    let last_usable = le_u64(&bytes[48..56]);
    let array_lba = le_u64(&bytes[72..80]);
    let entry_count = le_u32(&bytes[80..84]);
    let entry_size = le_u32(&bytes[84..88]);
    if current != expected
        || backup >= blocks
        || first_usable > last_usable
        || last_usable >= blocks
        || entry_count == 0
        || entry_count > 16_384
        || !(128..=4096).contains(&entry_size)
        || !entry_size.is_power_of_two()
        || bytes[56..72] == [0; 16]
    {
        return Err(Error::Invalid("invalid GPT header fields".into()));
    }
    let array_len = u64::from(entry_count)
        .checked_mul(u64::from(entry_size))
        .ok_or_else(|| Error::Invalid("GPT entry array length overflow".into()))?;
    let array_end = array_lba
        .checked_mul(u64::from(block_size))
        .and_then(|n| n.checked_add(array_len))
        .ok_or_else(|| Error::Invalid("GPT entry array range overflow".into()))?;
    if array_lba >= blocks || array_end > blocks * u64::from(block_size) {
        return Err(Error::Invalid(
            "GPT entry array is outside the device".into(),
        ));
    }
    Ok(Header {
        current,
        backup,
        first_usable,
        last_usable,
        disk_guid: bytes[56..72].try_into().unwrap(),
        array_lba,
        entry_count,
        entry_size,
        array_crc: le_u32(&bytes[88..92]),
    })
}

fn validate_header_pair(
    primary: &Header,
    backup: &Header,
    blocks: u64,
    block_size: u32,
) -> Result<()> {
    let array_bytes = u64::from(primary.entry_count) * u64::from(primary.entry_size);
    let array_blocks = array_bytes.div_ceil(u64::from(block_size));
    let primary_array_end = primary.array_lba.checked_add(array_blocks);
    let backup_array_end = backup.array_lba.checked_add(array_blocks);
    if primary.current != 1
        || primary.backup != blocks - 1
        || backup.current != blocks - 1
        || backup.backup != 1
        || primary.first_usable != backup.first_usable
        || primary.last_usable != backup.last_usable
        || primary.disk_guid != backup.disk_guid
        || primary.entry_count != backup.entry_count
        || primary.entry_size != backup.entry_size
        || primary.array_crc != backup.array_crc
        || primary.array_lba < 2
        || primary_array_end.is_none_or(|end| end > primary.first_usable)
        || backup.array_lba <= backup.last_usable
        || backup_array_end.is_none_or(|end| end > backup.current)
    {
        return Err(Error::Invalid(
            "primary and backup GPT headers disagree".into(),
        ));
    }
    Ok(())
}

fn read_array(device: &Device, header: &Header) -> Result<Vec<u8>> {
    let exact = header.entry_count as usize * header.entry_size as usize;
    let blocks = (exact as u64).div_ceil(u64::from(device.block_size()));
    let bytes = device.read_lba(header.array_lba, blocks)?;
    if crc32(&bytes[..exact]) != header.array_crc {
        return Err(Error::Invalid("GPT entry-array CRC mismatch".into()));
    }
    Ok(bytes[..exact].to_vec())
}

fn parse_entries(
    bytes: &[u8],
    count: u32,
    size: u32,
    first_usable: u64,
    last_usable: u64,
) -> Result<Vec<Option<GptEntry>>> {
    let mut result = Vec::with_capacity(count as usize);
    let mut ranges = Vec::new();
    for index in 0..count as usize {
        let raw = &bytes[index * size as usize..(index + 1) * size as usize];
        if raw[..16] == [0; 16] {
            if raw.iter().any(|byte| *byte != 0) {
                return Err(Error::Invalid(format!(
                    "empty GPT entry {} contains nonzero fields",
                    index + 1
                )));
            }
            result.push(None);
            continue;
        }
        if raw[16..32] == [0; 16] {
            return Err(Error::Invalid(format!(
                "GPT entry {} has a zero unique GUID",
                index + 1
            )));
        }
        let first_lba = le_u64(&raw[32..40]);
        let last_lba = le_u64(&raw[40..48]);
        let range = LbaRange::new(first_lba, last_lba)?;
        if first_lba < first_usable || last_lba > last_usable {
            return Err(Error::Invalid(format!(
                "GPT entry {} is outside usable space",
                index + 1
            )));
        }
        if ranges.iter().any(|r: &LbaRange| r.overlaps(range)) {
            return Err(Error::Invalid("GPT partitions overlap".into()));
        }
        ranges.push(range);
        let units = raw[56..128]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|u| *u != 0)
            .collect::<Vec<_>>();
        let name = String::from_utf16(&units)
            .map_err(|_| Error::Invalid("GPT partition name is invalid UTF-16".into()))?;
        result.push(Some(GptEntry {
            type_guid: raw[0..16].try_into().unwrap(),
            unique_guid: raw[16..32].try_into().unwrap(),
            first_lba,
            last_lba,
            attributes: le_u64(&raw[48..56]),
            name,
        }));
    }
    Ok(result)
}

fn encode_entries(entries: &[Option<GptEntry>]) -> Result<Vec<u8>> {
    let mut bytes = vec![0; entries.len() * ENTRY_SIZE as usize];
    for (index, entry) in entries.iter().enumerate() {
        let Some(entry) = entry else { continue };
        let raw = &mut bytes[index * ENTRY_SIZE as usize..(index + 1) * ENTRY_SIZE as usize];
        raw[..16].copy_from_slice(&entry.type_guid);
        raw[16..32].copy_from_slice(&entry.unique_guid);
        raw[32..40].copy_from_slice(&entry.first_lba.to_le_bytes());
        raw[40..48].copy_from_slice(&entry.last_lba.to_le_bytes());
        raw[48..56].copy_from_slice(&entry.attributes.to_le_bytes());
        let name: Vec<_> = entry.name.encode_utf16().collect();
        if name.len() > 36 {
            return Err(Error::Invalid(
                "GPT partition name exceeds 36 UTF-16 units".into(),
            ));
        }
        for (chunk, unit) in raw[56..128].as_chunks_mut::<2>().0.iter_mut().zip(name) {
            chunk.copy_from_slice(&unit.to_le_bytes());
        }
    }
    Ok(bytes)
}

fn encode_header(header: &Header, block_size: u32) -> Result<Vec<u8>> {
    let mut bytes = vec![0; block_size as usize];
    bytes[0..8].copy_from_slice(b"EFI PART");
    bytes[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&HEADER_SIZE.to_le_bytes());
    bytes[24..32].copy_from_slice(&header.current.to_le_bytes());
    bytes[32..40].copy_from_slice(&header.backup.to_le_bytes());
    bytes[40..48].copy_from_slice(&header.first_usable.to_le_bytes());
    bytes[48..56].copy_from_slice(&header.last_usable.to_le_bytes());
    bytes[56..72].copy_from_slice(&header.disk_guid);
    bytes[72..80].copy_from_slice(&header.array_lba.to_le_bytes());
    bytes[80..84].copy_from_slice(&header.entry_count.to_le_bytes());
    bytes[84..88].copy_from_slice(&header.entry_size.to_le_bytes());
    bytes[88..92].copy_from_slice(&header.array_crc.to_le_bytes());
    let crc = crc32(&bytes[..HEADER_SIZE as usize]);
    bytes[16..20].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

fn array_blocks(block_size: u32, count: u32, size: u32) -> Result<u64> {
    u64::from(count)
        .checked_mul(u64::from(size))
        .map(|n| n.div_ceil(u64::from(block_size)))
        .ok_or_else(|| Error::Invalid("GPT array size overflow".into()))
}

fn pad_array(mut bytes: Vec<u8>, block_size: u32) -> Result<Vec<u8>> {
    let block = block_size as usize;
    let padded = bytes
        .len()
        .checked_add(block - 1)
        .map(|n| n / block * block)
        .ok_or_else(|| Error::Invalid("GPT array size overflow".into()))?;
    bytes.resize(padded, 0);
    Ok(bytes)
}

fn protective_mbr(block_size: u32, blocks: u64) -> Vec<u8> {
    let mut bytes = vec![0; block_size as usize];
    bytes[446 + 4] = 0xee;
    bytes[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
    let sectors = (blocks - 1).min(u64::from(u32::MAX)) as u32;
    bytes[446 + 12..446 + 16].copy_from_slice(&sectors.to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    bytes
}

fn validate_protective_mbr(bytes: &[u8], blocks: u64) -> Result<()> {
    if bytes.len() < 512 || bytes[510..512] != [0x55, 0xaa] {
        return Err(Error::Invalid(
            "GPT protective MBR signature is missing".into(),
        ));
    }
    let entry = &bytes[446..462];
    let first = le_u32(&entry[8..12]);
    let sectors = le_u32(&entry[12..16]);
    let expected = (blocks - 1).min(u64::from(u32::MAX)) as u32;
    if entry[0] != 0
        || entry[4] != 0xee
        || first != 1
        || sectors != expected
        || bytes[462..510].iter().any(|byte| *byte != 0)
    {
        return Err(Error::Invalid("invalid GPT protective MBR".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(block_size: u32) {
        let blocks = if block_size == 512 { 4096 } else { 512 };
        let mut gpt = Gpt::empty(block_size, blocks).unwrap();
        gpt.entries[0] = Some(GptEntry {
            type_guid: [1; 16],
            unique_guid: [2; 16],
            first_lba: gpt.first_usable,
            last_lba: gpt.first_usable + 9,
            attributes: 4,
            name: "data".into(),
        });
        let path =
            std::env::temp_dir().join(format!("parted-gpt-{block_size}-{}", std::process::id()));
        std::fs::write(&path, vec![0; block_size as usize * blocks as usize]).unwrap();
        {
            let device = Device::open(&path, true, Some(block_size)).unwrap();
            for (lba, bytes) in gpt.writes().unwrap() {
                device.write_lba(lba, &bytes).unwrap();
            }
            device.sync().unwrap();
        }
        let device = Device::open(&path, false, Some(block_size)).unwrap();
        assert_eq!(Gpt::read(&device).unwrap().entries[0], gpt.entries[0]);
        drop(device);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn roundtrip_512() {
        roundtrip(512);
    }

    #[test]
    fn roundtrip_4096() {
        roundtrip(4096);
    }
}
