//! Coordinate mapping and PNG downscaling for `computer_use`.
//!
//! A model sees a downscaled screenshot but must speak in the logical-point
//! space the OS accepts. Two conversions bridge that gap and both live here so
//! they can be tested without a display: [`scaled_image_size`] decides the
//! capture's delivered size, and [`image_to_logical`] maps a reported pixel back
//! to a global logical point. [`resize_png_to`] performs the matching decode and
//! re-encode.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat};

use super::backend::{DisplayInfo, LogicalPoint};

/// The eight bytes every PNG stream starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Why a scaling or coordinate conversion failed.
#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    /// A display reported a size that cannot be scaled from.
    #[error("display dimensions must be finite and positive")]
    InvalidDisplayDimensions,
    /// A reported screenshot pixel was NaN or infinite.
    #[error("coordinate must be a finite number")]
    NonFiniteCoordinate,
    /// A reported screenshot pixel fell outside the delivered image.
    #[error("coordinate ({x}, {y}) is outside the {width}x{height} screenshot")]
    OutOfBounds {
        x: f64,
        y: f64,
        width: u32,
        height: u32,
    },
    /// The bytes were not a PNG this build can decode, or could not be encoded.
    #[error("invalid PNG: {0}")]
    InvalidPng(String),
}

/// The size a screenshot of `logical_w` x `logical_h` is delivered at.
///
/// The long edge is capped at `max_dimension` and the aspect ratio is
/// preserved. Small displays are never upscaled: a capture already inside the
/// cap is delivered at its own size.
pub fn scaled_image_size(
    logical_w: f64,
    logical_h: f64,
    max_dimension: u32,
) -> Result<(u32, u32), ScaleError> {
    if max_dimension == 0 || !logical_w.is_finite() || !logical_h.is_finite() {
        return Err(ScaleError::InvalidDisplayDimensions);
    }
    if logical_w <= 0.0 || logical_h <= 0.0 {
        return Err(ScaleError::InvalidDisplayDimensions);
    }
    let scale = (f64::from(max_dimension) / logical_w.max(logical_h)).min(1.0);
    Ok((
        (logical_w * scale).round().max(1.0) as u32,
        (logical_h * scale).round().max(1.0) as u32,
    ))
}

/// Map a pixel in the delivered screenshot to a global logical point.
///
/// `x` and `y` are pixel coordinates in the downscaled image produced by
/// [`scaled_image_size`] for this display, so the mapping first rescales them to
/// the display's logical size and then offsets them by the display's origin. A
/// secondary display placed left of or above the primary one therefore yields
/// negative logical coordinates, which is what the OS expects.
pub fn image_to_logical(
    display: &DisplayInfo,
    x: f64,
    y: f64,
    max_dimension: u32,
) -> Result<LogicalPoint, ScaleError> {
    if !x.is_finite() || !y.is_finite() {
        return Err(ScaleError::NonFiniteCoordinate);
    }
    let (img_w, img_h) = scaled_image_size(display.logical_w, display.logical_h, max_dimension)?;
    if x < 0.0 || y < 0.0 || x >= f64::from(img_w) || y >= f64::from(img_h) {
        return Err(ScaleError::OutOfBounds {
            x,
            y,
            width: img_w,
            height: img_h,
        });
    }
    Ok(LogicalPoint {
        x: display.origin_x + (x / f64::from(img_w)) * display.logical_w,
        y: display.origin_y + (y / f64::from(img_h)) * display.logical_h,
    })
}

/// Decode `raw_png` and re-encode it at exactly `target_w` x `target_h`.
///
/// A capture that already has the target size is returned unchanged after its
/// signature is checked, which keeps the pixel data bit-identical and skips a
/// pointless decode-and-re-encode round trip.
pub fn resize_png_to(raw_png: &[u8], target_w: u32, target_h: u32) -> Result<Vec<u8>, ScaleError> {
    if target_w == 0 || target_h == 0 {
        return Err(ScaleError::InvalidDisplayDimensions);
    }
    if !raw_png.starts_with(&PNG_SIGNATURE) {
        return Err(ScaleError::InvalidPng("missing PNG signature".to_string()));
    }
    let img = image::load_from_memory_with_format(raw_png, ImageFormat::Png)
        .map_err(|error| ScaleError::InvalidPng(error.to_string()))?;
    if (img.width(), img.height()) == (target_w, target_h) {
        return Ok(raw_png.to_vec());
    }
    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);
    encode_png(&resized)
}

/// Encode `img` as PNG bytes.
fn encode_png(img: &DynamicImage) -> Result<Vec<u8>, ScaleError> {
    let mut buffer = Cursor::new(Vec::new());
    img.write_to(&mut buffer, ImageFormat::Png)
        .map_err(|error| ScaleError::InvalidPng(error.to_string()))?;
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A display of the given size whose origin is `(origin_x, origin_y)`.
    fn display(
        index: u32,
        origin_x: f64,
        origin_y: f64,
        logical_w: f64,
        logical_h: f64,
    ) -> DisplayInfo {
        DisplayInfo {
            index,
            native_id: index + 1,
            origin_x,
            origin_y,
            logical_w,
            logical_h,
            is_primary: index == 0,
        }
    }

    /// Encode a `width` x `height` RGBA gradient as PNG bytes.
    fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        let img = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        img.write_to(&mut buffer, ImageFormat::Png)
            .expect("synthetic PNG encodes");
        buffer.into_inner()
    }

    /// The pixel dimensions of `png`, decoded through the same codec the tool uses.
    fn png_dimensions(png: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory_with_format(png, ImageFormat::Png)
            .expect("re-encoded PNG decodes");
        (img.width(), img.height())
    }

    #[test]
    fn scaled_image_size_preserves_aspect_and_never_upscales() {
        // A 1512x982 laptop panel shrinks onto the 1280 long edge.
        assert_eq!(
            scaled_image_size(1512.0, 982.0, 1280).expect("valid dimensions"),
            (1280, 831)
        );
        // A 3840x2160 4K panel keeps its 16:9 aspect at the cap.
        assert_eq!(
            scaled_image_size(3840.0, 2160.0, 1280).expect("valid dimensions"),
            (1280, 720)
        );
        // A display already under the cap is delivered at its own size.
        assert_eq!(
            scaled_image_size(800.0, 600.0, 1280).expect("valid dimensions"),
            (800, 600)
        );
        // Portrait orientation caps the long edge, not the width.
        assert_eq!(
            scaled_image_size(1080.0, 1920.0, 1280).expect("valid dimensions"),
            (720, 1280)
        );
        // A degenerate one-point display still yields a non-zero image.
        assert_eq!(
            scaled_image_size(0.4, 0.4, 1280).expect("valid dimensions"),
            (1, 1)
        );
    }

    #[test]
    fn scaled_image_size_rejects_invalid_inputs() {
        for (w, h, max) in [
            (f64::NAN, 982.0, 1280),
            (1512.0, f64::NAN, 1280),
            (f64::INFINITY, 982.0, 1280),
            (1512.0, f64::NEG_INFINITY, 1280),
            (0.0, 982.0, 1280),
            (1512.0, 0.0, 1280),
            (-1512.0, 982.0, 1280),
            (1512.0, -982.0, 1280),
            (1512.0, 982.0, 0),
        ] {
            assert!(
                matches!(
                    scaled_image_size(w, h, max),
                    Err(ScaleError::InvalidDisplayDimensions)
                ),
                "({w}, {h}) at max {max} must be rejected"
            );
        }
    }

    #[test]
    fn image_to_logical_maps_primary_and_negative_secondary_origins() {
        // The primary display at the global origin: a centered screenshot pixel
        // lands in the middle of the panel.
        let primary = display(0, 0.0, 0.0, 1512.0, 982.0);
        assert_eq!(
            scaled_image_size(primary.logical_w, primary.logical_h, 1280).expect("valid"),
            (1280, 831)
        );
        let center = image_to_logical(&primary, 640.0, 415.0, 1280).expect("in bounds");
        assert!((center.x - 756.0).abs() < 0.5, "x was {}", center.x);
        assert!((center.y - 490.5).abs() < 0.5, "y was {}", center.y);

        // The top-left pixel maps exactly onto the display origin.
        let origin = image_to_logical(&primary, 0.0, 0.0, 1280).expect("in bounds");
        assert_eq!(origin, LogicalPoint { x: 0.0, y: 0.0 });

        // A secondary display placed left of and above the primary one: its
        // origin is negative, so its top-left corner is too, and the whole
        // display spans from that origin to origin + logical size.
        let secondary = display(1, -1920.0, -120.0, 1920.0, 1080.0);
        assert_eq!(
            scaled_image_size(secondary.logical_w, secondary.logical_h, 1280).expect("valid"),
            (1280, 720)
        );
        let top_left = image_to_logical(&secondary, 0.0, 0.0, 1280).expect("in bounds");
        assert_eq!(
            top_left,
            LogicalPoint {
                x: -1920.0,
                y: -120.0
            }
        );

        let bottom_right = image_to_logical(&secondary, 1279.0, 719.0, 1280).expect("in bounds");
        assert!(
            bottom_right.x > -1920.0 && bottom_right.x < 0.0,
            "x was {}",
            bottom_right.x
        );
        assert!(
            bottom_right.y > -120.0 && bottom_right.y < 960.0,
            "y was {}",
            bottom_right.y
        );

        // The right-hand edge of the secondary display approaches the primary
        // display's left edge.
        let nearly_edge = image_to_logical(&secondary, 1279.0, 0.0, 1280).expect("in bounds");
        assert!(
            nearly_edge.x > -2.0 && nearly_edge.x < 0.0,
            "x was {}",
            nearly_edge.x
        );

        // The bottom edge of the secondary display approaches its own bottom.
        let nearly_bottom = image_to_logical(&secondary, 0.0, 719.0, 1280).expect("in bounds");
        assert!(
            nearly_bottom.y > 957.0 && nearly_bottom.y < 960.0,
            "y was {}",
            nearly_bottom.y
        );
    }

    #[test]
    fn image_to_logical_rejects_nan_inf_negative_and_out_of_bounds() {
        let primary = display(0, 0.0, 0.0, 1512.0, 982.0);
        for (x, y) in [(f64::NAN, 10.0), (10.0, f64::NAN), (f64::INFINITY, 10.0)] {
            assert!(
                matches!(
                    image_to_logical(&primary, x, y, 1280),
                    Err(ScaleError::NonFiniteCoordinate)
                ),
                "({x}, {y}) must be rejected as non-finite"
            );
        }
        for (x, y) in [
            (-1.0, 10.0),
            (10.0, -1.0),
            (1280.0, 0.0),
            (0.0, 831.0),
            (5000.0, 10.0),
        ] {
            let error = image_to_logical(&primary, x, y, 1280).expect_err("must be rejected");
            assert!(
                matches!(
                    error,
                    ScaleError::OutOfBounds {
                        width: 1280,
                        height: 831,
                        ..
                    }
                ),
                "({x}, {y}) must be out of bounds, got {error}"
            );
        }
        // The last deliverable pixel is still inside the image.
        assert!(image_to_logical(&primary, 1279.0, 830.0, 1280).is_ok());
        // A display whose dimensions cannot be scaled from fails the same way.
        let broken = display(0, 0.0, 0.0, 0.0, 982.0);
        assert!(matches!(
            image_to_logical(&broken, 1.0, 1.0, 1280),
            Err(ScaleError::InvalidDisplayDimensions)
        ));
    }

    #[test]
    fn resize_png_to_downscales_synthetic_png_and_rejects_corrupt_bytes() {
        let raw = synthetic_png(64, 32);
        assert_eq!(png_dimensions(&raw), (64, 32));

        let downscaled = resize_png_to(&raw, 16, 8).expect("valid PNG downscales");
        assert_eq!(png_dimensions(&downscaled), (16, 8));
        assert!(downscaled.starts_with(&PNG_SIGNATURE));

        // A capture already at the target size is passed through untouched.
        assert_eq!(
            resize_png_to(&raw, 64, 32).expect("matching size passes through"),
            raw
        );

        // Upscaling is allowed here: the caller decides the target size, and the
        // function only refuses impossible dimensions.
        assert_eq!(
            png_dimensions(&resize_png_to(&raw, 128, 64).expect("valid PNG upscales")),
            (128, 64)
        );

        // Zero target dimensions are a caller error, not corrupt input.
        for (w, h) in [(0, 8), (16, 0), (0, 0)] {
            assert!(
                matches!(
                    resize_png_to(&raw, w, h),
                    Err(ScaleError::InvalidDisplayDimensions)
                ),
                "target {w}x{h} must be rejected"
            );
        }

        // No PNG signature.
        assert!(matches!(
            resize_png_to(b"not a png at all", 16, 8),
            Err(ScaleError::InvalidPng(_))
        ));
        assert!(matches!(
            resize_png_to(&[], 16, 8),
            Err(ScaleError::InvalidPng(_))
        ));

        // A valid signature with a truncated body cannot be decoded.
        let truncated = &raw[..12];
        assert!(matches!(
            resize_png_to(truncated, 16, 8),
            Err(ScaleError::InvalidPng(_))
        ));
        // Signature plus garbage.
        let mut garbage = PNG_SIGNATURE.to_vec();
        garbage.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert!(matches!(
            resize_png_to(&garbage, 16, 8),
            Err(ScaleError::InvalidPng(_))
        ));
    }
}
