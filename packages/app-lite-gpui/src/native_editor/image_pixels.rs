//! Strict opaque thumbnail copy. Unsupported representations use CoreGraphics.

#[derive(Clone, Copy)]
pub(crate) struct ProviderFormat {
    pub bits_per_component: usize,
    pub bits_per_pixel: usize,
    pub bitmap_info: u32,
    pub canonical_srgb: bool,
    pub has_decode: bool,
}

pub(crate) fn copy_opaque_xrgb(
    bytes: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    budget: usize,
    format: ProviderFormat,
) -> Option<Vec<u8>> {
    let (output_len, source_len) = provider_storage(width, height, stride, budget, format)?;
    if bytes.len() != source_len {
        return None;
    }
    let mut pixels = Vec::with_capacity(output_len);
    for row in bytes.chunks_exact(stride) {
        for pixel in row[..width * 4].chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[3], pixel[2], pixel[1], 255]);
        }
    }
    Some(pixels)
}

/// Check before asking CoreGraphics to copy its provider, not after allocating.
pub(crate) fn provider_storage(
    width: usize,
    height: usize,
    stride: usize,
    budget: usize,
    format: ProviderFormat,
) -> Option<(usize, usize)> {
    // Exactly default-byte-order opaque XRGB8 in the canonical sRGB space.
    // All alpha, ICC, floating-point and decode-array conversions stay native.
    if width == 0
        || height == 0
        || !format.canonical_srgb
        || format.has_decode
        || format.bits_per_component != 8
        || format.bits_per_pixel != 32
        || format.bitmap_info != 6
    {
        return None;
    }
    let row_len = width.checked_mul(4)?;
    let output_len = row_len.checked_mul(height)?;
    if output_len > budget || stride < row_len || stride > row_len.checked_add(256)? {
        return None;
    }
    Some((output_len, stride.checked_mul(height)?))
}

#[cfg(target_os = "macos")]
pub(crate) fn copy_cgimage(image: &objc2_core_graphics::CGImage, budget: usize) -> Option<Vec<u8>> {
    use objc2_core_graphics::{CGColorSpace, CGDataProvider, CGImage, kCGColorSpaceSRGB};
    let space = CGImage::color_space(Some(image))?;
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let format = ProviderFormat {
        bits_per_component: CGImage::bits_per_component(Some(image)),
        bits_per_pixel: CGImage::bits_per_pixel(Some(image)),
        bitmap_info: CGImage::bitmap_info(Some(image)).0,
        canonical_srgb: *space == *srgb,
        has_decode: !CGImage::decode(Some(image)).is_null(),
    };
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let stride = CGImage::bytes_per_row(Some(image));
    provider_storage(width, height, stride, budget, format)?;
    let provider = CGImage::data_provider(Some(image))?;
    let data = CGDataProvider::data(Some(&provider))?;
    // SAFETY: CopyData returns an immutable CFData retained locally. Neither
    // this function nor another consumer exposes a mutation of it; copying
    // finishes before data/provider/image ownership leaves this scope.
    copy_opaque_xrgb(
        unsafe { data.as_bytes_unchecked() },
        width,
        height,
        stride,
        budget,
        format,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque() -> ProviderFormat {
        ProviderFormat {
            bits_per_component: 8,
            bits_per_pixel: 32,
            bitmap_info: 6,
            canonical_srgb: true,
            has_decode: false,
        }
    }

    #[test]
    fn copies_channels_and_rows_without_using_skip_bytes_as_alpha() {
        let src = [
            0, 10, 20, 30, 77, 40, 50, 60, 99, 70, 80, 90, 255, 100, 110, 120,
        ];
        assert_eq!(
            copy_opaque_xrgb(&src, 2, 2, 8, 16, opaque()),
            Some(vec![
                30, 20, 10, 255, 60, 50, 40, 255, 90, 80, 70, 255, 120, 110, 100, 255
            ])
        );
    }

    #[test]
    fn ignores_row_padding_without_flipping_rows() {
        let src = [0, 1, 2, 3, 8, 8, 8, 8, 0, 4, 5, 6, 9, 9, 9, 9];
        assert_eq!(
            copy_opaque_xrgb(&src, 1, 2, 8, 8, opaque()),
            Some(vec![3, 2, 1, 255, 6, 5, 4, 255])
        );
    }

    #[test]
    fn rejects_noncanonical_color_decode_alpha_order_or_depth() {
        let src = [0, 1, 2, 3];
        for format in [
            ProviderFormat {
                canonical_srgb: false,
                ..opaque()
            },
            ProviderFormat {
                has_decode: true,
                ..opaque()
            },
            ProviderFormat {
                bitmap_info: 5,
                ..opaque()
            },
            ProviderFormat {
                bitmap_info: 2,
                ..opaque()
            },
            ProviderFormat {
                bitmap_info: 0x2006,
                ..opaque()
            },
            ProviderFormat {
                bits_per_component: 16,
                ..opaque()
            },
            ProviderFormat {
                bits_per_pixel: 24,
                ..opaque()
            },
        ] {
            assert!(copy_opaque_xrgb(&src, 1, 1, 4, 4, format).is_none());
        }
    }

    #[test]
    fn refuses_truncated_extra_or_excessive_provider_storage() {
        assert!(copy_opaque_xrgb(&[0, 1, 2], 1, 1, 4, 4, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[0, 1, 2, 3, 4], 1, 1, 4, 4, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[0; 8], 2, 1, 4, 8, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[0; 512], 1, 1, 512, 4, opaque()).is_none());
    }

    #[test]
    fn refuses_zero_overflow_and_output_over_budget() {
        assert!(copy_opaque_xrgb(&[], 0, 1, 0, 4, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[], usize::MAX, 2, 4, usize::MAX, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[0; 8], 2, 1, 8, 7, opaque()).is_none());
        assert!(copy_opaque_xrgb(&[], 1, usize::MAX, 4, usize::MAX, opaque()).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_opaque_srgb_copy_keeps_pixels_and_rejects_color_conversion() {
        use objc2_core_foundation::CFData;
        use objc2_core_graphics::{
            CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
            kCGColorSpaceDisplayP3, kCGColorSpaceSRGB,
        };
        let raw = CFData::from_bytes(&[
            0, 10, 20, 30, 77, 40, 50, 60, 99, 70, 80, 90, 255, 100, 110, 120,
        ]);
        let provider = CGDataProvider::with_cf_data(Some(&raw)).unwrap();
        let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
        let p3 = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceDisplayP3 })).unwrap();
        let make = |space: &CGColorSpace, bitmap| unsafe {
            CGImage::new(
                2,
                2,
                8,
                32,
                8,
                Some(space),
                CGBitmapInfo(bitmap),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
            .unwrap()
        };
        let image = make(&srgb, 6);
        assert_eq!(
            copy_cgimage(&image, 16),
            Some(vec![
                30, 20, 10, 255, 60, 50, 40, 255, 90, 80, 70, 255, 120, 110, 100, 255
            ])
        );
        assert!(copy_cgimage(&image, 15).is_none());
        assert!(copy_cgimage(&make(&p3, 6), 16).is_none());
        assert!(copy_cgimage(&make(&srgb, 2), 16).is_none());
    }
}
