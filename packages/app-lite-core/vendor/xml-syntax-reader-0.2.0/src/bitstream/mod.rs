// Scalar is the fallback on x86/x86_64 (when no SIMD detected) and is used
// as the reference implementation in cross_validate_backends tests on all
// architectures.
#[cfg(any(
    not(target_arch = "aarch64"),
    test,
))]
pub mod scalar;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub mod sse2;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub mod avx2;

#[cfg(target_arch = "aarch64")]
pub mod neon;

/// 8 bit planes extracted from a 64-byte block.
///
/// `planes[b]` has bit `i` set if and only if bit `b` of the input byte at position `i` is set.
/// `planes[0]` is the LSB plane, `planes[7]` is the MSB plane.
#[derive(Debug, Clone, Copy)]
pub struct BitPlanes {
    pub planes: [u64; 8],
}

/// Function pointer type for the transpose operation.
///
/// # Safety
/// The function may require specific CPU features (SSE2, AVX2, NEON).
/// Callers must ensure the selected function matches the available hardware,
/// which `select_transpose()` guarantees.
pub type TransposeFn = unsafe fn(&[u8; 64]) -> BitPlanes;

/// Select the best available transposition implementation for the current platform.
///
/// Probes CPU features at runtime and returns the fastest safe option:
/// - x86-64/x86: AVX2 > SSE2 > scalar
/// - aarch64: NEON > scalar
/// - everything else: scalar
///
/// This should be called once (e.g. in `Reader::new()`) and the result cached.
pub fn select_transpose() -> TransposeFn {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        // Runtime feature detection requires std; in no_std, use compile-time target_feature.
        #[cfg(feature = "std")]
        {
            if std::is_x86_feature_detected!("avx2") {
                return avx2::transpose_64;
            }
            if std::is_x86_feature_detected!("sse2") {
                return sse2::transpose_64;
            }
        }
        #[cfg(not(feature = "std"))]
        {
            if cfg!(target_feature = "avx2") {
                return avx2::transpose_64;
            }
            if cfg!(target_feature = "sse2") {
                return sse2::transpose_64;
            }
        }
    }

    // NEON is always available on aarch64; other architectures fall back to scalar.
    #[cfg(target_arch = "aarch64")]
    return neon::transpose_64;

    #[cfg(not(target_arch = "aarch64"))]
    scalar::transpose_64
}

/// Transpose a block of input into bit planes.
///
/// Handles both full 64-byte blocks and partial blocks (< 64 bytes).
/// For partial blocks, the input is padded with zeros on a stack buffer.
///
/// Returns the `BitPlanes` and the number of valid bytes in the block.
#[inline]
pub fn transpose_block(transpose: TransposeFn, data: &[u8], offset: usize) -> (BitPlanes, usize) {
    let remaining = data.len() - offset;
    if remaining >= 64 {
        // SAFETY: remaining >= 64 guarantees offset + 64 <= data.len(), so this
        // pointer range is within bounds. select_transpose() guarantees the function
        // matches available hardware.
        let block: &[u8; 64] = unsafe { &*(data.as_ptr().add(offset) as *const [u8; 64]) };
        let planes = unsafe { transpose(block) };
        (planes, 64)
    } else {
        // Pad to 64 bytes on the stack
        let mut padded = [0u8; 64];
        padded[..remaining].copy_from_slice(&data[offset..]);
        // SAFETY: select_transpose() guarantees the function matches available hardware.
        let planes = unsafe { transpose(&padded) };
        (planes, remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpose_block_full() {
        let data = [0x42u8; 128];
        let (bp, len) = transpose_block(select_transpose(), &data, 0);
        assert_eq!(len, 64);
        // 0x42 = 0100_0010, bits 1 and 6 set
        assert_eq!(bp.planes[0], 0);
        assert_eq!(bp.planes[1], u64::MAX);
        assert_eq!(bp.planes[6], u64::MAX);
        assert_eq!(bp.planes[7], 0);
    }

    #[test]
    fn transpose_block_partial() {
        let data = [0xFFu8; 10];
        let (bp, len) = transpose_block(select_transpose(), &data, 0);
        assert_eq!(len, 10);
        // First 10 bits set, rest zero (from padding)
        let expected = (1u64 << 10) - 1;
        for plane in &bp.planes {
            assert_eq!(*plane, expected);
        }
    }

    #[test]
    fn transpose_block_with_offset() {
        let mut data = [0u8; 80];
        for b in &mut data[64..] {
            *b = 0xFF;
        }
        let (bp, len) = transpose_block(select_transpose(), &data, 64);
        assert_eq!(len, 16);
        let expected = (1u64 << 16) - 1;
        for plane in &bp.planes {
            assert_eq!(*plane, expected);
        }
    }

    /// Cross-validate all available backends against scalar with randomised data.
    #[test]
    fn cross_validate_backends() {
        // Generate several test vectors with different bit patterns
        let test_vectors: [[u8; 64]; 8] = [
            [0x00; 64],
            [0xFF; 64],
            [0x55; 64],
            [0xAA; 64],
            core::array::from_fn(|i| i as u8),
            core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(13)),
            core::array::from_fn(|i| (i as u8).wrapping_mul(127).wrapping_add(97)),
            core::array::from_fn(|i| {
                // Mix of XML-significant bytes
                match i % 8 {
                    0 => b'<',
                    1 => b'>',
                    2 => b'&',
                    3 => b'"',
                    4 => b' ',
                    5 => b'/',
                    6 => b'=',
                    _ => b'a',
                }
            }),
        ];

        for (vec_idx, data) in test_vectors.iter().enumerate() {
            let scalar = scalar::transpose_64(data);

            // Test whichever SIMD backends are available
            #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
            {
                if std::is_x86_feature_detected!("sse2") {
                    let simd = unsafe { sse2::transpose_64(data) };
                    for bit in 0..8 {
                        assert_eq!(
                            simd.planes[bit], scalar.planes[bit],
                            "SSE2 != scalar at plane {bit}, vector {vec_idx}",
                        );
                    }
                }
                if std::is_x86_feature_detected!("avx2") {
                    let simd = unsafe { avx2::transpose_64(data) };
                    for bit in 0..8 {
                        assert_eq!(
                            simd.planes[bit], scalar.planes[bit],
                            "AVX2 != scalar at plane {bit}, vector {vec_idx}",
                        );
                    }
                }
            }

            #[cfg(target_arch = "aarch64")]
            {
                let simd = unsafe { neon::transpose_64(data) };
                for bit in 0..8 {
                    assert_eq!(
                        simd.planes[bit], scalar.planes[bit],
                        "NEON != scalar at plane {bit}, vector {vec_idx}",
                    );
                }
            }
        }
    }

    /// Verify that select_transpose() returns a non-scalar backend on capable hardware.
    #[test]
    fn select_transpose_picks_simd() {
        let f = select_transpose();

        // Verify it produces correct results regardless of which backend was picked
        let data: [u8; 64] = core::array::from_fn(|i| i as u8);
        // SAFETY: select_transpose() picks a function matching available hardware.
        let bp = unsafe { f(&data) };
        let scalar = scalar::transpose_64(&data);

        for bit in 0..8 {
            assert_eq!(bp.planes[bit], scalar.planes[bit],
                "select_transpose() result differs from scalar at plane {bit}");
        }
    }
}
