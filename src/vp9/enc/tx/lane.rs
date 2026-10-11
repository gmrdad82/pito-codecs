use super::net::Lane;

pub trait Vector: Lane {
    const LANES: usize;
    fn zero() -> Self;
    fn load(p: &[i16]) -> Self;
    fn store(self, p: &mut [i16]);
    fn shl<const S: i32>(self) -> Self;
    fn rshr<const S: i32>(self) -> Self;
    fn add_to(self, dst: &mut [u8]);
}

pub trait Isa {
    type V4: Vector;
    type V8: Vector;
    type V16: Vector;
    fn transpose(buf: &mut [i16], n: usize);
    fn forward(buf: &mut [i16], tx_size: u8, tx_type: u8);
    fn inverse_add(
        buf: &mut [i16],
        tx_size: u8,
        tx_type: u8,
        rows: usize,
        dst: &mut [u8],
        stride: usize,
    );
}

#[derive(Clone, Copy)]
pub struct Cols<const W: usize>([i32; W]);

impl<const W: usize> Cols<W> {
    #[inline(always)]
    fn map(self, f: impl Fn(i32) -> i32) -> Self {
        Cols(self.0.map(f))
    }

    #[inline(always)]
    fn zip(a: Self, b: Self, f: impl Fn(i32, i32) -> i32) -> Self {
        let mut r = [0; W];
        for (i, v) in r.iter_mut().enumerate() {
            *v = f(a.0[i], b.0[i]);
        }
        Cols(r)
    }
}

#[inline(always)]
fn r14(v: i32) -> i32 {
    v.wrapping_add(1 << 13) >> 14
}

impl<const W: usize> Lane for Cols<W> {
    type W = Cols<W>;

    #[inline(always)]
    fn add(a: Self, b: Self) -> Self {
        Self::zip(a, b, i32::wrapping_add)
    }

    #[inline(always)]
    fn sub(a: Self, b: Self) -> Self {
        Self::zip(a, b, i32::wrapping_sub)
    }

    #[inline(always)]
    fn neg(a: Self) -> Self {
        a.map(i32::wrapping_neg)
    }

    #[inline(always)]
    fn rot(a: Self, b: Self, c0: i32, c1: i32) -> Self {
        Self::zip(a, b, |x, y| {
            r14(x.wrapping_mul(c0).wrapping_add(y.wrapping_mul(c1)))
        })
    }

    #[inline(always)]
    fn rot4(a: Self, b: Self, c: Self, d: Self, c0: i32, c1: i32, c2: i32, c3: i32) -> Self {
        let mut r = [0; W];
        for (i, v) in r.iter_mut().enumerate() {
            *v = r14(a.0[i]
                .wrapping_mul(c0)
                .wrapping_add(b.0[i].wrapping_mul(c1))
                .wrapping_add(c.0[i].wrapping_mul(c2))
                .wrapping_add(d.0[i].wrapping_mul(c3)));
        }
        Cols(r)
    }

    #[inline(always)]
    fn mulr(a: Self, c: i32) -> Self {
        a.map(|x| r14(x.wrapping_mul(c)))
    }

    #[inline(always)]
    fn prod(a: Self, b: Self, c0: i32, c1: i32) -> Self {
        Self::zip(a, b, |x, y| {
            x.wrapping_mul(c0).wrapping_add(y.wrapping_mul(c1))
        })
    }

    #[inline(always)]
    fn rsum(a: Self, b: Self) -> Self {
        Self::zip(a, b, |x, y| r14(x.wrapping_add(y)))
    }

    #[inline(always)]
    fn rdiff(a: Self, b: Self) -> Self {
        Self::zip(a, b, |x, y| r14(x.wrapping_sub(y)))
    }
}

impl<const W: usize> Vector for Cols<W> {
    const LANES: usize = W;

    #[inline(always)]
    fn zero() -> Self {
        Cols([0; W])
    }

    #[inline(always)]
    fn load(p: &[i16]) -> Self {
        let p = &p[..W];
        let mut r = [0; W];
        for i in 0..W {
            r[i] = p[i] as i32;
        }
        Cols(r)
    }

    #[inline(always)]
    fn store(self, p: &mut [i16]) {
        for (d, s) in p[..W].iter_mut().zip(self.0) {
            *d = s as i16;
        }
    }

    #[inline(always)]
    fn shl<const S: i32>(self) -> Self {
        self.map(|x| x << S)
    }

    #[inline(always)]
    fn rshr<const S: i32>(self) -> Self {
        self.map(|x| (x + (1 << (S - 1))) >> S)
    }

    #[inline(always)]
    fn add_to(self, dst: &mut [u8]) {
        for (d, s) in dst[..W].iter_mut().zip(self.0) {
            *d = (*d as i32 + s).clamp(0, 255) as u8;
        }
    }
}

pub struct Scalar;

impl Isa for Scalar {
    type V4 = Cols<4>;
    type V8 = Cols<8>;
    type V16 = Cols<8>;

    super::xform::kernels!(Scalar, Cols<4>, Cols<8>, Cols<8>, #[inline(never)]);

    #[inline(always)]
    fn transpose(buf: &mut [i16], n: usize) {
        for i in 0..n {
            for j in i + 1..n {
                buf.swap(i * n + j, j * n + i);
            }
        }
    }
}
