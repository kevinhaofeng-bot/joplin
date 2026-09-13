use super::BitPlanes;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[cfg(target_arch = "x86")]
use core::arch::x86::*;

/// AVX2 transposition of 64 bytes into 8 bit planes.
///
/// Loads 2 × 32-byte chunks, then for each of 8 bit positions:
/// AND with a broadcast bit mask, compare equal (produces 0xFF lanes),
/// `_mm256_movemask_epi8` → 32-bit mask. Combine 2 masks into one u64.
///
/// Total: 2 loads + 8 × (2 AND + 2 CMPEQ + 2 MOVEMASK + shift/OR) ≈ 50 ops.
/// Roughly 2× fewer operations than SSE2.
///
/// # Safety
/// Caller must ensure AVX2 is available (checked at runtime).
#[target_feature(enable = "avx2")]
pub unsafe fn transpose_64(data: &[u8; 64]) -> BitPlanes {
    let ptr = data.as_ptr();

    // SAFETY: `data` is &[u8; 64], so ptr..ptr+64 is valid and readable.
    let v0 = unsafe { _mm256_loadu_si256(ptr as *const __m256i) };
    let v1 = unsafe { _mm256_loadu_si256(ptr.add(32) as *const __m256i) };

    let mut planes = [0u64; 8];

    for bit in 0..8u8 {
        let mask = _mm256_set1_epi8(1i8 << bit);

        // AND each chunk with the bit mask
        let a0 = _mm256_and_si256(v0, mask);
        let a1 = _mm256_and_si256(v1, mask);

        // Compare equal: lanes that had the bit set become 0xFF, others 0x00
        let c0 = _mm256_cmpeq_epi8(a0, mask);
        let c1 = _mm256_cmpeq_epi8(a1, mask);

        // Extract MSB of each byte → 32-bit mask
        let m0 = _mm256_movemask_epi8(c0) as u32 as u64;
        let m1 = _mm256_movemask_epi8(c1) as u32 as u64;

        // Combine into a single u64 (low bits = earlier bytes)
        planes[bit as usize] = m0 | (m1 << 32);
    }

    BitPlanes { planes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_avx2_available() -> bool {
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        {
            std::is_x86_feature_detected!("avx2")
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
        {
            false
        }
    }

    #[test]
    fn matches_scalar() {
        if !is_avx2_available() {
            return;
        }

        let patterns: &[fn(usize) -> u8] = &[
            |_| 0x00,
            |_| 0xFF,
            |i| i as u8,
            |i| (i as u8).wrapping_mul(37).wrapping_add(13),
            |i| 0x55 ^ (i as u8),
            |i| if i < 32 { 0x3C } else { 0x26 },
        ];

        for (pat_idx, pat_fn) in patterns.iter().enumerate() {
            let data: [u8; 64] = core::array::from_fn(|i| pat_fn(i));
            let scalar = crate::bitstream::scalar::transpose_64(&data);
            let avx2 = unsafe { transpose_64(&data) };

            for bit in 0..8 {
                assert_eq!(
                    avx2.planes[bit], scalar.planes[bit],
                    "AVX2 mismatch at plane {bit} for pattern {pat_idx}",
                );
            }
        }
    }

    #[test]
    fn property_transpose() {
        if !is_avx2_available() {
            return;
        }

        let data: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(13));
        let bp = unsafe { transpose_64(&data) };
        for b in 0..8 {
            for i in 0..64 {
                let expected = ((data[i] >> b) & 1) as u64;
                let actual = (bp.planes[b] >> i) & 1;
                assert_eq!(actual, expected, "AVX2 mismatch at plane {b}, position {i}");
            }
        }
    }
}
