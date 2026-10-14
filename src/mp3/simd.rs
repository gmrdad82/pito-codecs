#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
pub struct F4(__m128);

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
pub struct F4(float32x4_t);

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Clone, Copy)]
pub struct F4([f32; 4]);

#[cfg(target_arch = "x86_64")]
#[allow(unused_unsafe)]
impl F4 {
    #[inline(always)]
    pub fn ld(s: &[f32], i: usize) -> F4 {
        let c = &s[i..i + 4];
        F4(unsafe { _mm_loadu_ps(c.as_ptr()) })
    }

    #[inline(always)]
    pub fn st(self, s: &mut [f32], i: usize) {
        let c = &mut s[i..i + 4];
        unsafe { _mm_storeu_ps(c.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    pub fn set(v: f32) -> F4 {
        F4(unsafe { _mm_set1_ps(v) })
    }

    #[inline(always)]
    pub fn add(self, b: F4) -> F4 {
        F4(unsafe { _mm_add_ps(self.0, b.0) })
    }

    #[inline(always)]
    pub fn sub(self, b: F4) -> F4 {
        F4(unsafe { _mm_sub_ps(self.0, b.0) })
    }

    #[inline(always)]
    pub fn mul(self, b: F4) -> F4 {
        F4(unsafe { _mm_mul_ps(self.0, b.0) })
    }

    #[inline(always)]
    pub fn rev(self) -> F4 {
        F4(unsafe { _mm_shuffle_ps::<0x1B>(self.0, self.0) })
    }

    #[inline(always)]
    pub fn lanes(self) -> [f32; 4] {
        let mut o = [0.0; 4];
        unsafe { _mm_storeu_ps(o.as_mut_ptr(), self.0) };
        o
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unused_unsafe)]
impl F4 {
    #[inline(always)]
    pub fn ld(s: &[f32], i: usize) -> F4 {
        let c = &s[i..i + 4];
        F4(unsafe { vld1q_f32(c.as_ptr()) })
    }

    #[inline(always)]
    pub fn st(self, s: &mut [f32], i: usize) {
        let c = &mut s[i..i + 4];
        unsafe { vst1q_f32(c.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    pub fn set(v: f32) -> F4 {
        F4(unsafe { vdupq_n_f32(v) })
    }

    #[inline(always)]
    pub fn add(self, b: F4) -> F4 {
        F4(unsafe { vaddq_f32(self.0, b.0) })
    }

    #[inline(always)]
    pub fn sub(self, b: F4) -> F4 {
        F4(unsafe { vsubq_f32(self.0, b.0) })
    }

    #[inline(always)]
    pub fn mul(self, b: F4) -> F4 {
        F4(unsafe { vmulq_f32(self.0, b.0) })
    }

    #[inline(always)]
    pub fn rev(self) -> F4 {
        F4(unsafe {
            let r = vrev64q_f32(self.0);
            vcombine_f32(vget_high_f32(r), vget_low_f32(r))
        })
    }

    #[inline(always)]
    pub fn lanes(self) -> [f32; 4] {
        let mut o = [0.0; 4];
        unsafe { vst1q_f32(o.as_mut_ptr(), self.0) };
        o
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
impl F4 {
    #[inline(always)]
    pub fn ld(s: &[f32], i: usize) -> F4 {
        let mut o = [0.0; 4];
        o.copy_from_slice(&s[i..i + 4]);
        F4(o)
    }

    #[inline(always)]
    pub fn st(self, s: &mut [f32], i: usize) {
        s[i..i + 4].copy_from_slice(&self.0);
    }

    #[inline(always)]
    pub fn set(v: f32) -> F4 {
        F4([v; 4])
    }

    #[inline(always)]
    pub fn add(self, b: F4) -> F4 {
        F4(std::array::from_fn(|k| self.0[k] + b.0[k]))
    }

    #[inline(always)]
    pub fn sub(self, b: F4) -> F4 {
        F4(std::array::from_fn(|k| self.0[k] - b.0[k]))
    }

    #[inline(always)]
    pub fn mul(self, b: F4) -> F4 {
        F4(std::array::from_fn(|k| self.0[k] * b.0[k]))
    }

    #[inline(always)]
    pub fn rev(self) -> F4 {
        F4([self.0[3], self.0[2], self.0[1], self.0[0]])
    }

    #[inline(always)]
    pub fn lanes(self) -> [f32; 4] {
        self.0
    }
}

impl F4 {
    #[inline(always)]
    pub fn mul_s(self, s: f32) -> F4 {
        self.mul(F4::set(s))
    }
}
