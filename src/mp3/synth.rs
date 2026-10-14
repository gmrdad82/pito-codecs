use super::simd::F4;
use super::tables::{SEC, WIN};

const SCALE: f32 = 1.0 / 32768.0;

fn dct2(g: &mut [f32], n: usize) {
    let mut k = 0;
    while k < n {
        let mut t = [[F4::set(0.0); 8]; 4];
        for i in 0..8 {
            let x0 = F4::ld(g, k + i * 18);
            let x1 = F4::ld(g, k + (15 - i) * 18);
            let x2 = F4::ld(g, k + (16 + i) * 18);
            let x3 = F4::ld(g, k + (31 - i) * 18);
            let t0 = x0.add(x3);
            let t1 = x1.add(x2);
            let t2 = x1.sub(x2).mul_s(SEC[3 * i]);
            let t3 = x0.sub(x3).mul_s(SEC[3 * i + 1]);
            t[0][i] = t0.add(t1);
            t[1][i] = t0.sub(t1).mul_s(SEC[3 * i + 2]);
            t[2][i] = t3.add(t2);
            t[3][i] = t3.sub(t2).mul_s(SEC[3 * i + 2]);
        }
        for x in t.iter_mut() {
            let [x0, x1, x2, x3, x4, x5, x6, x7] = *x;
            let xt = x0.sub(x7);
            let x0 = x0.add(x7);
            let x7 = x1.sub(x6);
            let x1 = x1.add(x6);
            let x6 = x2.sub(x5);
            let x2 = x2.add(x5);
            let x5 = x3.sub(x4);
            let x3 = x3.add(x4);
            let x4 = x0.sub(x3);
            let x0 = x0.add(x3);
            let x3 = x1.sub(x2);
            let x1 = x1.add(x2);
            x[0] = x0.add(x1);
            x[4] = x0.sub(x1).mul_s(0.70710677);
            let x5 = x5.add(x6);
            let x6 = x6.add(x7).mul_s(0.70710677);
            let x7 = x7.add(xt);
            let x3 = x3.add(x4).mul_s(0.70710677);
            let x5 = x5.sub(x7.mul_s(0.198912367));
            let x7 = x7.add(x5.mul_s(0.382683432));
            let x5 = x5.sub(x7.mul_s(0.198912367));
            let x0 = xt.sub(x6);
            let xt = xt.add(x6);
            x[1] = xt.add(x7).mul_s(0.50979561);
            x[2] = x4.add(x3).mul_s(0.54119611);
            x[3] = x0.sub(x5).mul_s(0.60134488);
            x[5] = x0.add(x5).mul_s(0.89997619);
            x[6] = x4.sub(x3).mul_s(1.30656302);
            x[7] = xt.sub(x7).mul_s(2.56291556);
        }
        let two = k + 3 > n;
        let put = |g: &mut [f32], at: usize, v: F4| {
            if two {
                let l = v.lanes();
                g[at] = l[0];
                g[at + 1] = l[1];
            } else {
                v.st(g, at);
            }
        };
        let mut y = k;
        for i in 0..7 {
            let s = t[3][i].add(t[3][i + 1]);
            put(g, y, t[0][i]);
            put(g, y + 18, t[2][i].add(s));
            put(g, y + 36, t[1][i].add(t[1][i + 1]));
            put(g, y + 54, t[2][i + 1].add(s));
            y += 72;
        }
        put(g, y, t[0][7]);
        put(g, y + 18, t[2][7].add(t[3][7]));
        put(g, y + 36, t[1][7]);
        put(g, y + 54, t[3][7]);
        k += 4;
    }
}

fn synth_pair(pcm: &mut [f32], at: usize, nch: usize, z: &[f32], zi: usize) {
    let z0 = |k: usize| z[zi + k * 64];
    let mut a = (z0(14) - z0(0)) * 29.0;
    a += (z0(1) + z0(13)) * 213.0;
    a += (z0(12) - z0(2)) * 459.0;
    a += (z0(3) + z0(11)) * 2037.0;
    a += (z0(10) - z0(4)) * 5153.0;
    a += (z0(5) + z0(9)) * 6574.0;
    a += (z0(8) - z0(6)) * 37489.0;
    a += z0(7) * 75038.0;
    pcm[at] = a * SCALE;
    let z2 = |k: usize| z[zi + 2 + k * 64];
    let mut a = z2(14) * 104.0;
    a += z2(12) * 1567.0;
    a += z2(10) * 9727.0;
    a += z2(8) * 64019.0;
    a += z2(6) * -9975.0;
    a += z2(4) * -45.0;
    a += z2(2) * 146.0;
    a += z2(0) * -5.0;
    pcm[at + 16 * nch] = a * SCALE;
}

fn synth(
    g: &[f32],
    xl: usize,
    nch: usize,
    pcm: &mut [f32],
    dst: usize,
    lins: &mut [f32],
    lb: usize,
) {
    let xr = xl + 576 * (nch - 1);
    let dstl = dst;
    let dstr = dst + nch - 1;
    let zl = lb + 15 * 64;
    lins[zl + 60] = g[xl + 288];
    lins[zl + 61] = g[xr + 288];
    lins[zl + 62] = g[xl];
    lins[zl + 63] = g[xr];
    lins[zl + 124] = g[xl + 1 + 288];
    lins[zl + 125] = g[xr + 1 + 288];
    lins[zl + 126] = g[xl + 1];
    lins[zl + 127] = g[xr + 1];
    synth_pair(pcm, dstr, nch, lins, lb + 61);
    synth_pair(pcm, dstr + 32 * nch, nch, lins, lb + 61 + 64);
    synth_pair(pcm, dstl, nch, lins, lb + 60);
    synth_pair(pcm, dstl + 32 * nch, nch, lins, lb + 60 + 64);
    let scale = F4::set(SCALE);
    let mut w = 0;
    for i in (0..15).rev() {
        let zi = zl + 4 * i;
        lins[zi] = g[xl + 18 * (31 - i)];
        lins[zi + 1] = g[xr + 18 * (31 - i)];
        lins[zi + 2] = g[xl + 1 + 18 * (31 - i)];
        lins[zi + 3] = g[xr + 1 + 18 * (31 - i)];
        lins[zi + 64] = g[xl + 1 + 18 * (1 + i)];
        lins[zi + 65] = g[xr + 1 + 18 * (1 + i)];
        lins[zi - 62] = g[xl + 18 * (1 + i)];
        lins[zi - 61] = g[xr + 18 * (1 + i)];
        let mut a = F4::set(0.0);
        let mut b = F4::set(0.0);
        for k in 0..8 {
            let w0 = F4::set(WIN[w]);
            let w1 = F4::set(WIN[w + 1]);
            w += 2;
            let vz = F4::ld(lins, zi - 64 * k);
            let vy = F4::ld(lins, zi - 64 * (15 - k));
            let bb = vz.mul(w1).add(vy.mul(w0));
            if k == 0 {
                b = bb;
                a = vz.mul(w0).sub(vy.mul(w1));
            } else if k % 2 == 1 {
                b = b.add(bb);
                a = a.add(vy.mul(w1).sub(vz.mul(w0)));
            } else {
                b = b.add(bb);
                a = a.add(vz.mul(w0).sub(vy.mul(w1)));
            }
        }
        let a = a.mul(scale).lanes();
        let b = b.mul(scale).lanes();
        pcm[dstr + (15 - i) * nch] = a[1];
        pcm[dstr + (17 + i) * nch] = b[1];
        pcm[dstl + (15 - i) * nch] = a[0];
        pcm[dstl + (17 + i) * nch] = b[0];
        pcm[dstr + (47 - i) * nch] = a[3];
        pcm[dstr + (49 + i) * nch] = b[3];
        pcm[dstl + (47 - i) * nch] = a[2];
        pcm[dstl + (49 + i) * nch] = b[2];
    }
}

pub fn granule(
    qmf: &mut [f32; 960],
    g: &mut [f32],
    nbands: usize,
    nch: usize,
    pcm: &mut [f32],
    lins: &mut [f32],
) {
    for i in 0..nch {
        dct2(&mut g[576 * i..], nbands);
    }
    lins[..960].copy_from_slice(qmf);
    for i in (0..nbands).step_by(2) {
        synth(g, i, nch, pcm, 32 * nch * i, lins, i * 64);
    }
    let tail = &lins[nbands * 64..nbands * 64 + 960];
    if nch == 1 {
        for i in (0..960).step_by(2) {
            qmf[i] = tail[i];
        }
    } else {
        qmf.copy_from_slice(tail);
    }
}
