use crate::{DatabenderError, FilterSpec, Result};

pub fn apply(
    filters: &[FilterSpec],
    pixels: &mut [u8],
    width: u32,
    height: u32,
    seed: u64,
) -> Result<()> {
    validate_buffer(pixels, width, height)?;

    for (index, filter) in filters.iter().enumerate() {
        match filter {
            FilterSpec::ChannelShift { pixels: offset } => {
                channel_shift(pixels, width, height, *offset)
            }
            FilterSpec::ScanlineDisplacement { max_shift } => scanline_displacement(
                pixels,
                width,
                height,
                *max_shift,
                derive_filter_seed(seed, index as u64),
            ),
            FilterSpec::PixelSort { threshold } => pixel_sort(pixels, width, height, *threshold),
            FilterSpec::Brightness { delta } => brightness(pixels, *delta),
            FilterSpec::Contrast { factor } => contrast(pixels, *factor),
            FilterSpec::Saturation { factor } => saturation(pixels, *factor),
            FilterSpec::HueRotate { degrees } => hue_rotate(pixels, *degrees),
            FilterSpec::Posterize { bits } => posterize(pixels, *bits),
            FilterSpec::Invert => invert(pixels),
            FilterSpec::RowDropout { probability } => row_dropout(
                pixels,
                width,
                height,
                *probability,
                derive_filter_seed(seed, index as u64),
            ),
            _ => {
                return Err(DatabenderError::OutputValidation {
                    reason: format!("filter {} is not an image-pixel filter", filter.name()),
                })
            }
        }
    }

    Ok(())
}

fn channel_shift(pixels: &mut [u8], width: u32, height: u32, offset: i32) {
    if width == 0 || offset == 0 {
        return;
    }

    let source = pixels.to_vec();
    let width = width as usize;
    let red_offset = offset.rem_euclid(width as i32) as usize;
    let blue_offset = (-offset).rem_euclid(width as i32) as usize;

    for row in 0..height as usize {
        for column in 0..width {
            let destination = (row * width + column) * 4;
            let red_source = (row * width + (column + width - red_offset) % width) * 4;
            let blue_source = (row * width + (column + width - blue_offset) % width) * 4;
            pixels[destination] = source[red_source];
            pixels[destination + 2] = source[blue_source + 2];
        }
    }
}

fn scanline_displacement(pixels: &mut [u8], width: u32, height: u32, max_shift: u32, seed: u64) {
    if width == 0 {
        return;
    }

    let source = pixels.to_vec();
    let width = width as usize;
    let effective_maximum = max_shift.min(width as u32) as i64;
    let span = effective_maximum * 2 + 1;

    for row in 0..height as usize {
        let random = splitmix64(seed.wrapping_add(row as u64));
        let shift = (random % span as u64) as i64 - effective_maximum;
        let normalized = shift.rem_euclid(width as i64) as usize;

        for column in 0..width {
            let destination = (row * width + column) * 4;
            let source_column = (column + width - normalized) % width;
            let source_index = (row * width + source_column) * 4;
            pixels[destination..destination + 4]
                .copy_from_slice(&source[source_index..source_index + 4]);
        }
    }
}

fn pixel_sort(pixels: &mut [u8], width: u32, height: u32, threshold: u8) {
    let row_length = width as usize * 4;
    for row in pixels.chunks_exact_mut(row_length).take(height as usize) {
        let mut start = 0;
        while start < width as usize {
            while start < width as usize && luminance(&row[start * 4..start * 4 + 4]) < threshold {
                start += 1;
            }
            let mut end = start;
            while end < width as usize && luminance(&row[end * 4..end * 4 + 4]) >= threshold {
                end += 1;
            }
            let mut sorted = row[start * 4..end * 4]
                .chunks_exact(4)
                .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                .collect::<Vec<_>>();
            sorted.sort_by_key(|pixel| luminance(pixel));
            for (destination, pixel) in row[start * 4..end * 4].chunks_exact_mut(4).zip(sorted) {
                destination.copy_from_slice(&pixel);
            }
            start = end;
        }
    }
}

fn brightness(pixels: &mut [u8], delta: i16) {
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = (i16::from(*channel) + delta).clamp(0, 255) as u8;
        }
    }
}

fn contrast(pixels: &mut [u8], factor: f64) {
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = ((f64::from(*channel) - 128.0) * factor + 128.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

fn saturation(pixels: &mut [u8], factor: f64) {
    for pixel in pixels.chunks_exact_mut(4) {
        let gray = f64::from(luminance(pixel));
        for channel in &mut pixel[..3] {
            *channel = (gray + (f64::from(*channel) - gray) * factor)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

fn hue_rotate(pixels: &mut [u8], degrees: f64) {
    for pixel in pixels.chunks_exact_mut(4) {
        let (hue, saturation, value) = rgb_to_hsv(pixel[0], pixel[1], pixel[2]);
        let (red, green, blue) = hsv_to_rgb((hue + degrees).rem_euclid(360.0), saturation, value);
        pixel[0] = red;
        pixel[1] = green;
        pixel[2] = blue;
    }
}

fn posterize(pixels: &mut [u8], bits: u8) {
    let levels = (1_u16 << bits) - 1;
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            let quantized = (u16::from(*channel) * levels + 127) / 255;
            *channel = ((quantized * 255 + levels / 2) / levels) as u8;
        }
    }
}

fn invert(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = 255 - *channel;
        }
    }
}

fn row_dropout(pixels: &mut [u8], width: u32, height: u32, probability: f64, seed: u64) {
    let row_length = width as usize * 4;
    for (row_index, row) in pixels
        .chunks_exact_mut(row_length)
        .take(height as usize)
        .enumerate()
    {
        let random = splitmix64(seed.wrapping_add(row_index as u64));
        let fraction = ((random >> 11) as f64) / ((1_u64 << 53) as f64);
        if fraction < probability {
            for pixel in row.chunks_exact_mut(4) {
                pixel[..3].fill(0);
            }
        }
    }
}

fn rgb_to_hsv(red: u8, green: u8, blue: u8) -> (f64, f64, f64) {
    let red = f64::from(red) / 255.0;
    let green = f64::from(green) / 255.0;
    let blue = f64::from(blue) / 255.0;
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let delta = maximum - minimum;
    let hue = if delta == 0.0 {
        0.0
    } else if maximum == red {
        60.0 * ((green - blue) / delta).rem_euclid(6.0)
    } else if maximum == green {
        60.0 * ((blue - red) / delta + 2.0)
    } else {
        60.0 * ((red - green) / delta + 4.0)
    };
    let saturation = if maximum == 0.0 { 0.0 } else { delta / maximum };
    (hue, saturation, maximum)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> (u8, u8, u8) {
    let chroma = value * saturation;
    let sector = hue / 60.0;
    let intermediate = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match sector.floor() as u8 {
        0 => (chroma, intermediate, 0.0),
        1 => (intermediate, chroma, 0.0),
        2 => (0.0, chroma, intermediate),
        3 => (0.0, intermediate, chroma),
        4 => (intermediate, 0.0, chroma),
        _ => (chroma, 0.0, intermediate),
    };
    let offset = value - chroma;
    (
        ((red + offset) * 255.0).round() as u8,
        ((green + offset) * 255.0).round() as u8,
        ((blue + offset) * 255.0).round() as u8,
    )
}

fn luminance(pixel: &[u8]) -> u8 {
    let weighted =
        u32::from(pixel[0]) * 2126 + u32::from(pixel[1]) * 7152 + u32::from(pixel[2]) * 722;
    (weighted / 10_000) as u8
}

fn validate_buffer(pixels: &[u8], width: u32, height: u32) -> Result<()> {
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "RGBA8 buffer has {} bytes; expected {expected} for {width}x{height}",
                pixels.len()
            ),
        });
    }
    Ok(())
}

fn derive_filter_seed(seed: u64, index: u64) -> u64 {
    splitmix64(seed.wrapping_add(index))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_shift_offsets_red_and_blue_without_changing_alpha() {
        let mut pixels = vec![
            10, 20, 30, 40, // first pixel
            50, 60, 70, 80, // second pixel
            90, 100, 110, 120, // third pixel
        ];

        channel_shift(&mut pixels, 3, 1, 1);

        assert_eq!(
            pixels,
            vec![90, 20, 70, 40, 10, 60, 110, 80, 50, 100, 30, 120]
        );
    }

    #[test]
    fn scanline_displacement_is_seeded_and_preserves_pixels() {
        let original = (0_u8..32).collect::<Vec<_>>();
        let mut first = original.clone();
        let mut second = original.clone();

        scanline_displacement(&mut first, 4, 2, 3, 42);
        scanline_displacement(&mut second, 4, 2, 3, 42);

        assert_eq!(first, second);
        for row in 0..2 {
            let mut original_pixels = original[row * 16..row * 16 + 16]
                .chunks_exact(4)
                .collect::<Vec<_>>();
            let mut shifted_pixels = first[row * 16..row * 16 + 16]
                .chunks_exact(4)
                .collect::<Vec<_>>();
            original_pixels.sort();
            shifted_pixels.sort();
            assert_eq!(original_pixels, shifted_pixels);
        }
    }

    #[test]
    fn pixel_sort_only_sorts_threshold_qualified_runs() {
        let mut pixels = vec![
            200, 200, 200, 1, // qualified
            100, 100, 100, 2, // qualified
            10, 10, 10, 3, // separator
            250, 250, 250, 4, // qualified
            150, 150, 150, 5, // qualified
        ];

        pixel_sort(&mut pixels, 5, 1, 50);

        assert_eq!(pixels[0], 100);
        assert_eq!(pixels[4], 200);
        assert_eq!(pixels[8], 10);
        assert_eq!(pixels[12], 150);
        assert_eq!(pixels[16], 250);
        assert_eq!(
            [pixels[3], pixels[7], pixels[11], pixels[15], pixels[19]],
            [2, 1, 3, 5, 4]
        );
    }

    #[test]
    fn apply_rejects_non_pixel_filters() {
        let error = apply(
            &[FilterSpec::ByteNoise { probability: 0.1 }],
            &mut [0; 4],
            1,
            1,
            42,
        )
        .unwrap_err();

        assert!(matches!(error, DatabenderError::OutputValidation { .. }));
    }

    #[test]
    fn color_adjustments_preserve_alpha_and_clamp_channels() {
        let original_alpha = 77;
        let mut pixels = [250, 10, 80, original_alpha];

        brightness(&mut pixels, 20);
        assert_eq!(pixels, [255, 30, 100, original_alpha]);
        contrast(&mut pixels, 0.0);
        assert_eq!(pixels, [128, 128, 128, original_alpha]);
        invert(&mut pixels);
        assert_eq!(pixels, [127, 127, 127, original_alpha]);
    }

    #[test]
    fn hue_rotation_and_desaturation_have_expected_colors() {
        let mut red = [255, 0, 0, 19];
        hue_rotate(&mut red, 120.0);
        assert_eq!(red, [0, 255, 0, 19]);

        saturation(&mut red, 0.0);
        assert_eq!(red[0], red[1]);
        assert_eq!(red[1], red[2]);
        assert_eq!(red[3], 19);
    }

    #[test]
    fn posterize_reduces_levels_without_touching_alpha() {
        let mut pixels = [20, 100, 240, 33];
        posterize(&mut pixels, 2);
        assert_eq!(pixels, [0, 85, 255, 33]);
    }

    #[test]
    fn row_dropout_is_seeded_and_preserves_alpha() {
        let original = [20, 40, 60, 11].repeat(8);
        let mut first = original.clone();
        let mut second = original;

        row_dropout(&mut first, 2, 4, 1.0, 42);
        row_dropout(&mut second, 2, 4, 1.0, 42);

        assert_eq!(first, second);
        assert!(first.chunks_exact(4).all(|pixel| pixel[3] == 11));
        assert!(first.chunks_exact(4).any(|pixel| pixel[..3] == [0, 0, 0]));
    }
}
