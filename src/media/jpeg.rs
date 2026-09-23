//! JPEG decoding.

use std::path::Path;

use anyhow::Result;

use super::{decode_as, DecodedImage, ImageDecoder, ImageFormat};

pub struct JpegDecoder;

impl ImageDecoder for JpegDecoder {
    fn format(&self) -> ImageFormat {
        ImageFormat::Jpeg
    }

    fn decode(&self, path: &Path) -> Result<DecodedImage> {
        decode_as(path, image::ImageFormat::Jpeg)
    }
}
