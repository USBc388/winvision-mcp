//! Set-of-Mark annotation: numbered color badges + coordinate grid drawn on
//! top of a captured frame (pure CPU drawing, no D2D dependency — a frame is
//! annotated in < 1 ms).

use image::RgbImage;

pub struct Badge {
    pub n: u32,
    /// Image-local top-left coordinates.
    pub x: i32,
    pub y: i32,
}

const BADGE_COLORS: [[u8; 3]; 10] = [
    [230, 57, 70],   // red
    [244, 122, 32],  // orange
    [42, 157, 143],  // teal
    [58, 90, 172],   // blue
    [142, 68, 173],  // purple
    [233, 84, 134],  // pink
    [0, 150, 199],   // sky
    [196, 154, 26],  // gold
    [76, 175, 80],   // green
    [121, 85, 72],   // brown
];

/// 5x7 pixel font, digits 0..9; rows encoded as 5-bit lines (bit4 = leftmost).
const FONT: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111], // 2
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110], // 5
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100], // 9
];

fn put_px(img: &mut RgbImage, x: i32, y: i32, c: [u8; 3]) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, image::Rgb(c));
    }
}

fn draw_digit(img: &mut RgbImage, d: u32, x: i32, y: i32, scale: u32, c: [u8; 3]) {
    if d > 9 {
        return;
    }
    let s = scale.max(1) as i32;
    for (row, bits) in FONT[d as usize].iter().enumerate() {
        for col in 0..5u8 {
            if bits & (1 << (4 - col)) != 0 {
                for dy in 0..s {
                    for dx in 0..s {
                        put_px(img, x + col as i32 * s + dx, y + row as i32 * s + dy, c);
                    }
                }
            }
        }
    }
}

/// Draw one numbered badge (filled rounded-ish box, dark border, white number).
pub fn draw_badge(img: &mut RgbImage, b: &Badge) {
    let scale = if img.width() >= 1600 { 3 } else { 2 };
    let digits = if b.n == 0 { 1 } else { ((b.n as f64).log10().floor() as usize) + 1 };
    let dw = 6 * scale as i32;
    let dh = 7 * scale as i32;
    let pad = 2;
    let bw = digits as i32 * dw - (dw - 5 * scale as i32) + pad * 2;
    let bh = dh + pad * 2;
    let color = BADGE_COLORS[(b.n as usize) % BADGE_COLORS.len()];
    let x0 = b.x.clamp(0, img.width() as i32 - bw.min(img.width() as i32).max(1));
    let y0 = b.y.clamp(0, img.height() as i32 - bh.min(img.height() as i32).max(1));
    // Fill + 1px dark border.
    for yy in y0..(y0 + bh) {
        for xx in x0..(x0 + bw) {
            let border = xx == x0 || xx == x0 + bw - 1 || yy == y0 || yy == y0 + bh - 1;
            put_px(img, xx, yy, if border { [20, 20, 20] } else { color });
        }
    }
    // White digits.
    let num_str_len = digits;
    let total_w = num_str_len as i32 * dw - (dw - 5 * scale as i32);
    let mut dx = x0 + pad + ((bw - pad * 2 - total_w) / 2);
    let mut rest = b.n;
    let mut digit_order = Vec::with_capacity(digits);
    if rest == 0 {
        digit_order.push(0u32);
    } else {
        while rest > 0 {
            digit_order.push(rest % 10);
            rest /= 10;
        }
        digit_order.reverse();
    }
    for d in digit_order {
        draw_digit(img, d, dx, y0 + pad, scale, [255, 255, 255]);
        dx += dw;
    }
}

/// Light coordinate grid every `step` pixels.
pub fn draw_grid(img: &mut RgbImage, step: u32) {
    let step = step.max(20);
    let c: [u8; 3] = [110, 110, 110];
    for x in (0..img.width()).step_by(step as usize) {
        for y in 0..img.height() {
            put_px(img, x as i32, y as i32, c);
        }
    }
    for y in (0..img.height()).step_by(step as usize) {
        for x in 0..img.width() {
            put_px(img, x as i32, y as i32, c);
        }
    }
}

/// Encode an RGB image as JPEG with the given quality.
pub fn encode_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(30, 95));
    img.write_with_encoder(enc)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;
    Ok(out)
}

/// Scale image so width <= max_width (never upscales beyond 1.5x).
pub fn scale_to_width(img: &RgbImage, max_width: u32) -> RgbImage {
    let max_width = max_width.clamp(320, 1920);
    if img.width() <= max_width {
        return img.clone();
    }
    let ratio = max_width as f32 / img.width() as f32;
    let nh = ((img.height() as f32 * ratio) as u32).max(1);
    image::imageops::resize(img, max_width, nh, image::imageops::FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    #[test]
    fn badge_draws_pixels() {
        let mut img = RgbImage::new(200, 100);
        draw_badge(&mut img, &Badge { n: 10, x: 10, y: 10 }); // color index 10 % 10 = 0 (red)
        let mut red_like = 0;
        for p in img.pixels() {
            if p.0 == BADGE_COLORS[0] {
                red_like += 1;
            }
        }
        assert!(red_like > 50, "expected filled badge, got {red_like} px");
    }

    #[test]
    fn badge_number_42_renders_two_digits() {
        let mut img = RgbImage::new(300, 200);
        draw_badge(&mut img, &Badge { n: 42, x: 5, y: 5 });
        // White pixels of the digits must exist inside the badge area.
        let white = img
            .view(0, 0, 60, 30)
            .pixels()
            .filter(|(_, _, px)| px.0 == [255, 255, 255])
            .count();
        assert!(white > 10, "digits not drawn ({white} white px)");
    }

    #[test]
    fn badge_clamps_inside_image() {
        let mut img = RgbImage::new(40, 20);
        draw_badge(&mut img, &Badge { n: 123456, x: 35, y: 18 });
        // Must not panic; badge partially visible.
        let colored = img.pixels().filter(|p| p.0 != [0, 0, 0]).count();
        assert!(colored > 0);
    }

    #[test]
    fn grid_draws_lines() {
        let mut img = RgbImage::new(100, 100);
        draw_grid(&mut img, 20);
        let gray = img.pixels().filter(|p| p.0 == [110, 110, 110]).count();
        // 5 vertical + 5 horizontal lines of 100 px each.
        assert!(gray >= 900, "grid missing ({gray} px)");
    }

    #[test]
    fn jpeg_encode_roundtrip() {
        let mut img = RgbImage::new(64, 64);
        draw_badge(&mut img, &Badge { n: 1, x: 2, y: 2 });
        let jpg = encode_jpeg(&img, 80).expect("encode");
        assert!(jpg.len() > 100);
        assert_eq!(&jpg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    }

    #[test]
    fn scale_only_downscales() {
        let img = RgbImage::new(2000, 1000);
        let out = scale_to_width(&img, 1280);
        assert_eq!(out.width(), 1280);
        assert_eq!(out.height(), 640);
        let small = RgbImage::new(600, 300);
        let out2 = scale_to_width(&small, 1280);
        assert_eq!(out2.width(), 600);
    }
}
