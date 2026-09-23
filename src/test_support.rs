//! Helpers shared by the unit tests: a self-cleaning directory tree that can
//! be filled with real PNG and JPEG files.

#![cfg(test)]

use std::path::PathBuf;

use image::{ImageFormat, Rgb, RgbImage};

/// A directory under the system temp dir that removes itself on drop.
pub struct TempTree(PathBuf);

impl TempTree {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "imageviewer-test-{}-{}-{:?}",
            name,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        Self(root)
    }


    pub fn dir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).expect("create dir");
        path
    }

    /// Writes a file with arbitrary bytes, creating parent directories.
    pub fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, contents).expect("write file");
        path
    }

    /// Writes a real single-colour image, encoded from the extension.
    pub fn image(&self, relative: &str, width: u32, height: u32, rgb: [u8; 3]) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }

        let buffer = RgbImage::from_pixel(width, height, Rgb(rgb));
        let format = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => ImageFormat::Png,
            Some("jpg") | Some("jpeg") => ImageFormat::Jpeg,
            other => panic!("unsupported test image extension: {other:?}"),
        };

        buffer.save_with_format(&path, format).expect("encode image");
        path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
