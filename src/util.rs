use crate::{Error, Result};
use std::io::Read;

const CRC32_TABLE: [u32; 256] = crc32_table();

const fn crc32_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < table.len() {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC32_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
    }
    !crc
}

pub fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed-width field"))
}

pub fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed-width field"))
}

pub fn parse_guid(text: &str) -> Result<[u8; 16]> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid(format!("invalid GUID: {text}")));
    }
    let mut canonical = [0_u8; 16];
    for (i, byte) in canonical.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| Error::Invalid(format!("invalid GUID: {text}")))?;
    }
    let mut disk = canonical;
    disk[0..4].reverse();
    disk[4..6].reverse();
    disk[6..8].reverse();
    Ok(disk)
}

pub fn format_guid(mut disk: [u8; 16]) -> String {
    disk[0..4].reverse();
    disk[4..6].reverse();
    disk[6..8].reverse();
    let h = disk.map(|b| format!("{b:02x}")).concat();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub fn unique_guid() -> Result<[u8; 16]> {
    let mut out = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut out)?;
    // The version field is byte-swapped in GPT's on-disk GUID representation.
    out[7] = (out[7] & 0x0f) | 0x40;
    out[8] = (out[8] & 0x3f) | 0x80;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_crc_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn guid_disk_byte_order_roundtrip() {
        let text = "00112233-4455-6677-8899-aabbccddeeff";
        assert_eq!(format_guid(parse_guid(text).unwrap()), text);
    }

    #[test]
    fn generated_guid_has_uuid_v4_bits_and_is_nonzero() {
        let guid = unique_guid().unwrap();
        assert_ne!(guid, [0; 16]);
        assert_eq!(guid[7] >> 4, 4);
        assert_eq!(guid[8] >> 6, 2);
    }
}
