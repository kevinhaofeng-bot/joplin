//! Parallel bounded decoder for plain RGB8 PNG. Other representations stay native.
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) struct ProxyPixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SOURCE_RGB_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn decode_plain_png(path: &Path, budget: usize, max_edge: u32) -> Option<ProxyPixels> {
    if budget < 4 || max_edge == 0 {
        return None;
    }
    let file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > MAX_SOURCE_BYTES {
        return None;
    }
    let mut input = BufReader::new(file);
    let (width, height) = plain_header(&mut input, length)?;
    input.seek(SeekFrom::Start(0)).ok()?;
    // Use the already-resolved PNG library directly: Reader::finish also
    // validates the tail/IEND, unlike image's first-frame-only adapter.
    let decoder = png::Decoder::new_with_limits(
        input,
        png::Limits {
            bytes: MAX_SOURCE_RGB_BYTES as usize,
        },
    );
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.width != width
        || info.height != height
        || info.color_type != png::ColorType::Rgb
        || info.bit_depth != png::BitDepth::Eight
    {
        return None;
    }
    let source_len = usize::try_from(
        u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(3)?,
    )
    .ok()?;
    if reader.output_buffer_size()? != source_len {
        return None;
    }
    let mut source = vec![0u8; source_len];
    let frame = reader.next_frame(&mut source).ok()?;
    if frame.width != width
        || frame.height != height
        || frame.color_type != png::ColorType::Rgb
        || frame.bit_depth != png::BitDepth::Eight
    {
        return None;
    }
    reader.finish().ok()?;
    drop(reader);
    let rgb = image::RgbImage::from_raw(width, height, source)?;
    let edge = max_edge.min(((budget / 4) as f64).sqrt().floor() as u32);
    let scale = (f64::from(edge) / f64::from(width.max(height))).min(1.0);
    let target_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let target_height = (f64::from(height) * scale).round().max(1.0) as u32;
    let output_len = (target_width as usize)
        .checked_mul(target_height as usize)?
        .checked_mul(4)?;
    if output_len > budget {
        return None;
    }
    let resized = if (target_width, target_height) == (width, height) {
        rgb
    } else {
        image::imageops::resize(
            &rgb,
            target_width,
            target_height,
            image::imageops::FilterType::Lanczos3,
        )
    };
    let mut bgra = Vec::new();
    bgra.try_reserve_exact(output_len).ok()?;
    for pixel in resized.pixels() {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    Some(ProxyPixels {
        width: target_width,
        height: target_height,
        bgra,
    })
}

/// Constant-space admission scan, not a second PNG decoder. Unknown chunks,
/// colour/orientation/animation metadata and large source buffers stay native.
/// The PNG library validates chunk checksums and compressed data before publish.
fn plain_header(input: &mut (impl Read + Seek), length: u64) -> Option<(u32, u32)> {
    let mut signature = [0; 8];
    input.read_exact(&mut signature).ok()?;
    if signature != [137, 80, 78, 71, 13, 10, 26, 10] {
        return None;
    }
    let mut dimensions = None;
    let mut saw_data = false;
    loop {
        let mut header = [0; 8];
        input.read_exact(&mut header).ok()?;
        let count = u32::from_be_bytes(header[..4].try_into().ok()?) as u64;
        let end = input
            .stream_position()
            .ok()?
            .checked_add(count)?
            .checked_add(4)?;
        if end > length {
            return None;
        }
        match &header[4..] {
            b"IHDR"
                if dimensions.is_none() && count == 13 && input.stream_position().ok()? == 16 =>
            {
                let mut data = [0; 13];
                input.read_exact(&mut data).ok()?;
                let width = u32::from_be_bytes(data[..4].try_into().ok()?);
                let height = u32::from_be_bytes(data[4..8].try_into().ok()?);
                let rgb_bytes = u64::from(width)
                    .checked_mul(u64::from(height))?
                    .checked_mul(3)?;
                if width == 0
                    || height == 0
                    || width > 16384
                    || height > 16384
                    || rgb_bytes > MAX_SOURCE_RGB_BYTES
                    || data[8..] != [8, 2, 0, 0, 0]
                {
                    return None;
                }
                dimensions = Some((width, height));
            }
            b"IDAT" if dimensions.is_some() => {
                saw_data = true;
            }
            b"IEND" if dimensions.is_some() && saw_data && count == 0 && end == length => {
                return dimensions;
            }
            _ => return None,
        }
        input.seek(SeekFrom::Start(end)).ok()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "joplin-png-proxy-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let path = root.join("source.png");
            fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_dir(self.0.parent().unwrap());
        }
    }
    fn png(width: u32, height: u32, bytes: &[u8], color: image::ExtendedColorType) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(bytes, width, height, color)
            .unwrap();
        out
    }
    fn crc(bytes: &[u8]) -> u32 {
        let mut value = !0u32;
        for byte in bytes {
            value ^= u32::from(*byte);
            for _ in 0..8 {
                value = (value >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(value & 1)));
            }
        }
        !value
    }
    fn with_chunk(source: &[u8], tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = source[..33].to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let mut chunk = tag.to_vec();
        chunk.extend_from_slice(payload);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc(&chunk).to_be_bytes());
        out.extend_from_slice(&source[33..]);
        out
    }

    #[test]
    fn plain_rgb_preserves_literal_channels_rows_and_opaque_alpha() {
        let f = Fixture::new(&png(
            2,
            2,
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 11, 22, 33],
            image::ExtendedColorType::Rgb8,
        ));
        let p = decode_plain_png(&f.0, 16, 1024).expect("plain PNG should use the bounded decoder");
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(
            p.bgra,
            [
                0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 33, 22, 11, 255
            ]
        );
    }
    #[test]
    fn resized_solid_colour_is_bounded_without_colour_or_alpha_changes() {
        let f = Fixture::new(&png(
            80,
            40,
            &[23, 45, 67].repeat(80 * 40),
            image::ExtendedColorType::Rgb8,
        ));
        let p = decode_plain_png(&f.0, 1024, 16).expect("plain PNG should resize");
        assert_eq!((p.width, p.height), (16, 8));
        assert_eq!(p.bgra, [67, 45, 23, 255].repeat(16 * 8));
    }
    #[test]
    fn odd_aspect_ratio_uses_nearest_pixel_dimensions() {
        let f = Fixture::new(&png(
            3,
            5,
            &[23, 45, 67].repeat(15),
            image::ExtendedColorType::Rgb8,
        ));
        let p = decode_plain_png(&f.0, 64, 3).unwrap();
        assert_eq!((p.width, p.height), (2, 3));
    }
    #[test]
    fn output_respects_small_budget_and_does_not_upscale() {
        let f = Fixture::new(&png(
            14,
            14,
            &[23, 45, 67].repeat(196),
            image::ExtendedColorType::Rgb8,
        ));
        let p = decode_plain_png(&f.0, 16, 1024).unwrap();
        assert_eq!((p.width, p.height, p.bgra.len()), (2, 2, 16));
        let f = Fixture::new(&png(1, 1, &[23, 45, 67], image::ExtendedColorType::Rgb8));
        let p = decode_plain_png(&f.0, 4096, 1024).unwrap();
        assert_eq!((p.width, p.height, p.bgra.len()), (1, 1, 4));
    }
    #[test]
    fn colour_direction_animation_and_extra_metadata_stay_on_native_path() {
        let source = png(1, 1, &[23, 45, 67], image::ExtendedColorType::Rgb8);
        for (tag, payload) in [
            (*b"sRGB", vec![0]),
            (*b"gAMA", 100000u32.to_be_bytes().to_vec()),
            (*b"cHRM", vec![0; 32]),
            (*b"eXIf", vec![0; 16]),
            (*b"iCCP", vec![0; 8]),
            (*b"acTL", vec![0; 8]),
            (*b"tRNS", vec![0; 6]),
            (*b"tEXt", b"Title\0hello".to_vec()),
        ] {
            let f = Fixture::new(&with_chunk(&source, &tag, &payload));
            assert!(
                decode_plain_png(&f.0, 4096, 1024).is_none(),
                "metadata {tag:?} must preserve native conversion"
            );
        }
    }
    #[test]
    fn alpha_grayscale_and_sixteen_bit_stay_on_native_path() {
        for (bytes, color) in [
            (vec![23, 45, 67, 128], image::ExtendedColorType::Rgba8),
            (vec![123], image::ExtendedColorType::L8),
            (vec![0, 23, 0, 45, 0, 67], image::ExtendedColorType::Rgb16),
        ] {
            let f = Fixture::new(&png(1, 1, &bytes, color));
            assert!(decode_plain_png(&f.0, 4096, 1024).is_none());
        }
    }
    #[test]
    fn truncated_corrupt_trailing_and_empty_inputs_do_not_publish_pixels() {
        let source = png(
            2,
            2,
            &[23, 45, 67].repeat(4),
            image::ExtendedColorType::Rgb8,
        );
        let mut corrupt = source.clone();
        corrupt[29] ^= 1;
        let mut corrupt_end = source.clone();
        *corrupt_end.last_mut().unwrap() ^= 1;
        let mut trailing = source.clone();
        trailing.push(0);
        for bytes in [
            vec![],
            source[..source.len() - 1].to_vec(),
            corrupt,
            corrupt_end,
            trailing,
        ] {
            let f = Fixture::new(&bytes);
            assert!(decode_plain_png(&f.0, 4096, 1024).is_none());
        }
    }
    #[test]
    fn oversize_source_and_insufficient_output_budget_fall_back_before_decode() {
        let source = png(1, 1, &[23, 45, 67], image::ExtendedColorType::Rgb8);
        let f = Fixture::new(&source);
        assert!(decode_plain_png(&f.0, 3, 1024).is_none());
        assert!(decode_plain_png(&f.0, 4096, 0).is_none());
        let file = fs::OpenOptions::new().write(true).open(&f.0).unwrap();
        file.set_len(MAX_SOURCE_BYTES + 1).unwrap();
        assert!(decode_plain_png(&f.0, 4096, 1024).is_none());
        let mut giant = source.clone();
        giant[16..20].copy_from_slice(&16384u32.to_be_bytes());
        giant[20..24].copy_from_slice(&16384u32.to_be_bytes());
        let checksum = crc(&giant[12..29]);
        giant[29..33].copy_from_slice(&checksum.to_be_bytes());
        let f = Fixture::new(&giant);
        assert!(decode_plain_png(&f.0, 4096, 1024).is_none());
    }
}
