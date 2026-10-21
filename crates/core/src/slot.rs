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
/// # Errors
///
/// If the image has no slot marker or several, or the slot runs past its end.
pub fn locate(image: &[u8]) -> Result<usize, String> {
    // Collect every marker position; there must be exactly one. Zero means
    // this isn't a stub; several would make "which one runs?" a guess.
    let find = |marker: [u8; 16]| -> Vec<usize> {
        image
            .windows(marker.len())
            .enumerate()
            .filter(|(_, w)| *w == marker)
            .map(|(i, _)| i)
            .collect()
    };
    // The legacy marker only counts when the current one is absent. The rea
    // tool itself contains both (as the constants it compares against), and
    // must still be told apart from a stub by its current-marker match alone.
    let mut hits = find(MARKER);
    if hits.is_empty() {
        hits = find(LEGACY_MARKER);
    }
    match hits.as_slice() {
        [] => Err("no alias slot found (not a rea stub?)".into()),
        &[offset] if offset + SIZE <= image.len() => Ok(offset),
        [_] => Err("alias slot is truncated".into()),
        _ => Err(format!(
            "found {} alias slot markers; expected exactly one",
            hits.len()
        )),
    }
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
        // No marker, and two markers.
        assert!(locate(b"nothing here").is_err());
        let mut twice = MARKER.repeat(2);
        twice.resize(SIZE * 3, 0);
        assert!(locate(&twice).unwrap_err().contains("2 alias slot markers"));
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
        // With both, the current marker is the one found.
        let mut both = LEGACY_MARKER.to_vec();
        both.extend_from_slice(&MARKER);
        both.resize(SIZE * 3, 0);
        assert_eq!(locate(&both), Ok(LEGACY_MARKER.len()));
    }
}
