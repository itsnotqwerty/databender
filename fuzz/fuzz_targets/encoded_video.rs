#![no_main]

use databender::codecs::encoded_video::mutate_packet;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Some((&controls, packet)) = input.split_first() else {
        return;
    };
    let codecs = ["h264", "hevc", "vp8", "vp9", "av1"];
    let codec = codecs[usize::from(controls) % codecs.len()];
    let length_size = usize::from((controls >> 3) & 0x07);
    let budget = usize::from(controls >> 1);
    let intensity = f64::from(controls) / 255.0;
    let mut candidate = packet.to_vec();
    let original_len = candidate.len();

    if let Ok(impact) = mutate_packet(
        codec,
        &mut candidate,
        Some(length_size),
        budget,
        intensity,
        u64::from(controls),
    ) {
        assert_eq!(candidate.len(), original_len);
        assert!(impact.mutated_bytes <= budget);
        assert!(impact.mutated_bytes <= impact.eligible_bytes);
    }
});
