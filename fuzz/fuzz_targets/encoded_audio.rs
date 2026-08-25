#![no_main]

use databender::{
    codecs::{mp3_frames, ogg_pages},
    filters::mp3,
    FilterSpec,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Some((&controls, encoded)) = input.split_first() else {
        return;
    };

    if mp3_frames::parse(encoded).is_ok() {
        let mut candidate = encoded.to_vec();
        let filter = FilterSpec::parse(&format!(
            "mp3-main-data-noise:byte_budget={},intensity={}",
            usize::from(controls & 0x1f),
            f64::from(controls) / 255.0
        ))
        .expect("bounded generated MP3 filter");
        let original_len = candidate.len();
        if mp3::apply(&filter, &mut candidate, u64::from(controls)).is_ok() {
            assert_eq!(candidate.len(), original_len);
            assert!(mp3_frames::parse(&candidate).is_ok());
        }
    }

    if ogg_pages::parse(encoded).is_ok() {
        let mut candidate = encoded.to_vec();
        let original_len = candidate.len();
        if ogg_pages::mutate(
            &mut candidate,
            usize::from(controls & 0x1f),
            0,
            0,
            f64::from(controls) / 255.0,
            u64::from(controls),
        )
        .is_ok()
        {
            assert_eq!(candidate.len(), original_len);
            assert!(ogg_pages::parse(&candidate).is_ok());
        }
    }
});
