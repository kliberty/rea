//! Binary layout of the template slot in patched stub binaries.
//!
//! A stub contains one [`Slot`] in its read-only data. `rea patch` copies
//! the stub, finds the slot by its marker, and overwrites it with an alias's
//! name and template. At run time the stub reads the slot back with [`decode`].
//!
//! Layout (fixed, `repr(C)`, no padding since every field is a byte array):
//!
//! | bytes      | field  | contents                                   |
//! |------------|--------|--------------------------------------------|
//! | 0..16      | marker | [`MARKER`], left unchanged by patching     |
//! | 16..20     | len    | payload length, u32 little-endian; 0 = empty |
//! | 20..20+CAP | data   | `name` + `\n` + `template`, zero padded    |
//!
//! Constraint on change: the static itself must be declared in the stub
//! binary (src/bin/rea-stub.rs), never in this crate. A static here would
//! also be linked into the `rea` tool, giving it a valid empty slot, and
//! then `patch` could no longer tell the tool from a stub (and refuse to
//! overwrite it).

use std::fmt;

/// Identifies the slot in a binary image. The final `1` is a format version:
/// bump it if the layout ever changes, so old patchers can't misread new stubs.
pub const MARKER: [u8; 16] = *b"rea:alias:slot:1";

/// The marker in stubs built before the rename from aliasmux to rea. The
/// layout is the same, so [`locate`] accepts it too: aliases made with the
/// old tool are still listed, inspected, and recognized as aliases that
/// `patch` may replace. New stubs only ever carry [`MARKER`].
pub const LEGACY_MARKER: [u8; 16] = *b"aliasmux:slot:v1";

/// Maximum payload size in bytes. Arbitrary: generous for one-line templates
/// while keeping each stub small. Changing it changes the stub layout, so
/// stubs and patcher must be rebuilt together (as they are, in one workspace).
pub const CAPACITY: usize = 4096;

/// Size of the whole slot in bytes.
pub const SIZE: usize = std::mem::size_of::<Slot>();

/// The slot as it sits in memory and on disk.
#[repr(C)]
pub struct Slot {
    /// Always [`MARKER`].
    pub marker: [u8; 16],
    /// Payload length, little-endian. Stored as bytes, not `u32`, so the layout
    /// has no alignment padding and reads the same on every target.
    pub len: [u8; 4],
    /// Payload followed by zeros.
    pub data: [u8; CAPACITY],
}

/// An unpatched slot, for stubs to initialize their static with.
pub const EMPTY: Slot = Slot {
    marker: MARKER,
    len: [0; 4],
    data: [0; CAPACITY],
};

/// Why a slot could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotError {
    /// Length 0: this stub was never patched.
    Empty,
    /// The contents are inconsistent (bad length, invalid UTF-8, no name).
    Corrupt(&'static str),
}

impl fmt::Display for SlotError {
    /// Describe the problem for the user.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotError::Empty => f.write_str(
                "this is an unpatched rea stub; create aliases from it with `rea patch`",
            ),
            SlotError::Corrupt(why) => write!(f, "corrupt alias slot: {why}"),
        }
    }
}

/// Read the name and template out of a slot, from a stub's static or from a
/// file image.
///
/// # Errors
///
/// If the slot is empty or its contents don't decode.
pub fn decode(slot: &Slot) -> Result<(&str, &str), SlotError> {
    let len = u32::from_le_bytes(slot.len) as usize;
    // Zero length is the unpatched state, which deserves its own message.
    if len == 0 {
        return Err(SlotError::Empty);
    }
    // A length past the buffer can only come from corruption or a mismatched
    // patcher; reading it would go out of bounds.
    let payload = slot
        .data
        .get(..len)
        .ok_or(SlotError::Corrupt("length exceeds capacity"))?;
    let text =
        std::str::from_utf8(payload).map_err(|_| SlotError::Corrupt("payload is not UTF-8"))?;
    // The name ends at the first newline; templates are single lines.
    text.split_once('\n')
        .ok_or(SlotError::Corrupt("missing name"))
}

/// Build the bytes of a patched slot: exactly [`SIZE`] of them.
///
/// # Errors
///
/// If the name or template contains a newline, or the payload doesn't fit.
pub fn encode(name: &str, template: &str) -> Result<Vec<u8>, String> {
    // Newlines would break the name/template split, and alias files can't
    // produce them anyway; refuse rather than corrupt.
    if name.contains('\n') || template.contains('\n') {
        return Err("name and template must be single lines".into());
    }
    let payload = format!("{name}\n{template}");
    if payload.len() > CAPACITY {
        return Err(format!(
            "alias `{name}` is {} bytes; patched stubs hold at most {CAPACITY}",
            payload.len()
        ));
    }
    // Marker, length, payload, then zero padding up to the full slot size, so
    // leftovers from a previous payload can never survive a re-patch.
    let mut bytes = Vec::with_capacity(SIZE);
    bytes.extend_from_slice(&MARKER);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload.as_bytes());
    bytes.resize(SIZE, 0);
    Ok(bytes)
}

/// Find the slot in a binary image, as a byte offset.
///
/// A marker alone doesn't make a slot: a binary that uses the marker as a
/// constant (the rea tool, or a stub built without LTO, which keeps more of
/// rea-core than it calls) carries copies of the bytes with unrelated data
/// after them. Only a marker followed by a well-formed slot counts, so
/// whatever copies the compiler and linker keep, a stub has exactly one.
///
/// # Errors
///
/// If the image has no well-formed slot, or more than one.
pub fn locate(image: &[u8]) -> Result<usize, String> {
    // Every marker position, current or legacy, and of those the ones that
    // start a well-formed slot.
    let markers: Vec<usize> = image
        .windows(MARKER.len())
        .enumerate()
        .filter(|(_, w)| *w == MARKER || *w == LEGACY_MARKER)
        .map(|(i, _)| i)
        .collect();
    let slots: Vec<usize> = markers
        .iter()
        .copied()
        .filter(|&offset| is_slot(image, offset))
        .collect();
    match (slots.as_slice(), markers.len()) {
        (&[offset], _) => Ok(offset),
        ([], 0) => Err("no alias slot marker found".into()),
        ([], _) => Err("alias slot marker found, but no well-formed slot after it".into()),
        // Several would make "which one runs?" a guess.
        _ => Err(format!(
            "found {} alias slots; expected exactly one",
            slots.len()
        )),
    }
}

/// Whether a well-formed slot starts at `offset`: it fits in the image, its
/// length is within [`CAPACITY`], a non-empty payload decodes, and every byte
/// after the payload is zero, as [`encode`] and [`EMPTY`] always leave it.
/// Unrelated data after a stray copy of the marker fails at least one of
/// these, almost certainly the zero padding: up to 4 KiB of zeros.
fn is_slot(image: &[u8], offset: usize) -> bool {
    let Some(bytes) = image.get(offset..offset + SIZE) else {
        return false;
    };
    // The length field follows the marker; the data follows the length.
    let (len, data) = bytes[MARKER.len()..].split_at(4);
    let len = u32::from_le_bytes(len.try_into().expect("4 bytes")) as usize;
    if len > CAPACITY || data[len..].iter().any(|&b| b != 0) {
        return false;
    }
    // Empty is a valid (unpatched) slot; otherwise the payload must decode.
    matches!(decode_at(image, offset), Ok(_) | Err(SlotError::Empty))
}

/// Interpret the slot at `offset`, as found by [`locate`], in a file image.
///
/// # Errors
///
/// As for [`decode`], or if the image is too short to hold a slot there.
pub fn decode_at(image: &[u8], offset: usize) -> Result<(&str, &str), SlotError> {
    let bytes = image
        .get(offset..offset + SIZE)
        .ok_or(SlotError::Corrupt("slot is truncated"))?;
    // SAFETY: `Slot` is repr(C) and made only of byte arrays, so it has
    // alignment 1, no padding, and every bit pattern is valid. `bytes` is
    // exactly `SIZE` long, so the reference covers only valid memory.
    let slot = unsafe { &*bytes.as_ptr().cast::<Slot>() };
    decode(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_an_image() {
        // A fake binary with junk around the slot, as in a real executable.
        let mut image = vec![0xAAu8; 100];
        let start = image.len();
        // SAFETY: as in `decode_at`; the slot is plain bytes.
        let empty =
            unsafe { std::slice::from_raw_parts((&EMPTY as *const Slot).cast::<u8>(), SIZE) };
        image.extend_from_slice(empty);
        image.extend_from_slice(&[0xBB; 50]);

        // Unpatched: found, but reports empty.
        let offset = locate(&image).unwrap();
        assert_eq!(offset, start);
        assert_eq!(decode_at(&image, offset), Err(SlotError::Empty));

        // Patched: reads back exactly what was written.
        image[offset..offset + SIZE]
            .copy_from_slice(&encode("mkvize", "ffmpeg -i {1} {1}.mkv").unwrap());
        assert_eq!(
            decode_at(&image, offset),
            Ok(("mkvize", "ffmpeg -i {1} {1}.mkv"))
        );
    }

    #[test]
    fn rejects_bad_input() {
        // Too large for the slot, or multi-line.
        assert!(encode("x", &"y".repeat(CAPACITY)).is_err());
        assert!(encode("x", "a\nb").is_err());
        // No marker at all.
        assert!(
            locate(b"nothing here")
                .unwrap_err()
                .contains("no alias slot")
        );
        // Two well-formed slots: which one runs would be a guess.
        let mut twice = encode("a", "true").unwrap();
        twice.extend_from_slice(&encode("b", "true").unwrap());
        assert!(locate(&twice).unwrap_err().contains("found 2 alias slots"));
    }

    #[test]
    fn stray_copies_of_the_marker_are_not_slots() {
        // A copy of the marker constant followed by unrelated data, as in the
        // tool or a stub built without LTO, then the real (empty) slot.
        let mut image = vec![0xAAu8; 10];
        image.extend_from_slice(&MARKER);
        image.extend_from_slice(b"some other read-only data");
        let start = image.len();
        image.extend_from_slice(&encode("x", "true").unwrap());
        image[start + MARKER.len()..start + SIZE].fill(0);
        assert_eq!(locate(&image), Ok(start));
        assert_eq!(decode_at(&image, start), Err(SlotError::Empty));
        // Only stray copies, current and legacy: not a stub.
        let mut tool = MARKER.to_vec();
        tool.extend_from_slice(&[0x01; 64]);
        tool.extend_from_slice(&LEGACY_MARKER);
        tool.extend_from_slice(&[0x02; SIZE]);
        assert!(locate(&tool).unwrap_err().contains("no well-formed slot"));
        // A copy with a plausible length but junk in the padding.
        let mut junk = encode("x", "true").unwrap();
        junk[SIZE - 1] = 1;
        assert!(locate(&junk).is_err());
        // A copy too close to the end to hold a slot.
        let mut short = vec![0u8; 8];
        short.extend_from_slice(&MARKER);
        assert!(locate(&short).is_err());
    }

    #[test]
    fn finds_slots_with_the_legacy_marker() {
        // An alias made by the tool before the rename: same layout, old marker.
        let mut slot = encode("mkvize", "ffmpeg -i {1} {1}.mkv").unwrap();
        slot[..LEGACY_MARKER.len()].copy_from_slice(&LEGACY_MARKER);
        let mut image = vec![0xAAu8; 100];
        image.extend_from_slice(&slot);
        let offset = locate(&image).unwrap();
        assert_eq!(offset, 100);
        assert_eq!(
            decode_at(&image, offset),
            Ok(("mkvize", "ffmpeg -i {1} {1}.mkv"))
        );
        // The new tool contains the current marker as a constant; next to a
        // legacy slot, as when it reads an old alias's bytes, that copy
        // doesn't count.
        let mut both = MARKER.to_vec();
        both.extend_from_slice(b"unrelated");
        let start = both.len();
        both.extend_from_slice(&slot);
        assert_eq!(locate(&both), Ok(start));
    }
}
