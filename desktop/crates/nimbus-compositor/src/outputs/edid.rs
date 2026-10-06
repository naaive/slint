// SPDX-License-Identifier: MIT

//! The make, model, and serial number of a display, from its EDID (VESA E-EDID 1.4, section 3).

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const DESCRIPTORS: [usize; 4] = [54, 72, 90, 108];
const NAME_TAG: u8 = 0xfc;
const SERIAL_TAG: u8 = 0xff;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edid {
    /// The three-letter PNP manufacturer ID, such as `DEL`.
    pub make: String,
    /// The monitor name, or the product code in hexadecimal when there's none.
    pub model: String,
    /// The serial number string, or the numeric serial number; empty when there's neither.
    pub serial: String,
}

impl Edid {
    /// Parses the base block of an EDID; `None` if it isn't one.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let block = data.get(..128)?;
        if block[..8] != HEADER {
            return None;
        }
        let id = u16::from_be_bytes([block[8], block[9]]);
        let letter = |shift: u16| char::from(b'@' + ((id >> shift) & 0x1f) as u8);
        let make: String = [letter(10), letter(5), letter(0)].into_iter().collect();
        let product = u16::from_le_bytes([block[10], block[11]]);
        let number = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
        let text = |tag: u8| {
            DESCRIPTORS.iter().find_map(|&at| {
                let descriptor = &block[at..at + 18];
                (descriptor[..3] == [0, 0, 0] && descriptor[3] == tag).then(|| {
                    let text = &descriptor[5..];
                    let end = text.iter().position(|&b| b == b'\n').unwrap_or(text.len());
                    String::from_utf8_lossy(&text[..end]).trim().to_string()
                })
            })
        };
        Some(Self {
            make,
            model: text(NAME_TAG)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("0x{product:04X}")),
            serial: text(SERIAL_TAG)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| if number == 0 { String::new() } else { number.to_string() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edid(descriptors: &[(u8, &str)], number: u32) -> Vec<u8> {
        let mut data = vec![0u8; 128];
        data[..8].copy_from_slice(&HEADER);
        // "DEL": D=4, E=5, L=12.
        data[8..10].copy_from_slice(&((4u16 << 10) | (5 << 5) | 12).to_be_bytes());
        data[10..12].copy_from_slice(&0xa0c1u16.to_le_bytes());
        data[12..16].copy_from_slice(&number.to_le_bytes());
        for (&at, (tag, text)) in DESCRIPTORS.iter().zip(descriptors) {
            data[at + 3] = *tag;
            let mut field = [b' '; 13];
            field[..text.len()].copy_from_slice(text.as_bytes());
            if text.len() < 13 {
                field[text.len()] = b'\n';
            }
            data[at + 5..at + 18].copy_from_slice(&field);
        }
        data
    }

    #[test]
    fn reads_the_descriptors() {
        let parsed =
            Edid::parse(&edid(&[(NAME_TAG, "DELL U2720Q"), (SERIAL_TAG, "CN0ABC123")], 7)).unwrap();
        assert_eq!(
            parsed,
            Edid { make: "DEL".into(), model: "DELL U2720Q".into(), serial: "CN0ABC123".into() }
        );
    }

    #[test]
    fn falls_back_to_the_numbers() {
        let parsed = Edid::parse(&edid(&[], 4242)).unwrap();
        assert_eq!((parsed.model.as_str(), parsed.serial.as_str()), ("0xA0C1", "4242"));
        assert_eq!(Edid::parse(&edid(&[], 0)).unwrap().serial, "");
    }

    #[test]
    fn rejects_other_data() {
        assert_eq!(Edid::parse(&[0; 64]), None);
        assert_eq!(Edid::parse(&[1; 128]), None);
    }
}
