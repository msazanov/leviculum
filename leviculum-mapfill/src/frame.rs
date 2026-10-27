//! An Rgb565 framebuffer over borrowed memory, in the panel's byte order.
//!
//! The ST7789 takes RGB565 high byte first, so each pixel is stored as a
//! big-endian `u16`: the buffer goes out over SPI as it is, with no swap
//! pass. Borrowed so the same type sits in internal SRAM or in PSRAM.

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

/// `c` as 8-bit red, green and blue, each channel's top bits repeated
/// into its bottom ones so full intensity stays 255: the one conversion
/// every consumer of a frame outside the panel (a PPM, a terminal picture)
/// goes through.
pub fn rgb888(c: Rgb565) -> [u8; 3] {
    let v = RawU16::from(c).into_inner();
    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
    [
        ((r << 3) | (r >> 2)) as u8,
        ((g << 2) | (g >> 4)) as u8,
        ((b << 3) | (b >> 2)) as u8,
    ]
}

/// A `width` x `height` Rgb565 frame, rows top to bottom.
pub struct Frame<'a> {
    px: &'a mut [u16],
    width: u32,
    height: u32,
}

impl<'a> Frame<'a> {
    /// A frame over `px`, which holds at least `width * height` pixels.
    /// `None` when it is too short.
    pub fn new(px: &'a mut [u16], width: u32, height: u32) -> Option<Self> {
        let n = (width as usize).checked_mul(height as usize)?;
        let px = px.get_mut(..n)?;
        Some(Self { px, width, height })
    }

    /// The pixels as the panel takes them.
    #[allow(unsafe_code)]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: a `u16` slice is a valid byte slice of twice the length,
        // and `u8` has alignment 1.
        unsafe { core::slice::from_raw_parts(self.px.as_ptr().cast::<u8>(), self.px.len() * 2) }
    }

    /// Width and height in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame as packed 8-bit RGB, rows top to bottom, through
    /// [`rgb888`], appended to `out`.
    pub fn rgb888_into(&self, out: &mut impl Extend<u8>) {
        for &px in self.px.iter() {
            let c = Rgb565::from(RawU16::new(u16::from_be(px)));
            out.extend(rgb888(c));
        }
    }

    /// The colour at `(x, y)`, or `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb565> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let v = u16::from_be(self.px[(y * self.width + x) as usize]);
        Some(Rgb565::from(RawU16::new(v)))
    }
}

impl OriginDimensions for Frame<'_> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

impl DrawTarget for Frame<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(
        &mut self,
        it: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in it {
            if p.x >= 0 && p.y >= 0 && (p.x as u32) < self.width && (p.y as u32) < self.height {
                self.px[(p.y as u32 * self.width + p.x as u32) as usize] =
                    RawU16::from(c).into_inner().to_be();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let a = area.intersection(&self.bounding_box());
        if a.size.width == 0 || a.size.height == 0 {
            return Ok(());
        }
        let v = RawU16::from(color).into_inner().to_be();
        let (x0, w) = (a.top_left.x as usize, a.size.width as usize);
        for y in a.top_left.y as u32..a.top_left.y as u32 + a.size.height {
            let row = (y * self.width) as usize + x0;
            self.px[row..row + w].fill(v);
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        self.px.fill(RawU16::from(color).into_inner().to_be());
        Ok(())
    }
}
