//! Base64 transport for the terminal frame payloads.
//!
//! Frames carry raw escape-sequence bytes, and `serde_json` writes a `Vec<u8>` as an array of
//! decimal numbers — for typical escape-sequence bytes about three and a third characters each,
//! formatted and re-parsed one element at a time. Since a repainting agent sends most of a screen
//! on every keystroke, that encoding sits directly in the input-latency path. Base64 keeps the
//! envelope readable JSON while more than halving the payload and reducing it to a single pass
//! over the bytes.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Serializes `Vec<u8>` fields as a base64 string via `#[serde(with = "crate::base64")]`.
pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    encode(bytes).serialize(serializer)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    let encoded = String::deserialize(deserializer)?;
    decode(encoded.as_bytes()).ok_or_else(|| D::Error::custom("payload is not valid base64"))
}

fn encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |block, (index, byte)| {
                block | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            encoded.push(ALPHABET[(block >> (18 - 6 * index)) as usize & 0x3f] as char);
        }
        for _ in chunk.len()..3 {
            encoded.push(PAD as char);
        }
    }
    encoded
}

fn decode(encoded: &[u8]) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(4) {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 4 * 3);
    for chunk in encoded.chunks(4) {
        let padding = chunk.iter().filter(|byte| **byte == PAD).count();
        if padding > 2 || chunk[..4 - padding].contains(&PAD) {
            return None;
        }
        let mut block = 0_u32;
        for (index, byte) in chunk[..4 - padding].iter().enumerate() {
            let value = ALPHABET.iter().position(|candidate| candidate == byte)?;
            block |= (value as u32) << (18 - 6 * index);
        }
        for index in 0..3 - padding {
            bytes.push((block >> (16 - 8 * index)) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_payload_length_and_byte_value() {
        for length in 0..=32 {
            let bytes = (0..length).map(|index| index as u8).collect::<Vec<_>>();
            assert_eq!(
                decode(encode(&bytes).as_bytes()).as_deref(),
                Some(&bytes[..])
            );
        }
        let all_bytes = (0..=255).collect::<Vec<u8>>();
        assert_eq!(decode(encode(&all_bytes).as_bytes()), Some(all_bytes));
    }

    #[test]
    fn matches_the_canonical_alphabet_and_padding() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"\xff\xef\xfe"), "/+/+");
    }

    #[test]
    fn rejects_malformed_input_instead_of_inventing_bytes() {
        assert_eq!(decode(b"Zg="), None, "length must be a multiple of four");
        assert_eq!(decode(b"Z==="), None, "at most two padding characters");
        assert_eq!(decode(b"Z=g="), None, "padding may not appear mid-block");
        assert_eq!(decode(b"Zg-="), None, "characters outside the alphabet");
    }

    #[test]
    fn a_repainted_screen_is_far_smaller_than_a_json_byte_array() {
        let screen = std::iter::repeat_n(b"\x1b[38;5;42mcell\x1b[m", 1_000)
            .flatten()
            .copied()
            .collect::<Vec<u8>>();
        let as_array = serde_json::to_string(&screen).unwrap().len();
        let as_base64 = serde_json::to_string(&encode(&screen)).unwrap().len();

        assert!(
            as_base64 * 2 < as_array,
            "base64 payload {as_base64} should be under half the array form {as_array}"
        );
    }
}
