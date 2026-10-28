//! Images: the pictures a person sends, or points at.
//!
//! A model that can see is a model that can be asked about a diagram, a
//! screenshot, a photograph of a bug. The picture itself is carried in the
//! message as base64, which is what both the OpenAI-compatible shape and
//! the Anthropic one expect.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The largest picture that is carried along.
///
/// The APIs that take images count them in bytes, and a picture much
/// bigger than this is a picture nobody meant to send.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;

/// A picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// The kind of picture, e.g. `image/png`.
    pub media_type: String,
    /// The picture itself, base64.
    pub data: String,
}

impl Image {
    /// A picture from bytes that are already the right kind.
    pub fn from_bytes(bytes: &[u8], media_type: &str) -> Result<Image, String> {
        if bytes.is_empty() {
            return Err("the file is empty".into());
        }
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "the picture is {} MB, which is too big to send (the limit is {} MB)",
                bytes.len() / (1024 * 1024),
                MAX_BYTES / (1024 * 1024)
            ));
        }
        Ok(Image {
            media_type: media_type.to_string(),
            data: base64_encode(bytes),
        })
    }

    /// A picture from a file.
    pub fn from_file(path: &Path) -> Result<Image, String> {
        let media_type =
            media_type_of(path).ok_or_else(|| format!("{} is not a picture", path.display()))?;
        let bytes =
            std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        Image::from_bytes(&bytes, media_type)
    }

    /// A short description, for the model and for the user.
    pub fn describe(&self) -> String {
        let size = self.data.len() * 3 / 4;
        format!("a picture ({}, {} kB)", self.media_type, size / 1024)
    }
}

/// The kind of picture a file holds, from its name.
pub fn media_type_of(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".png") {
        Some("image/png")
    } else if name.ends_with(".jpg") || name.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if name.ends_with(".gif") {
        Some("image/gif")
    } else if name.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

/// Encode bytes as base64, without another crate.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_encoded_as_base64() {
        // The examples from the base64 specification.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_kind_of_picture_comes_from_the_name() {
        assert_eq!(media_type_of(Path::new("shot.PNG")), Some("image/png"));
        assert_eq!(media_type_of(Path::new("shot.jpeg")), Some("image/jpeg"));
        assert_eq!(media_type_of(Path::new("notes.txt")), None);
    }

    #[test]
    fn a_picture_that_is_too_big_is_refused() {
        let big = vec![0u8; MAX_BYTES + 1];
        assert!(Image::from_bytes(&big, "image/png").is_err());
    }

    #[test]
    fn an_empty_file_is_refused() {
        assert!(Image::from_bytes(&[], "image/png").is_err());
    }
}
