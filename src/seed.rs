#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SeedIdentity<'a> {
    File(&'a [u8]),
    Stage(u64),
    AudioStream(u64),
    VideoStream(u64),
    Frame(u64),
    Packet(u64),
}

pub(crate) fn derive_seed(parent: u64, identity: SeedIdentity<'_>) -> u64 {
    let (tag, value) = match identity {
        SeedIdentity::File(path) => return derive_bytes(parent, b"file", path),
        SeedIdentity::Stage(index) => (b"stage".as_slice(), index),
        SeedIdentity::AudioStream(index) => (b"audio-stream".as_slice(), index),
        SeedIdentity::VideoStream(index) => (b"video-stream".as_slice(), index),
        SeedIdentity::Frame(index) => (b"frame".as_slice(), index),
        SeedIdentity::Packet(index) => (b"packet".as_slice(), index),
    };
    derive_bytes(parent, tag, &value.to_le_bytes())
}

fn derive_bytes(parent: u64, tag: &[u8], value: &[u8]) -> u64 {
    let mut hash = parent ^ 0xcbf2_9ce4_8422_2325;
    for byte in tag.iter().chain(value) {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3);
    }
    let mut mixed = hash.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_repeatable_and_domain_separated() {
        let identities = [
            SeedIdentity::File(b"input.mp4"),
            SeedIdentity::Stage(0),
            SeedIdentity::AudioStream(0),
            SeedIdentity::VideoStream(0),
            SeedIdentity::Frame(0),
            SeedIdentity::Packet(0),
        ];
        let seeds = identities.map(|identity| derive_seed(42, identity));

        assert_eq!(seeds, identities.map(|identity| derive_seed(42, identity)));
        assert_eq!(
            seeds
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            6
        );
        assert_ne!(
            derive_seed(42, SeedIdentity::Frame(0)),
            derive_seed(42, SeedIdentity::Frame(1))
        );
    }
}
