#![no_main]

use databender::{codecs::image::fuzz_image_container, MediaFormat};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Some((&selector, encoded)) = input.split_first() else {
        return;
    };
    let formats = [
        MediaFormat::Jpeg,
        MediaFormat::Png,
        MediaFormat::WebP,
        MediaFormat::Avif,
    ];
    fuzz_image_container(formats[usize::from(selector) % formats.len()], encoded);
});