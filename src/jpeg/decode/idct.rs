const SCALE_BITS: i32 = 512 + 65536 + (128 << 17);

fn wa(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

fn ws(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

fn wm(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

fn fsh(x: i32) -> i32 {
    x.wrapping_shl(12)
}

fn clamp(a: i32) -> u8 {
    a.clamp(0, 255) as u8
}

fn dc_only(v: &[i32; 64], out: &mut [u8], stride: usize) {
    let c = clamp(wa(wa(v[0], 4), 1024) >> 3);
    for row in out.chunks_mut(stride).take(8) {
        if let Some(r) = row.get_mut(..8) {
            r.fill(c);
        }
    }
}

pub fn idct(v: &mut [i32; 64], out: &mut [u8], stride: usize) {
    if v[1..].iter().all(|&c| c == 0) {
        return dc_only(v, out, stride);
    }
    for c in 0..8 {
        if (1..8).all(|k| v[c + 8 * k] == 0) {
            let d = wa(fsh(v[c]), 512) >> 10;
            for k in 0..8 {
                v[c + 8 * k] = d;
            }
            continue;
        }
        let p2 = v[c + 16];
        let p3 = v[c + 48];
        let p1 = wm(wa(p2, p3), 2217);
        let t2 = wa(p1, wm(p3, -7567));
        let t3 = wa(p1, wm(p2, 3135));
        let p2 = v[c];
        let p3 = v[32 + c];
        let t0 = fsh(wa(p2, p3));
        let t1 = fsh(ws(p2, p3));
        let x0 = wa(wa(t0, t3), 512);
        let x3 = wa(ws(t0, t3), 512);
        let x1 = wa(wa(t1, t2), 512);
        let x2 = wa(ws(t1, t2), 512);
        let (t0, t1, t2, t3) = odd(v[c + 56], v[c + 40], v[c + 24], v[c + 8]);
        v[c] = wa(x0, t3) >> 10;
        v[c + 8] = wa(x1, t2) >> 10;
        v[c + 16] = wa(x2, t1) >> 10;
        v[c + 24] = wa(x3, t0) >> 10;
        v[c + 32] = ws(x3, t0) >> 10;
        v[c + 40] = ws(x2, t1) >> 10;
        v[c + 48] = ws(x1, t2) >> 10;
        v[c + 56] = ws(x0, t3) >> 10;
    }
    for (r, row) in v.chunks_exact(8).zip(out.chunks_mut(stride)) {
        let p2 = r[2];
        let p3 = r[6];
        let p1 = wm(wa(p2, p3), 2217);
        let t2 = wa(p1, wm(p3, -7567));
        let t3 = wa(p1, wm(p2, 3135));
        let t0 = fsh(wa(r[0], r[4]));
        let t1 = fsh(ws(r[0], r[4]));
        let x0 = wa(wa(t0, t3), SCALE_BITS);
        let x3 = wa(ws(t0, t3), SCALE_BITS);
        let x1 = wa(wa(t1, t2), SCALE_BITS);
        let x2 = wa(ws(t1, t2), SCALE_BITS);
        let (t0, t1, t2, t3) = odd(r[7], r[5], r[3], r[1]);
        let px = [
            clamp(wa(x0, t3) >> 17),
            clamp(wa(x1, t2) >> 17),
            clamp(wa(x2, t1) >> 17),
            clamp(wa(x3, t0) >> 17),
            clamp(ws(x3, t0) >> 17),
            clamp(ws(x2, t1) >> 17),
            clamp(ws(x1, t2) >> 17),
            clamp(ws(x0, t3) >> 17),
        ];
        if let Some(dst) = row.get_mut(..8) {
            dst.copy_from_slice(&px);
        }
    }
}

fn odd(t0: i32, t1: i32, t2: i32, t3: i32) -> (i32, i32, i32, i32) {
    let p3 = wa(t0, t2);
    let p4 = wa(t1, t3);
    let p1 = wa(t0, t3);
    let p2 = wa(t1, t2);
    let p5 = wm(wa(p3, p4), 4816);
    let t0 = wm(t0, 1223);
    let t1 = wm(t1, 8410);
    let t2 = wm(t2, 12586);
    let t3 = wm(t3, 6149);
    let p1 = wa(p5, wm(p1, -3685));
    let p2 = wa(p5, wm(p2, -10497));
    let p3 = wm(p3, -8034);
    let p4 = wm(p4, -1597);
    (
        wa(t0, wa(p1, p3)),
        wa(t1, wa(p2, p4)),
        wa(t2, wa(p2, p3)),
        wa(t3, wa(p1, p4)),
    )
}
