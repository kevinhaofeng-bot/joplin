use super::BitPlanes;

/// Scalar (non-SIMD) transposition of 64 bytes into 8 bit planes.
pub fn transpose_64(data: &[u8; 64]) -> BitPlanes {
    let mut planes = [0u64; 8];
    for i in 0..64 {
        let byte = data[i];
        planes[0] |= ((byte as u64) & 1) << i;
        planes[1] |= ((byte as u64 >> 1) & 1) << i;
        planes[2] |= ((byte as u64 >> 2) & 1) << i;
        planes[3] |= ((byte as u64 >> 3) & 1) << i;
        planes[4] |= ((byte as u64 >> 4) & 1) << i;
        planes[5] |= ((byte as u64 >> 5) & 1) << i;
        planes[6] |= ((byte as u64 >> 6) & 1) << i;
        planes[7] |= ((byte as u64 >> 7) & 1) << i;
    }
    BitPlanes { planes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zeros() {
        let data = [0u8; 64];
        let bp = transpose_64(&data);
        for plane in &bp.planes {
            assert_eq!(*plane, 0);
        }
    }

    #[test]
    fn all_ones() {
        let data = [0xFFu8; 64];
        let bp = transpose_64(&data);
        for plane in &bp.planes {
            assert_eq!(*plane, u64::MAX);
        }
    }

    #[test]
    fn alternating_bits() {
        // 0x55 = 0101_0101 - bits 0,2,4,6 set
        let data = [0x55u8; 64];
        let bp = transpose_64(&data);
        assert_eq!(bp.planes[0], u64::MAX); // bit 0 set in all bytes
        assert_eq!(bp.planes[1], 0);        // bit 1 clear in all bytes
        assert_eq!(bp.planes[2], u64::MAX); // bit 2 set
        assert_eq!(bp.planes[3], 0);
        assert_eq!(bp.planes[4], u64::MAX);
        assert_eq!(bp.planes[5], 0);
        assert_eq!(bp.planes[6], u64::MAX);
        assert_eq!(bp.planes[7], 0);
    }

    #[test]
    fn single_byte_set() {
        let mut data = [0u8; 64];
        data[7] = 0x3C; // '<' = 0011_1100, bits 2,3,4,5 set
        let bp = transpose_64(&data);
        assert_eq!(bp.planes[0], 0);
        assert_eq!(bp.planes[1], 0);
        assert_eq!(bp.planes[2], 1 << 7); // bit 2 set at position 7
        assert_eq!(bp.planes[3], 1 << 7);
        assert_eq!(bp.planes[4], 1 << 7);
        assert_eq!(bp.planes[5], 1 << 7);
        assert_eq!(bp.planes[6], 0);
        assert_eq!(bp.planes[7], 0);
    }

    #[test]
    fn property_transpose_matches_individual_bits() {
        // Verify the fundamental property: planes[b] bit i == (input[i] >> b) & 1
        let data: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(13));
        let bp = transpose_64(&data);
        for b in 0..8 {
            for i in 0..64 {
                let expected = ((data[i] >> b) & 1) as u64;
                let actual = (bp.planes[b] >> i) & 1;
                assert_eq!(
                    actual, expected,
                    "mismatch at plane {b}, position {i}: byte=0x{:02X}",
                    data[i]
                );
            }
        }
    }
}
