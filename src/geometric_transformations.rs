//! Geometric transformations of images. This includes rotations, translation, and general
//! projective transformations.

use std::ops::Mul;
use conv::ValueInto;
use image::{GenericImage, GenericImageView, ImageBuffer, Pixel};
use rayon::prelude::*;
use crate::definitions::{Clamp, Image};
use crate::math::cast;

#[derive(Copy, Clone, Debug)]
enum TransformationClass {
    Translation,
    Affine,
    Projection,
}

/// A 2d projective transformation, stored as a row major 3x3 matrix.
///
/// Transformations combine by pre-multiplication, i.e. applying `P * Q` is equivalent to
/// applying `Q` and then applying `P`. For example, the following defines a rotation
/// about the point (320.0, 240.0).
///
/// ```
/// use imageproc::geometric_transformations::*;
/// use std::f32::consts::PI;
///
/// let (cx, cy) = (320.0, 240.0);
///
/// let c_rotation = Projection::translate(cx, cy)
///     * Projection::rotate(PI / 6.0)
///     * Projection::translate(-cx, -cy);
/// ```
/// 
/// See ./examples/projection.rs for more examples.
#[derive(Copy, Clone, Debug)]
pub struct Projection {
    transform: [f32; 9],
    inverse: [f32; 9],
    class: TransformationClass,
}

impl Projection {
    /// Creates a 2d projective transform from a row-major 3x3 matrix in homogeneous coordinates.
    ///
    /// Returns `None` if the matrix is not invertible.
    pub fn from_matrix(transform: [f32; 9]) -> Option<Projection> {
        let transform = normalize(transform);
        let class = class_from_matrix(transform);
        try_inverse(&transform).map(|inverse| Projection {
            transform,
            inverse,
            class,
        })
    }

    /// A translation by (tx, ty).
    pub fn translate(tx: f32, ty: f32) -> Projection {
        Projection {
            transform: [
                1.0, 0.0, tx,
                0.0, 1.0, ty,
                0.0, 0.0, 1.0
            ],
            inverse: [
                1.0, 0.0, -tx,
                0.0, 1.0, -ty,
                0.0, 0.0, 1.0
            ],
            class: TransformationClass::Translation,
        }
    }

    /// A clockwise rotation around the top-left corner of the image by theta radians.
    pub fn rotate(theta: f32) -> Projection {
        let s = theta.sin();
        let c = theta.cos();
        Projection {
            transform: [
                  c,  -s, 0.0,
                  s,   c, 0.0,
                0.0, 0.0, 1.0
            ],
            inverse: [
                  c,   s, 0.0,
                 -s,   c, 0.0,
                0.0, 0.0, 1.0
            ],
            class: TransformationClass::Affine,
        }
    }

    /// An anisotropic scaling (sx, sy).
    /// 
    /// Note that the `warp` function does not change the size of the input image.
    /// If you want to resize an image then use the `imageops` module in the `image` crate.
    pub fn scale(sx: f32, sy: f32) -> Projection {
        Projection {
            transform: [
                 sx, 0.0, 0.0,
                0.0,  sy, 0.0,
                0.0, 0.0, 1.0
            ],
            inverse: [
                1.0 / sx, 0.0,      0.0,
                0.0,      1.0 / sy, 0.0,
                0.0,      0.0,      1.0
            ],
            class: TransformationClass::Affine,
        }
    }

    /// Inverts the transformation.
    pub fn invert(self) -> Projection {
        Projection {
            transform: self.inverse,
            inverse: self.transform,
            class: self.class,
        }
    }

    /// Calculates a projection from a set of four control point pairs.
    pub fn from_control_points(from: [(f32, f32); 4], to: [(f32, f32); 4]) -> Option<Projection> {
        use rulinalg::matrix::*;

        let (xf1, yf1, xf2, yf2, xf3, yf3, xf4, yf4) = (
            from[0].0 as f64,
            from[0].1 as f64,
            from[1].0 as f64,
            from[1].1 as f64,
            from[2].0 as f64,
            from[2].1 as f64,
            from[3].0 as f64,
            from[3].1 as f64,
        );

        let (x1, y1, x2, y2, x3, y3, x4, y4) = (
            to[0].0 as f64,
            to[0].1 as f64,
            to[1].0 as f64,
            to[1].1 as f64,
            to[2].0 as f64,
            to[2].1 as f64,
            to[3].0 as f64,
            to[3].1 as f64,
        );

        let a = Matrix::new(
            9,
            9,
            vec![
                0.0, 0.0, 0.0, -xf1, -yf1, -1.0, y1 * xf1, y1 * yf1, y1,
                xf1, yf1, 1.0, 0.0, 0.0, 0.0, -x1 * xf1, -x1 * yf1, -x1,
                0.0, 0.0, 0.0, -xf2, -yf2, -1.0, y2 * xf2, y2 * yf2, y2,
                xf2, yf2, 1.0, 0.0, 0.0, 0.0, -x2 * xf2, -x2 * yf2, -x2,
                0.0, 0.0, 0.0, -xf3, -yf3, -1.0, y3 * xf3, y3 * yf3, y3,
                xf3, yf3, 1.0, 0.0, 0.0, 0.0, -x3 * xf3, -x3 * yf3, -x3,
                0.0, 0.0, 0.0, -xf4, -yf4, -1.0, y4 * xf4, y4 * yf4, y4,
                xf4, yf4, 1.0, 0.0, 0.0, 0.0, -x4 * xf4, -x4 * yf4, -x4,
                xf4, yf4, 1.0, 0.0, 0.0, 0.0, -x4 * xf4, -x4 * yf4, -x4,
            ],
        );

        match a.svd() {
            Err(_) => None,
            Ok((s, _, v)) => {
                // rank(a) must be 8, but not 7
                if s[[8,8]].abs() > 0.01 || s[[7,7]].abs() < 0.01 {
                    None
                } else {
                    let h = v.col(8).into_matrix().into_vec();
                    let mut transform = [
                        h[0] as f32,
                        h[1] as f32,
                        h[2] as f32,
                        h[3] as f32,
                        h[4] as f32,
                        h[5] as f32,
                        h[6] as f32,
                        h[7] as f32,
                        h[8] as f32,
                    ];
                    transform = normalize(transform);
                    let class = class_from_matrix(transform);

                    try_inverse(&transform).map(|inverse| Projection {
                        transform,
                        inverse,
                        class,
                    })
                }
            }
        }
    }

    // Helper functions used as optimization in warp.
    #[inline(always)]
    fn map_projective(&self, x: f32, y: f32) -> (f32, f32) {
        let t = &self.transform;
        let d = t[6] * x + t[7] * y + t[8];
        (
            (t[0] * x + t[1] * y + t[2]) / d,
            (t[3] * x + t[4] * y + t[5]) / d,
        )
    }

    #[inline(always)]
    fn map_affine(&self, x: f32, y: f32) -> (f32, f32) {
        let t = &self.transform;
        ((t[0] * x + t[1] * y + t[2]), (t[3] * x + t[4] * y + t[5]))
    }

    #[inline(always)]
    fn map_translation(&self, x: f32, y: f32) -> (f32, f32) {
        let t = &self.transform;
        let tx = t[2];
        let ty = t[5];
        (x + tx, y + ty)
    }
}

impl<'a, 'b> Mul<&'b Projection> for &'a Projection {
    type Output = Projection;

    fn mul(self, rhs: &Projection) -> Projection {
        use TransformationClass as TC;
        let t = mul3x3(self.transform, rhs.transform);
        let i = mul3x3(rhs.inverse, self.inverse);

        let class = match (self.class, rhs.class) {
            (TC::Translation, TC::Translation) => TC::Translation,
            (TC::Translation, TC::Affine) => TC::Affine,
            (TC::Affine, TC::Translation) => TC::Affine,
            (TC::Affine, TC::Affine) => TC::Affine,
            (_, _) => TC::Projection,
        };

        Projection {
            transform: t,
            inverse: i,
            class: class,
        }
    }
}

impl Mul<Projection> for Projection {
    type Output = Projection;

    fn mul(self, rhs: Projection) -> Projection {
        &self * &rhs
    }
}

impl<'a, 'b> Mul<&'b (f32, f32)> for &'a Projection {
    type Output = (f32, f32);

    fn mul(self, rhs: &(f32, f32)) -> (f32, f32) {
        let (x, y) = *rhs;
        self.map_projective(x, y)
    }
}

impl Mul<(f32, f32)> for Projection {
    type Output = (f32, f32);

    fn mul(self, rhs: (f32, f32)) -> (f32, f32) {
        &self * &rhs
    }
}

/// Rotates an image clockwise about its center.
/// The output image has the same dimensions as the input. Output pixels
/// whose pre-image lies outside the input image are set to `default`.
pub fn rotate_about_center<P>(
    image: &Image<P>,
    theta: f32,
    interpolation: Interpolation,
    default: P,
) -> Image<P>
where
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let (w, h) = image.dimensions();
    rotate(
        image,
        (w as f32 / 2.0, h as f32 / 2.0),
        theta,
        interpolation,
        default,
    )
}

/// Rotates an image clockwise about the provided center by theta radians.
/// The output image has the same dimensions as the input. Output pixels
/// whose pre-image lies outside the input image are set to `default`.
pub fn rotate<P>(
    image: &Image<P>,
    center: (f32, f32),
    theta: f32,
    interpolation: Interpolation,
    default: P,
) -> Image<P>
where
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let (cx, cy) = center;
    let projection = Projection::translate(cx, cy) * Projection::rotate(theta) * Projection::translate(-cx, -cy);
    warp(image, &projection, interpolation, default)
}

/// Translates the input image by t. Note that image coordinates increase from
/// top left to bottom right. Output pixels whose pre-image are not in the input
/// image are set to the boundary pixel in the input image nearest to their pre-image.
// TODO: it's confusing that this has different behaviour to
// TODO: attempting the equivalent transformation using Projection.
pub fn translate<P>(image: &Image<P>, t: (i32, i32)) -> Image<P>
where
    P: Pixel + 'static,
{
    use std::cmp;

    let (width, height) = image.dimensions();
    let mut out = ImageBuffer::new(width, height);

    let w = width as i32;
    let h = height as i32;

    for y in 0..height {
        for x in 0..width {
            let x_in = cmp::max(0, cmp::min(x as i32 - t.0, w - 1));
            let y_in = cmp::max(0, cmp::min(y as i32 - t.1, h - 1));
            // (x_in, y_in) and (x, y) are guaranteed to be in bounds
            unsafe {
                let p = image.unsafe_get_pixel(x_in as u32, y_in as u32);
                out.unsafe_put_pixel(x, y, p);
            }
        }
    }

    out
}

/// Applies a projective transformation to an image.
/// 
/// The returned image has the same dimensions as `image`. Output pixels
/// whose pre-image lies outside the input image are set to `default`.
/// 
/// The provided projection defines a mapping from locations in the input image to their
/// corresponding location in the output image.
pub fn warp<P>(
    image: &Image<P>,
    projection: &Projection,
    interpolation: Interpolation,
    default: P,
) -> Image<P>
where
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let (width, height) = image.dimensions();
    let mut out = ImageBuffer::new(width, height);
    warp_into(image, projection, interpolation, default, &mut out);
    out
}

/// Applies a projective transformation to an image, writing to a provided output.
///
/// See the [`warp`](fn.warp.html) documentation for more information.
pub fn warp_into<P>(
    image: &Image<P>,
    projection: &Projection,
    interpolation: Interpolation,
    default: P,
    out: &mut Image<P>,
) where
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32> + Sync,
{
    let projection = projection.invert();
    let nn = |x, y| nearest(image, x, y, default);
    let bl = |x, y| interpolate(image, x, y, default);
    let wt = |x, y| projection.map_projective(x, y);
    let wa = |x, y| projection.map_affine(x, y);
    let wp = |x, y| projection.map_translation(x, y);
    use Interpolation as I;
    use TransformationClass as TC;

    match (interpolation, projection.class) {
        (I::Nearest, TC::Translation) => warp_inner(out, wt, nn),
        (I::Nearest, TC::Affine) => warp_inner(out, wa, nn),
        (I::Nearest, TC::Projection) => warp_inner(out, wp, nn),
        (I::Bilinear, TC::Translation) => warp_inner(out, wt, bl),
        (I::Bilinear, TC::Affine) => warp_inner(out, wa, bl),
        (I::Bilinear, TC::Projection) => warp_inner(out, wp, bl),
    }
}

/// Warps an image using the provided function to define the pre-image of each output pixel.
/// 
/// # Examples
/// Applying a wave pattern.
/// ```
/// use image::{ImageBuffer, Luma};
/// use imageproc::utils::gray_bench_image;
/// use imageproc::geometric_transformations::*;
///
/// let image = gray_bench_image(300, 300);
/// let warped = warp_with(
///     &image,
///     |x, y| (x, y + (x / 30.0).sin()),
///     Interpolation::Nearest,
///     Luma([0u8])
/// );
/// ```
pub fn warp_with<P, F>(
    image: &Image<P>,
    mapping: F,
    interpolation: Interpolation,
    default: P,
) -> Image<P>
where
    F: Fn(f32, f32) -> (f32, f32) + Sync + Send,
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let (width, height) = image.dimensions();
    let mut out = ImageBuffer::new(width, height);
    warp_into_with(image, mapping, interpolation, default, &mut out);
    out
}

/// Warps an image using the provided function to define the pre-image of each output pixel,
/// writing into a preallocated output.
/// 
/// See the [`warp_with`](fn.warp_with.html) documentation for more information.
pub fn warp_into_with<P, F>(
    image: &Image<P>,
    mapping: F,
    interpolation: Interpolation,
    default: P,
    out: &mut Image<P>,
) where
    F: Fn(f32, f32) -> (f32, f32) + Send + Sync,
    P: Pixel + Send + Sync + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let nn = |x, y| nearest(image, x, y, default);
    let bl = |x, y| interpolate(image, x, y, default);
    use Interpolation as I;

    match interpolation {
        I::Nearest => warp_inner(out, mapping, nn),
        I::Bilinear => warp_inner(out, mapping, bl),
    }
}

// Work horse of all warp functions
// TODO: make faster by avoiding boundary checks in inner section of src image
fn warp_inner<P, Fc, Fi>(out: &mut Image<P>, mapping: Fc, get_pixel: Fi)
where
    P: Pixel + 'static,
    <P as Pixel>::Subpixel: Send + Sync,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
    Fc: Fn(f32, f32) -> (f32, f32) + Send + Sync,
    Fi: Fn(f32, f32) -> P + Send + Sync,
{
    let width = out.width();
    let raw_out = out.as_mut();
    let pitch = P::CHANNEL_COUNT as usize * width as usize;

    raw_out
        .par_chunks_mut(pitch)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, slice) in row.chunks_mut(P::CHANNEL_COUNT as usize).enumerate() {
                let (px, py) = mapping(x as f32, y as f32);
                *P::from_slice_mut(slice) = get_pixel(px, py);
            }
        });
}

// Classifies transformation by looking up transformation matrix coefficients
fn class_from_matrix(mx: [f32; 9]) -> TransformationClass {
    if (mx[6] - 0.0).abs() < 1e-10 && (mx[7] - 0.0).abs() < 1e-10 && (mx[8] - 1.0).abs() < 1e-10 {
        if (mx[0] - 1.0).abs() < 1e-10
            && (mx[1] - 0.0).abs() < 1e-10
            && (mx[3] - 0.0).abs() < 1e-10
            && (mx[4] - 1.0).abs() < 1e-10
        {
            TransformationClass::Translation
        } else {
            TransformationClass::Affine
        }
    } else {
        TransformationClass::Projection
    }
}

fn normalize(mx: [f32; 9]) -> [f32; 9] {
    [
        mx[0] / mx[8],
        mx[1] / mx[8],
        mx[2] / mx[8],
        mx[3] / mx[8],
        mx[4] / mx[8],
        mx[5] / mx[8],
        mx[6] / mx[8],
        mx[7] / mx[8],
        mx[8] / mx[8],
    ]
}

// TODO: write me in f64
fn try_inverse(t: &[f32; 9]) -> Option<[f32; 9]> {
    let (t00, t01, t02, t10, t11, t12, t20, t21, t22) =
        (t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[8]);

    let m00 = t11 * t22 - t12 * t21;
    let m01 = t10 * t22 - t12 * t20;
    let m02 = t10 * t21 - t11 * t20;

    let det = t00 * m00 - t01 * m01 + t02 * m02;

    if det.abs() < 1e-10 {
        return None;
    }

    let m10 = t01 * t22 - t02 * t21;
    let m11 = t00 * t22 - t02 * t20;
    let m12 = t00 * t21 - t01 * t20;
    let m20 = t01 * t12 - t02 * t11;
    let m21 = t00 * t12 - t02 * t10;
    let m22 = t00 * t11 - t01 * t10;

    let inv = [
         m00 / det, -m10 / det,  m20 / det,
        -m01 / det,  m11 / det, -m21 / det,
         m02 / det, -m12 / det,  m22 / det,
    ];

    Some(normalize(inv))
}

fn mul3x3(a: [f32; 9], b: [f32; 9]) -> [f32; 9] {
    let (a11, a12, a13, a21, a22, a23, a31, a32, a33) =
        (a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    let (b11, b12, b13, b21, b22, b23, b31, b32, b33) =
        (b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]);

    [
        a11 * b11 + a12 * b21 + a13 * b31,
        a11 * b12 + a12 * b22 + a13 * b32,
        a11 * b13 + a12 * b23 + a13 * b33,
        a21 * b11 + a22 * b21 + a23 * b31,
        a21 * b12 + a22 * b22 + a23 * b32,
        a21 * b13 + a22 * b23 + a23 * b33,
        a31 * b11 + a32 * b21 + a33 * b31,
        a31 * b12 + a32 * b22 + a33 * b32,
        a31 * b13 + a32 * b23 + a33 * b33,
    ]
}

fn blend<P>(
    top_left: P,
    top_right: P,
    bottom_left: P,
    bottom_right: P,
    right_weight: f32,
    bottom_weight: f32,
) -> P
where
    P: Pixel,
    P::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let top = top_left.map2(&top_right, |u, v| {
        P::Subpixel::clamp((1f32 - right_weight) * cast(u) + right_weight * cast(v))
    });

    let bottom = bottom_left.map2(&bottom_right, |u, v| {
        P::Subpixel::clamp((1f32 - right_weight) * cast(u) + right_weight * cast(v))
    });

    top.map2(&bottom, |u, v| {
        P::Subpixel::clamp((1f32 - bottom_weight) * cast(u) + bottom_weight * cast(v))
    })
}

fn interpolate<P>(image: &Image<P>, x: f32, y: f32, default: P) -> P
where
    P: Pixel + 'static,
    <P as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
{
    let left = x.floor();
    let right = left + 1f32;
    let top = y.floor();
    let bottom = top + 1f32;

    let right_weight = x - left;
    let bottom_weight = y - top;

    // default if out of bound
    let (width, height) = image.dimensions();
    if left < 0f32 || right >= width as f32 || top < 0f32 || bottom >= height as f32 {
        default
    } else {
        let (tl, tr, bl, br) = unsafe {
            (
                image.unsafe_get_pixel(left as u32, top as u32),
                image.unsafe_get_pixel(right as u32, top as u32),
                image.unsafe_get_pixel(left as u32, bottom as u32),
                image.unsafe_get_pixel(right as u32, bottom as u32),
            )
        };
        blend(tl, tr, bl, br, right_weight, bottom_weight)
    }
}

#[inline(always)]
fn nearest<P: Pixel + 'static>(image: &Image<P>, x: f32, y: f32, default: P) -> P {
    let rx = x.round();
    let ry = y.round();

    let (width, height) = image.dimensions();
    if rx < 0f32 || rx >= width as f32 || ry < 0f32 || ry >= height as f32 {
        default
    } else {
        unsafe { image.unsafe_get_pixel(rx as u32, ry as u32) }
    }
}

/// How to handle pixels whose pre-image lies between input pixels.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Interpolation {
    /// Choose the nearest pixel to the pre-image of the
    /// output pixel.
    Nearest,
    /// Bilinearly interpolate between the four pixels
    /// closest to the pre-image of the output pixel.
    Bilinear,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::gray_bench_image;
    use image::{GrayImage, Luma};
    use test::{Bencher, black_box};

    #[test]
    fn test_rotate_nearest_zero_radians() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12);

        let rotated = rotate(
            &image,
            (0f32, 0f32),
            0f32,
            Interpolation::Nearest,
            Luma([99u8]),
        );
        assert_pixels_eq!(rotated, image);
    }

    #[test]
    fn text_rotate_nearest_quarter_turn_clockwise() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12);

        let expected = gray_image!(
            11, 01, 99;
            12, 02, 99);
        let c = Projection::translate(1.0, 0.0);
        let rot = c * Projection::rotate(90f32.to_radians()) * c.invert();

        let rotated = warp(&image, &rot, Interpolation::Nearest, Luma([99u8]));
        assert_pixels_eq!(rotated, expected);
    }

    #[test]
    fn text_rotate_nearest_half_turn_anticlockwise() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12);

        let expected = gray_image!(
            12, 11, 10;
            02, 01, 00);
        let c = Projection::translate(1.0, 0.5);

        let rot = c * Projection::rotate((-180f32).to_radians()) * c.invert();

        let rotated = warp(&image, &rot, Interpolation::Nearest, Luma([99u8]));
        assert_pixels_eq!(rotated, expected);
    }

    #[bench]
    fn bench_rotate_nearest(b: &mut Bencher) {
        let image = GrayImage::from_pixel(200, 200, Luma([15u8]));
        let c = Projection::translate(3.0, 3.0);
        let rot = c * Projection::rotate(1f32.to_degrees()) * c.invert();
        b.iter(|| {
            let rotated = warp(&image, &rot, Interpolation::Nearest, Luma([98u8]));
            black_box(rotated);
        });
    }

    #[bench]
    fn bench_rotate_bilinear(b: &mut Bencher) {
        let image = GrayImage::from_pixel(200, 200, Luma([15u8]));
        let c = Projection::translate(3.0, 3.0);
        let rot = c * Projection::rotate(1f32.to_degrees()) * c.invert();
        b.iter(|| {
            let rotated = warp(&image, &rot, Interpolation::Bilinear, Luma([98u8]));
            black_box(rotated);
        });
    }

    #[test]
    fn test_translate_positive_x_positive_y() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 00, 01;
            00, 00, 01;
            10, 10, 11);

        let translated = translate(&image, (1, 1));
        assert_pixels_eq!(translated, expected);
    }

    #[test]
    fn test_translate_positive_x_negative_y() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            10, 10, 11;
            20, 20, 21;
            20, 20, 21);

        let translated = translate(&image, (1, -1));
        assert_pixels_eq!(translated, expected);
    }

    #[test]
    fn test_translate_large_x_large_y() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 00, 00;
            00, 00, 00;
            00, 00, 00);

        // Translating by more than the image width and height
        let translated = translate(&image, (5, 5));
        assert_pixels_eq!(translated, expected);
    }

    #[bench]
    fn bench_translate(b: &mut Bencher) {
        let image = gray_bench_image(500, 500);
        b.iter(|| {
            let translated = translate(&image, (30, 30));
            black_box(translated);
        });
    }

    #[test]
    fn test_translate_positive_x_positive_y_projection() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 00, 00;
            00, 00, 01;
            00, 10, 11);

        let translated = warp(
            &image,
            &Projection::translate(1.0, 1.0),
            Interpolation::Nearest,
            Luma([0u8]),
        );
        assert_pixels_eq!(translated, expected);
    }

    #[test]
    fn test_translate_positive_x_negative_y_projection() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 10, 11;
            00, 20, 21;
            00, 00, 00);

        let translated = warp(
            &image,
            &Projection::translate(1.0, -1.0),
            Interpolation::Nearest,
            Luma([0u8]),
        );
        assert_pixels_eq!(translated, expected);
    }

    #[test]
    fn test_translate_large_x_large_y_projection() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 00, 00;
            00, 00, 00;
            00, 00, 00);

        // Translating by more than the image width and height
        let translated = warp(
            &image,
            &Projection::translate(5.0, 5.0),
            Interpolation::Nearest,
            Luma([0u8]),
        );
        assert_pixels_eq!(translated, expected);
    }

    #[bench]
    fn bench_translate_projection(b: &mut Bencher) {
        let image = gray_bench_image(500, 500);
        let t = Projection::translate(-30.0, -30.0);

        b.iter(|| {
            let translated = warp(&image, &t, Interpolation::Nearest, Luma([0u8]));
            black_box(translated);
        });
    }

    #[bench]
    fn bench_translate_with(b: &mut Bencher) {
        let image = gray_bench_image(500, 500);

        b.iter(|| {
            let (width, height) = image.dimensions();
            let mut out = ImageBuffer::new(width, height);
            warp_into_with(
                &image,
                |x, y| (x - 30.0, y - 30.0),
                Interpolation::Nearest,
                Luma([0u8]),
                &mut out,
            );
            black_box(out);
        });
    }

    #[test]
    fn test_affine() {
        let image = gray_image!(
            00, 01, 02;
            10, 11, 12;
            20, 21, 22);

        let expected = gray_image!(
            00, 00, 00;
            00, 00, 01;
            00, 10, 11);

        let aff = Projection::from_matrix([
            1.0, 0.0, 1.0,
            0.0, 1.0, 1.0,
            0.0, 0.0, 1.0
        ]).unwrap();

        let translated = warp(&image, &aff, Interpolation::Nearest, Luma([0u8]));
        assert_pixels_eq!(translated, expected);
    }

    #[bench]
    fn bench_affine_nearest(b: &mut Bencher) {
        let image = GrayImage::from_pixel(200, 200, Luma([15u8]));

        let aff = Projection::from_matrix([
            1.0, 0.0, -1.0,
            0.0, 1.0, -1.0,
            0.0, 0.0,  1.0
        ]).unwrap();

        b.iter(|| {
            let transformed = warp(&image, &aff, Interpolation::Nearest, Luma([0u8]));
            black_box(transformed);
        });
    }

    #[bench]
    fn bench_affine_bilinear(b: &mut Bencher) {
        let image = GrayImage::from_pixel(200, 200, Luma([15u8]));

        let aff = Projection::from_matrix([
            1.8,      -0.2, 5.0,
            0.2,       1.9, 6.0,
            0.0002, 0.0003, 1.0
        ]).unwrap();

        b.iter(|| {
            let transformed = warp(&image, &aff, Interpolation::Bilinear, Luma([0u8]));
            black_box(transformed);
        });
    }

    #[test]
    fn test_from_control_points_translate() {
        let from = [(0f32, 0.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];
        let to = [(10f32, 5.0), (60.0, 55.0), (60.0, 5.0), (10.0, 55.0)];

        let p = Projection::from_control_points(from, to);
        assert!(p.is_some());

        let out = p.unwrap() * (0f32, 0f32);

        assert_approx_eq!(out.0, 10.0, 1e-10);
        assert_approx_eq!(out.1, 5.0, 1e-10);
    }

    #[test]
    fn test_from_control_points() {
        let from = [(0f32, 0.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];
        let to = [(16f32, 20.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];

        let p = Projection::from_control_points(from, to);
        assert!(p.is_some());

        let out = p.unwrap() * (0f32, 0f32);

        assert_approx_eq!(out.0, 16.0, 1e-10);
        assert_approx_eq!(out.1, 20.0, 1e-10);
    }

    #[test]
    fn test_from_control_points_known_transform() {
        let t = Projection::translate(10f32, 10f32);
        let p = t * Projection::rotate(90f32.to_radians()) * t.invert();

        let from = [(0f32, 0.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];
        let to = [p * from[0], p * from[1], p * from[2], p * from[3]];

        let p_est = Projection::from_control_points(from, to);
        assert!(p_est.is_some());
        let p_est = p_est.unwrap();

        for i in 0..50 {
            for j in 0..50 {
                let pt = (i as f32, j as f32);
                assert_approx_eq!((p * pt).0, (p_est * pt).0, 1e-3);
                assert_approx_eq!((p * pt).1, (p_est * pt).1, 1e-3);
            }
        }
    }

    #[test]
    fn test_from_control_points_colinear() {
        let from = [(0f32, 0.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];
        let to = [(0f32, 5.0), (0.0, 55.0), (0.0, 5.0), (10.0, 55.0)];

        let p = Projection::from_control_points(from, to);
        // Should fail if 3 points are colinear
        assert!(p.is_none());
    }

    #[bench]
    fn bench_from_control_points(b: &mut Bencher) {
        let from = [(0f32, 0.0), (50.0, 50.0), (50.0, 0.0), (0.0, 50.0)];
        let to = [(10f32, 5.0), (60.0, 55.0), (60.0, 5.0), (10.0, 55.0)];

        b.iter(|| {
            let proj = Projection::from_control_points(from, to);
            black_box(proj);
        });
    }
}
