use crate::{DatabenderError, FilterSpec, Result};

pub fn apply(filters: &[FilterSpec], payload: &mut [u8], seed: u64) -> Result<()> {
    for (index, filter) in filters.iter().enumerate() {
        let mut random = Random::new(seed.wrapping_add(index as u64));
        match filter {
            FilterSpec::ByteNoise { probability } => noise(payload, *probability, &mut random),
            FilterSpec::ByteRepeat { count } => repeat(payload, *count, &mut random),
            FilterSpec::ByteDrop { count } => drop_bytes(payload, *count, &mut random),
            FilterSpec::ByteSwap { count } => swap(payload, *count, &mut random),
            _ => {
                return Err(DatabenderError::OutputValidation {
                    reason: format!("filter {} is not an encoded-payload filter", filter.name()),
                })
            }
        }
    }
    Ok(())
}

fn noise(payload: &mut [u8], probability: f64, random: &mut Random) {
    for byte in payload {
        if random.fraction() < probability {
            *byte ^= 1 << (random.next() % 8);
        }
    }
}

fn repeat(payload: &mut [u8], count: usize, random: &mut Random) {
    if payload.len() < 2 {
        return;
    }
    for _ in 0..count {
        let source = random.index(payload.len() - 1);
        payload[source + 1] = payload[source];
    }
}

fn drop_bytes(payload: &mut [u8], count: usize, random: &mut Random) {
    if payload.is_empty() {
        return;
    }
    for _ in 0..count.min(payload.len()) {
        let index = random.index(payload.len());
        payload.copy_within(index + 1.., index);
        *payload.last_mut().expect("payload is not empty") = 0;
    }
}

fn swap(payload: &mut [u8], count: usize, random: &mut Random) {
    if payload.len() < 2 {
        return;
    }
    for _ in 0..count {
        let first = random.index(payload.len());
        let mut second = random.index(payload.len() - 1);
        if second >= first {
            second += 1;
        }
        payload.swap(first, second);
    }
}

struct Random(u64);

impl Random {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, length: usize) -> usize {
        (self.next() % length as u64) as usize
    }

    fn fraction(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1_u64 << 53) as f64);
        ((self.next() >> 11) as f64) * SCALE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_operations_are_length_preserving_and_deterministic() {
        let filters = [
            FilterSpec::ByteNoise { probability: 0.5 },
            FilterSpec::ByteRepeat { count: 3 },
            FilterSpec::ByteDrop { count: 2 },
            FilterSpec::ByteSwap { count: 4 },
        ];
        let original = (0_u8..32).collect::<Vec<_>>();
        let mut first = original.clone();
        let mut second = original.clone();

        apply(&filters, &mut first, 42).unwrap();
        apply(&filters, &mut second, 42).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.len(), original.len());
        assert_ne!(first, original);
    }

    #[test]
    fn zero_probability_noise_is_a_noop() {
        let mut payload = vec![1, 2, 3, 4];
        apply(
            &[FilterSpec::ByteNoise { probability: 0.0 }],
            &mut payload,
            42,
        )
        .unwrap();
        assert_eq!(payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn drop_shifts_and_zero_fills_without_resizing() {
        let mut payload = vec![1, 2, 3, 4];
        drop_bytes(&mut payload, 1, &mut Random::new(42));

        assert_eq!(payload.len(), 4);
        assert_eq!(payload.last(), Some(&0));
    }

    #[test]
    fn rejects_filters_from_other_domains() {
        let error = apply(
            &[FilterSpec::PixelSort { threshold: 128 }],
            &mut [1, 2, 3, 4],
            42,
        )
        .unwrap_err();
        assert!(matches!(error, DatabenderError::OutputValidation { .. }));
    }
}
