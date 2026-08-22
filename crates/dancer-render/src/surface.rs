//! A reusable DIB section that sprite cells are composited into.
//!
//! Allocated once and reused every frame. Phase 0.2 measured the present call at
//! 0.066–0.112 ms for 128–512 px, under 1% of a 60 Hz budget, so the cost here is
//! the compositing memcpy rather than GDI.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::*;

use crate::RenderError;

/// An owned top-down 32-bit DIB plus the memory DC it is selected into.
pub struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    px: *mut u32,
    width: u32,
    height: u32,
}

// The surface is only ever touched from the render thread; the raw pointer is
// into a DIB section this type exclusively owns.
unsafe impl Send for Surface {}

impl Surface {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        let (width, height) = (width.max(1), height.max(1));
        unsafe {
            let screen_dc = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen_dc));
            ReleaseDC(None, screen_dc);
            if dc.is_invalid() {
                return Err(RenderError::Surface);
            }

            let header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    // Negative height = top-down, so row 0 is the top and the
                    // buffer indexes the same way the sprite data does.
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &header, DIB_RGB_COLORS, &mut bits, None, 0)
                    .map_err(|_| RenderError::Surface)?;
            let old = SelectObject(dc, bitmap.into());

            Ok(Self {
                dc,
                bitmap,
                old,
                px: bits as *mut u32,
                width,
                height,
            })
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn dc(&self) -> HDC {
        self.dc
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        // Safety: `px` points at a DIB section of exactly width*height u32s that
        // this Surface owns for its whole lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.px, (self.width * self.height) as usize) }
    }

    /// Clear to fully transparent.
    ///
    /// Zero is correct here and not merely convenient: with premultiplied alpha,
    /// `a = 0` requires `r = g = b = 0`, so an all-zero buffer is the only valid
    /// representation of "nothing".
    pub fn clear(&mut self) {
        self.pixels_mut().fill(0);
    }

    /// Blit a premultiplied cell, source-over, scaled by nearest neighbour.
    ///
    /// Nearest neighbour is deliberate: sprite sheets are pixel art and bilinear
    /// scaling makes them muddy. Spec §13's `scaling_type` can revisit this.
    pub fn blit_scaled(
        &mut self,
        cell: &[u32],
        cell_w: u32,
        cell_h: u32,
        dst_w: u32,
        dst_h: u32,
        mirror: bool,
    ) {
        self.blit_with(cell_w, cell_h, dst_w, dst_h, mirror, |i| cell[i]);
    }

    /// Blit a dissolve between two cells of the same size: `t = 0` is all
    /// `from`, `t = 1` is all `to`.
    ///
    #[allow(clippy::too_many_arguments)]
    /// A per-pixel lerp of premultiplied colour *is* the correct dissolve — the
    /// alpha fades along with the colour, so a limb that exists in one pose and
    /// not the other fades rather than popping. What it is not is motion: both
    /// poses are visible at once in between, which is the trade-off the caller's
    /// `blend` setting opts into.
    pub fn blit_scaled_blend(
        &mut self,
        from: &[u32],
        to: &[u32],
        t: f32,
        cell_w: u32,
        cell_h: u32,
        dst_w: u32,
        dst_h: u32,
        mirror: bool,
    ) {
        if from.len() != to.len() {
            return self.blit_scaled(to, cell_w, cell_h, dst_w, dst_h, mirror);
        }
        let w = (t.clamp(0.0, 1.0) * 256.0).round() as u32;
        match w {
            0 => self.blit_scaled(from, cell_w, cell_h, dst_w, dst_h, mirror),
            256 => self.blit_scaled(to, cell_w, cell_h, dst_w, dst_h, mirror),
            _ => self.blit_with(cell_w, cell_h, dst_w, dst_h, mirror, |i| {
                lerp_premultiplied(from[i], to[i], w)
            }),
        }
    }

    /// Blit a cell at a fraction of its opacity, `0.0..=1.0`.
    #[allow(clippy::too_many_arguments)]
    ///
    /// Premultiplied, so scaling every channel by the weight is the whole job:
    /// colour and coverage fade together, which is what a ghost should do.
    pub fn blit_scaled_faded(
        &mut self,
        cell: &[u32],
        opacity: f32,
        cell_w: u32,
        cell_h: u32,
        dst_w: u32,
        dst_h: u32,
        mirror: bool,
    ) {
        let w = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        match w {
            0 => {}
            256 => self.blit_scaled(cell, cell_w, cell_h, dst_w, dst_h, mirror),
            _ => self.blit_with(cell_w, cell_h, dst_w, dst_h, mirror, |i| {
                lerp_premultiplied(0, cell[i], w)
            }),
        }
    }

    fn blit_with(
        &mut self,
        cell_w: u32,
        cell_h: u32,
        dst_w: u32,
        dst_h: u32,
        mirror: bool,
        sample: impl Fn(usize) -> u32,
    ) {
        if cell_w == 0 || cell_h == 0 || dst_w == 0 || dst_h == 0 {
            return;
        }
        let (sw, sh) = (self.width, self.height);
        let ox = ((sw as i32 - dst_w as i32) / 2).max(0) as u32;
        let oy = ((sh as i32 - dst_h as i32) / 2).max(0) as u32;
        let px = unsafe { std::slice::from_raw_parts_mut(self.px, (sw * sh) as usize) };

        for y in 0..dst_h.min(sh.saturating_sub(oy)) {
            let sy = (y as u64 * cell_h as u64 / dst_h as u64) as u32;
            let src_row = (sy * cell_w) as usize;
            let dst_row = ((y + oy) * sw + ox) as usize;
            for x in 0..dst_w.min(sw.saturating_sub(ox)) {
                let sx = (x as u64 * cell_w as u64 / dst_w as u64) as u32;
                let sx = if mirror { cell_w - 1 - sx } else { sx };
                let s = sample(src_row + sx as usize);
                if s >> 24 == 0 {
                    continue; // fully transparent, nothing to compose
                }
                let d = &mut px[dst_row + x as usize];
                *d = if s >> 24 == 255 {
                    s
                } else {
                    over_premultiplied(s, *d)
                };
            }
        }
    }
}

/// `a + (b − a) · w/256`, per channel, on premultiplied BGRA.
fn lerp_premultiplied(a: u32, b: u32, w: u32) -> u32 {
    let chan = |shift: u32| {
        let x = (a >> shift) & 0xff;
        let y = (b >> shift) & 0xff;
        ((x * (256 - w) + y * w + 128) >> 8).min(255)
    };
    (chan(24) << 24) | (chan(16) << 16) | (chan(8) << 8) | chan(0)
}

/// Porter-Duff source-over for premultiplied BGRA packed in a `u32`.
fn over_premultiplied(src: u32, dst: u32) -> u32 {
    let sa = src >> 24;
    let inv = 255 - sa;
    let chan = |shift: u32| {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        (s + (d * inv + 127) / 255).min(255)
    };
    (chan(24) << 24) | (chan(16) << 16) | (chan(8) << 8) | chan(0)
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Physical size of the window a sheet needs at a given scale.
pub fn surface_size(cell_w: u32, cell_h: u32, scale: f32) -> (u32, u32) {
    let w = (cell_w as f32 * scale).round().max(1.0) as u32;
    let h = (cell_h as f32 * scale).round().max(1.0) as u32;
    (w, h)
}

/// Convenience for callers that only have an `HWND`.
pub fn client_size(hwnd: HWND) -> (u32, u32) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    (
        (r.right - r.left).max(1) as u32,
        (r.bottom - r.top).max(1) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::{lerp_premultiplied, over_premultiplied};

    #[test]
    fn opaque_source_replaces_destination() {
        let src = 0xFF_11_22_33;
        assert_eq!(over_premultiplied(src, 0xFF_AA_BB_CC), src);
    }

    #[test]
    fn transparent_source_leaves_destination() {
        let dst = 0xFF_AA_BB_CC;
        assert_eq!(over_premultiplied(0x00_00_00_00, dst), dst);
    }

    #[test]
    fn half_alpha_over_opaque_stays_opaque() {
        // Premultiplied half-alpha white over opaque black.
        let out = over_premultiplied(0x80_80_80_80, 0xFF_00_00_00);
        assert_eq!(out >> 24, 255, "alpha must saturate, not wrap");
    }

    #[test]
    fn lerp_endpoints_are_exact_and_the_middle_fades_alpha_too() {
        let a = 0xFF_10_20_30;
        let b = 0x00_00_00_00;
        assert_eq!(lerp_premultiplied(a, b, 0), a);
        assert_eq!(lerp_premultiplied(a, b, 256), b);
        let mid = lerp_premultiplied(a, b, 128);
        assert_eq!(mid >> 24, 0x80, "a pose that vanishes should fade, not pop");
        assert_eq!((mid >> 16) & 0xff, 0x08);
    }
}
