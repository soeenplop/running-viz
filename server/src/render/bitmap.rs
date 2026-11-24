use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};

pub const WIDTH: u32 = 400;
pub const HEIGHT: u32 = 300;
pub const BYTES: usize = (WIDTH * HEIGHT / 8) as usize;

/// 1-bit framebuffer in the e-paper's native format:
/// rows top to bottom, 8 pixels per byte, MSB = leftmost pixel, bit 1 = white.
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap(Vec<u8>);

impl Default for Bitmap {
    fn default() -> Self {
        Self(vec![0xFF; BYTES])
    }
}

impl Bitmap {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// `true` if the pixel is black (ink).
    pub fn is_black(&self, x: u32, y: u32) -> bool {
        let i = (y * WIDTH + x) as usize;
        self.0[i / 8] & (0x80 >> (i % 8)) == 0
    }

    /// Stable 64-bit FNV-1a hash of the pixels, used as HTTP ETag.
    pub fn hash(&self) -> u64 {
        self.0.iter().fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        })
    }

    /// PNG with `scale`× nearest-neighbour upscaling (1 = native size).
    pub fn to_png(&self, scale: u32) -> Vec<u8> {
        let (w, h) = (WIDTH * scale, HEIGHT * scale);
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                data.push(if self.is_black(x / scale, y / scale) {
                    0
                } else {
                    255
                });
            }
        }
        writer.write_image_data(&data).expect("png data");
        writer.finish().expect("png finish");
        out
    }
}

impl OriginDimensions for Bitmap {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Bitmap {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, color) in pixels {
            if p.x < 0 || p.y < 0 || p.x >= WIDTH as i32 || p.y >= HEIGHT as i32 {
                continue;
            }
            let i = (p.y as u32 * WIDTH + p.x as u32) as usize;
            let mask = 0x80 >> (i % 8);
            match color {
                BinaryColor::On => self.0[i / 8] &= !mask,
                BinaryColor::Off => self.0[i / 8] |= mask,
            }
        }
        Ok(())
    }
}
