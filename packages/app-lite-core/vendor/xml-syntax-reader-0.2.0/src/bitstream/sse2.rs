use super::BitPlanes;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[cfg(target_arch = "x86")]
use core::arch::x86::*;

/// SSE2 transposition of 64 bytes into 8 bit planes.
///
/// Loads 4 × 16-byte chunks, then for each of 8 bit positions:
/// AND with a broadcast bit mask, compare equal (produces 0xFF lanes),
/// `_mm_movemask_epi8` → 16-bit mask. Combine 4 masks into one u64.
///
/// Total: 4 loads + 8 × (4 AND + 4 CMPEQ + 4 MOVEMASK + shifts/ORs) ≈ 100 ops.
///
/// # Safety
/// Caller must ensure SSE2 is available (baseline on x86-64).
#[target_feature(enable = "sse2")]
pub unsafe fn transpose_64(data: &[u8; 64]) -> BitPlanes {
    let ptr = data.as_ptr();

    // SAFETY: `data` is &[u8; 64], so ptr..ptr+64 is valid and readable.
    let v0 = unsafe { _mm_loadu_si128(ptr as *const __m128i) };
    let v1 = unsafe { _mm_loadu_si128(ptr.add(16) as *const __m128i) };
    let v2 = unsafe { _mm_loadu_si128(ptr.add(32) as *const __m128i) };
    let v3 = unsafe { _mm_loadu_si128(ptr.add(48) as *const __m128i) };

    let mut planes = [0u64; 8];

    for bit in 0..8u8 {
        let mask = _mm_set1_epi8(1i8 << bit);

        // AND each chunk with the bit mask
        let a0 = _mm_and_si128(v0, mask);
        let a1 = _mm_and_si128(v1, mask);
        let a2 = _mm_and_si128(v2, mask);
        let a3 = _mm_and_si128(v3, mask);

        // Compare equal: lanes that had the bit set become 0xFF, others 0x00
        let c0 = _mm_cmpeq_epi8(a0, mask);
        let c1 = _mm_cmpeq_epi8(a1, mask);
        let c2 = _mm_cmpeq_epi8(a2, mask);
        let c3 = _mm_cmpeq_epi8(a3, mask);

        // Extract MSB of each byte → 16-bit mask
        let m0 = _mm_movemask_epi8(c0) as u64;
        let m1 = _mm_movemask_epi8(c1) as u64;
        let m2 = _mm_movemask_epi8(c2) as u64;
        let m3 = _mm_movemask_epi8(c3) as u64;

        // Combine into a single u64 (low bits = earlier bytes)
        planes[bit as usize] = m0 | (m1 << 16) | (m2 << 32) | (m3 << 48);
    }

    BitPlanes { planes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_sse2_available() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            // SSE2 is always available on x86-64
            true
        }
        #[cfg(target_arch = "x86")]
        {
            std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
        {
            false
        }
    }

    #[test]
    fn matches_scalar() {
        if !is_sse2_available() {
            return;
        }

        // Test with multiple patterns
        let patterns: &[fn(usize) -> u8] = &[
            |_| 0x00,
            |_| 0xFF,
            |i| i as u8,
            |i| (i as u8).wrapping_mul(37).wrapping_add(13),
            |i| 0x55 ^ (i as u8),
            |i| if i < 32 { 0x3C } else { 0x26 }, // '<' and '&'
        ];

        for (pat_idx, pat_fn) in patterns.iter().enumerate() {
            let data: [u8; 64] = core::array::from_fn(|i| pat_fn(i));
            let scalar = crate::bitstream::scalar::transpose_64(&data);
            let sse2 = unsafe { transpose_64(&data) };

            for bit in 0..8 {
                assert_eq!(
                    sse2.planes[bit], scalar.planes[bit],
                    "SSE2 mismatch at plane {bit} for pattern {pat_idx}",
                );
            }
        }
    }

    #[test]
    fn property_transpose() {
        if !is_sse2_available() {
            return;
        }

        let data: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(13));
        let bp = unsafe { transpose_64(&data) };
        for b in 0..8 {
            for i in 0..64 {
                let expected = ((data[i] >> b) & 1) as u64;
                let actual = (bp.planes[b] >> i) & 1;
                assert_eq!(actual, expected, "SSE2 mismatch at plane {b}, position {i}");
            }
        }
    }
}
