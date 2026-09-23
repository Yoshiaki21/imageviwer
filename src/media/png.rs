//! PNG decoding.

use std::path::Path;

use anyhow::Result;

use super::{decode_as, DecodedImage, ImageDecoder, ImageFormat};

pub struct PngDecoder;

impl ImageDecoder for PngDecoder {
    fn format(&self) -> ImageFormat {
        ImageFormat::Png
    }

    fn decode(&self, path: &Path) -> Result<DecodedImage> {
        decode_as(path, image::ImageFormat::Png)
    }
}
