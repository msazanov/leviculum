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
