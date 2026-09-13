use super::BitPlanes;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// NEON (aarch64) transposition of 64 bytes into 8 bit planes.
///
/// Loads 4 × 16-byte chunks, then for each of 8 bit positions:
/// AND with a broadcast bit mask, compare equal (produces 0xFF lanes),
/// then extract a 16-bit mask using a shift-and-add reduction
/// (NEON lacks x86's `pmovmskb`). Combine 4 masks into one u64.
///
/// # Safety
/// Caller must ensure NEON is available (baseline on aarch64).
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub unsafe fn transpose_64(data: &[u8; 64]) -> BitPlanes {
    let ptr = data.as_ptr();

    // SAFETY: `data` is &[u8; 64], so ptr..ptr+64 is valid and readable.
    let v0 = unsafe { vld1q_u8(ptr) };
    let v1 = unsafe { vld1q_u8(ptr.add(16)) };
    let v2 = unsafe { vld1q_u8(ptr.add(32)) };
    let v3 = unsafe { vld1q_u8(ptr.add(48)) };

    // Bit extraction powers: [1, 2, 4, 8, 16, 32, 64, 128] repeated for high lane
    let bit_select: uint8x16_t = unsafe {
        vld1q_u8(
            [1u8, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128].as_ptr(),
        )
    };

    let mut planes = [0u64; 8];

    for bit in 0..8u8 {
        let mask = vdupq_n_u8(1u8 << bit);

        // AND each chunk with the bit mask, then compare
        let c0 = vceqq_u8(vandq_u8(v0, mask), mask);
        let c1 = vceqq_u8(vandq_u8(v1, mask), mask);
        let c2 = vceqq_u8(vandq_u8(v2, mask), mask);
        let c3 = vceqq_u8(vandq_u8(v3, mask), mask);

        // Manual movemask: AND with bit_select, pairwise add to reduce 16 bytes → 2 bytes
        // SAFETY: movemask_neon requires neon target feature, which this function enables.
        let m0 = unsafe { movemask_neon(c0, bit_select) };
        let m1 = unsafe { movemask_neon(c1, bit_select) };
        let m2 = unsafe { movemask_neon(c2, bit_select) };
        let m3 = unsafe { movemask_neon(c3, bit_select) };

        planes[bit as usize] = m0 | (m1 << 16) | (m2 << 32) | (m3 << 48);
    }

    BitPlanes { planes }
}

/// Extract a 16-bit mask from a comparison result vector on NEON.
///
/// Takes a vector where each byte is either 0xFF (set) or 0x00 (clear),
/// and produces a u64 with the corresponding 16 bits.
///
/// Algorithm: AND with [1,2,4,8,16,32,64,128,1,2,4,8,16,32,64,128],
/// then pairwise-add down to 2 bytes, giving low-byte and high-byte of the mask.
///
/// # Safety
/// Caller must ensure NEON is available.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
unsafe fn movemask_neon(cmp: uint8x16_t, bit_select: uint8x16_t) -> u64 {
    // AND: each lane becomes its bit weight (or 0)
    let masked = vandq_u8(cmp, bit_select);

    // Pairwise add: 16 bytes → 8 half-sums (each 0..3 bits worth)
    let p1 = vpaddlq_u8(masked); // 16×u8 → 8×u16

    // Continue reducing: 8×u16 → 4×u32
    let p2 = vpaddlq_u16(p1);

    // 4×u32 → 2×u64
    let p3 = vpaddlq_u32(p2);

    // Extract the two 8-bit masks
    let lo = vgetq_lane_u64(p3, 0) as u64;
    let hi = vgetq_lane_u64(p3, 1) as u64;

    lo | (hi << 8)
}

#[cfg(test)]
mod tests {
    #[cfg(target_arch = "aarch64")]
    use super::*;

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn matches_scalar() {
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
            let neon = unsafe { transpose_64(&data) };

            for bit in 0..8 {
                assert_eq!(
                    neon.planes[bit], scalar.planes[bit],
                    "NEON mismatch at plane {bit} for pattern {pat_idx}",
                );
            }
        }
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn property_transpose() {
        let data: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(13));
        let bp = unsafe { transpose_64(&data) };
        for b in 0..8 {
            for i in 0..64 {
                let expected = ((data[i] >> b) & 1) as u64;
                let actual = (bp.planes[b] >> i) & 1;
                assert_eq!(actual, expected, "NEON mismatch at plane {b}, position {i}");
            }
        }
    }
}
