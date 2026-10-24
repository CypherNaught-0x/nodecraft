use super::*;

/// Original procedural detail: brick-sized colour regions, fine shared luminance texture,
/// and low-frequency lighting. No external photograph or compressible constant-colour raster.
fn detailed_jpeg() -> Vec<u8> {
    let raster = image::RgbImage::from_fn(4800, 3584, |x, y| {
        let mut hash = x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b);
        hash ^= hash >> 16;
        hash = hash.wrapping_mul(0x7feb_352d);
        hash ^= hash >> 15;
        let grain = i32::try_from(hash & 127).unwrap() - 64;
        let mortar = x % 96 < 3 || y % 48 < 3;
        let base = if mortar { [180, 170, 150] } else { [150, 100, 70] };
        let light = i32::try_from((x / 32 + y / 24) % 33).unwrap() - 16;
        image::Rgb(base.map(|channel| u8::try_from((channel + grain + light).clamp(0, 255)).unwrap()))
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&raster).unwrap();
    out
}

#[test]
fn detailed_four_k_jpeg_above_the_old_eight_mib_limit_survives_intact() {
    let bytes = detailed_jpeg();
    assert!(bytes.len() > 8 * 1024 * 1024 && bytes.len() <= 16 * 1024 * 1024, "detailed 4K JPEG is {} bytes", bytes.len());
    let old = Limits { output_bytes: 8 * 1024 * 1024, ..Limits::default() };
    assert!(response::parse(images_reply(&bytes, 1), &old).is_err());
    let done = response::parse(images_reply(&bytes, 1), &Limits::default()).unwrap();
    let [crate::Output::Bytes { bytes: actual, .. }] = done.outputs.as_slice() else { panic!("one complete paid JPEG") };
    assert_eq!(actual.as_ref(), bytes);
    let meta = nodecraft_media::probe_image(actual, &Limits::default().image(false)).unwrap();
    assert_eq!((meta.width, meta.height), (4800, 3584));
}

#[test]
fn response_pixel_headroom_remains_inclusive_and_bounded() {
    // A response can differ modestly from a requested ratio. Request spelling and tuple
    // validation remain exact; response admission measures the actual compressed image.
    let bytes = sized_jpeg(6000, 3000);
    let done = response::parse(images_reply(&bytes, 2), &Limits::default()).unwrap();
    assert_eq!(done.outputs.len(), 2);
    assert!(response::parse(images_reply(&bytes, 2), &Limits { total_pixels: 35_999_999, ..Limits::default() }).is_err());
    let over = sized_jpeg(6000, 3001);
    assert!(response::parse(images_reply(&over, 1), &Limits::default()).is_err());
}
