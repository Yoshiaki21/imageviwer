//! JPEG XL decoding, through jxl-rs's adapter for the `image` crate.
//!
//! `image` has no JPEG XL variant of its own, so this cannot go through
//! `decode_as`; it builds the adapter directly instead. jxl-rs applies the
//! header's orientation itself, and only the first frame of an animation is
//! decoded.

use std::path::Path;

use anyhow::{Context as _, Result};
use image::DynamicImage;
use jxl_image_rs_integration::JxlDecoder as AdapterDecoder;

use super::{DecodedImage, ImageDecoder, ImageFormat};

pub struct JxlDecoder;

impl ImageDecoder for JxlDecoder {
    fn format(&self) -> ImageFormat {
        ImageFormat::Jxl
    }

    fn decode(&self, path: &Path) -> Result<DecodedImage> {
        // The adapter buffers the reader itself.
        let file = std::fs::File::open(path).context("opening file")?;
        let decoder = AdapterDecoder::new(file).context("creating decoder")?;
        let image = DynamicImage::from_decoder(decoder).context("decoding pixels")?;
        Ok(DecodedImage::from_dynamic(image))
    }
}
