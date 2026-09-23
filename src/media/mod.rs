//! Image decoding, kept behind an extension → format → decoder indirection.
//!
//! Only PNG and JPEG are in scope (spec §6). Adding a format means adding an
//! [`ImageFormat`] variant; every `match` on it here is exhaustive, so the
//! compiler points at each place that still needs work.

mod jpeg;
mod png;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui_kit::RenderImage;
use image::{DynamicImage, Frame, RgbaImage};
use smallvec::SmallVec;

/// A decoded frame in the BGRA layout that GPUI's renderer uploads directly.
pub struct DecodedImage {
    bgra: RgbaImage,
}

impl DecodedImage {
    /// Converts a decoded image into GPUI's pixel layout (BGRA).
    fn from_dynamic(image: DynamicImage) -> Self {
        let mut bgra = image.into_rgba8();
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Self { bgra }
    }

    pub fn width(&self) -> u32 {
        self.bgra.width()
    }

    pub fn height(&self) -> u32 {
        self.bgra.height()
    }

    /// The raw BGRA bytes of one pixel, for verifying the channel order.
    #[cfg(test)]
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.bgra.get_pixel(x, y).0
    }

    /// Hands the pixels to GPUI. The returned image must be released with
    /// `Window::drop_image` once it is no longer displayed, or its texture
    /// stays in the sprite atlas.
    pub fn into_render_image(self) -> Arc<RenderImage> {
        Arc::new(RenderImage::new(SmallVec::from_elem(
            Frame::new(self.bgra),
            1,
        )))
    }
}

/// An image format the viewer can display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    /// Every supported format. Extend together with the enum.
    pub const ALL: [ImageFormat; 2] = [ImageFormat::Png, ImageFormat::Jpeg];

    /// Lower-case extensions that map to this format.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            ImageFormat::Png => &["png"],
            ImageFormat::Jpeg => &["jpg", "jpeg"],
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|format| format.extensions().contains(&extension.as_str()))
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        Self::from_extension(extension)
    }

    /// The decoder responsible for this format.
    fn decoder(self) -> &'static dyn ImageDecoder {
        match self {
            ImageFormat::Png => &png::PngDecoder,
            ImageFormat::Jpeg => &jpeg::JpegDecoder,
        }
    }
}

/// Decodes one image format from disk.
pub trait ImageDecoder: Send + Sync {
    /// The format this decoder handles.
    fn format(&self) -> ImageFormat;

    /// Reads and decodes `path`, or fails if the file is missing or corrupt.
    fn decode(&self, path: &Path) -> Result<DecodedImage>;
}

/// Decodes `path` with the decoder its extension selects.
pub fn decode(path: &Path) -> Result<DecodedImage> {
    let format = ImageFormat::from_path(path)
        .with_context(|| format!("unsupported image extension: {}", path.display()))?;
    let decoder = format.decoder();
    debug_assert_eq!(
        decoder.format(),
        format,
        "the decoder table maps a format to the wrong decoder"
    );
    decoder
        .decode(path)
        .with_context(|| format!("decoding {}", path.display()))
}

/// Whether `path` has an extension the viewer can display.
pub fn is_supported(path: &Path) -> bool {
    ImageFormat::from_path(path).is_some()
}

/// Shared decoding body: the format is already known, so the file contents are
/// never sniffed and a mislabelled file fails rather than being displayed.
fn decode_as(path: &Path, format: image::ImageFormat) -> Result<DecodedImage> {
    let file = std::fs::File::open(path).context("opening file")?;
    let reader = std::io::BufReader::new(file);
    let mut decoder = image::ImageReader::with_format(reader, format)
        .into_decoder()
        .context("creating decoder")?;

    // EXIF orientation is part of the image's intended presentation, so apply
    // it the way GPUI's own loader does. Fully qualified because this crate
    // defines an `ImageDecoder` trait of its own.
    let orientation =
        image::ImageDecoder::orientation(&mut decoder).context("reading orientation")?;

    let mut image = DynamicImage::from_decoder(decoder).context("decoding pixels")?;
    image.apply_orientation(orientation);

    Ok(DecodedImage::from_dynamic(image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempTree;

    #[test]
    fn extensions_map_to_formats_case_insensitively() {
        assert_eq!(ImageFormat::from_extension("PNG"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_extension("Jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_extension("jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_extension("bmp"), None);
    }

    #[test]
    fn every_format_has_a_decoder_for_itself() {
        for format in ImageFormat::ALL {
            assert_eq!(format.decoder().format(), format);
        }
    }

    #[test]
    fn support_follows_the_extension() {
        assert!(is_supported(Path::new("/tmp/a.PNG")));
        assert!(!is_supported(Path::new("/tmp/a.gif")));
        assert!(!is_supported(Path::new("/tmp/a")));
    }

    #[test]
    fn png_decodes_to_bgra_at_the_right_size() {
        let tree = TempTree::new("decode-png");
        // Distinct channels so a swapped or dropped one is obvious.
        let path = tree.image("a.png", 7, 3, [10, 20, 30]);

        let decoded = decode(&path).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (7, 3));
        // GPUI uploads BGRA, so blue comes first and alpha is opaque.
        assert_eq!(decoded.pixel(0, 0), [30, 20, 10, 255]);
        assert_eq!(decoded.pixel(6, 2), [30, 20, 10, 255]);
    }

    #[test]
    fn jpeg_decodes_to_bgra() {
        let tree = TempTree::new("decode-jpeg");
        let path = tree.image("a.jpg", 16, 16, [200, 40, 90]);

        let decoded = decode(&path).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
        let [b, g, r, a] = decoded.pixel(8, 8);
        assert_eq!(a, 255);
        // JPEG is lossy, so allow a small deviation per channel.
        assert!(r.abs_diff(200) < 12, "red was {r}");
        assert!(g.abs_diff(40) < 12, "green was {g}");
        assert!(b.abs_diff(90) < 12, "blue was {b}");
    }

    #[test]
    fn a_corrupt_file_fails_instead_of_panicking() {
        let tree = TempTree::new("corrupt");
        let path = tree.write("broken.png", b"\x89PNG\r\n\x1a\nnot really a png");
        assert!(decode(&path).is_err());
    }

    #[test]
    fn a_missing_file_fails() {
        assert!(decode(Path::new("/nonexistent/none.png")).is_err());
    }

    #[test]
    fn contents_are_never_sniffed_past_the_extension() {
        // A JPEG named .png must fail rather than be displayed, so that the
        // extension stays the single source of truth for the decoder table.
        let tree = TempTree::new("mislabelled");
        let jpeg = tree.image("real.jpg", 8, 8, [1, 2, 3]);
        let bytes = std::fs::read(&jpeg).expect("read");
        let path = tree.write("mislabelled.png", &bytes);
        assert!(decode(&path).is_err());
    }
}
