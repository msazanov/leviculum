//! The framebuffer stores pixels in the ST7789's byte order and clips.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use leviculum_mapfill::frame::Frame;

#[test]
fn pixels_go_out_high_byte_first() {
    let mut px = vec![0u16; 4 * 3];
    let mut f = Frame::new(&mut px, 4, 3).expect("frame");
    let c = Rgb565::new(0b10101, 0b110011, 0b01110);
    f.fill_solid(&Rectangle::new(Point::new(-2, 1), Size::new(4, 9)), c)
        .expect("fill");
    Pixel(Point::new(3, 0), Rgb565::WHITE)
        .draw(&mut f)
        .expect("pixel");
    Pixel(Point::new(4, 0), Rgb565::WHITE)
        .draw(&mut f)
        .expect("outside, ignored");
    let raw = (0b10101u16 << 11) | (0b110011 << 5) | 0b01110;
    let b = f.bytes();
    assert_eq!(b.len(), 24);
    assert_eq!(&b[8..10], &raw.to_be_bytes());
    assert_eq!(&b[6..8], &[0xFF, 0xFF]);
    assert_eq!(f.pixel(1, 2), Some(c));
    assert_eq!(f.pixel(2, 2), Some(Rgb565::BLACK));
    assert_eq!(f.pixel(4, 0), None);
    assert!(Frame::new(&mut [0u16; 11], 4, 3).is_none());
}

/// The one RGB888 conversion: full channels stay 255, the colour tables'
/// background comes back as the bytes it was written from, and a frame
/// converts pixel by pixel in row order.
#[test]
fn rgb888_keeps_full_intensity_and_row_order() {
    use leviculum_mapfill::frame::rgb888;
    use leviculum_mapfill::style::BACKGROUND;
    assert_eq!(rgb888(Rgb565::WHITE), [255, 255, 255]);
    assert_eq!(rgb888(Rgb565::BLACK), [0, 0, 0]);
    // 242, 239, 233 lose their low bits in Rgb565 and get them back from
    // the top ones.
    assert_eq!(rgb888(BACKGROUND), [247, 239, 239]);
    let mut px = [0u16; 2];
    let mut frame = Frame::new(&mut px, 2, 1).expect("frame");
    let Ok(()) = Pixel(Point::new(1, 0), Rgb565::RED).draw(&mut frame);
    let mut out = Vec::new();
    frame.rgb888_into(&mut out);
    assert_eq!(out, [0, 0, 0, 255, 0, 0]);
}

/// The width table the map core picks per zoom starts from the one
/// `line_style` carries.
#[test]
fn the_default_widths_are_line_styles() {
    use leviculum_mapfill::style::{line_style, line_width, Class, LINE_WIDTHS};
    for c in Class::ALL {
        let want = if c.is_area() { 0 } else { line_style(c).1 };
        assert_eq!(line_width(&LINE_WIDTHS, c), want, "{c:?}");
    }
}
