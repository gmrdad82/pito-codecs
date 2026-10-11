pub(super) struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn with_capacity(cap: usize) -> Self {
        BitWriter {
            out: Vec::with_capacity(cap),
            acc: 0,
            n: 0,
        }
    }

    #[inline(always)]
    pub fn put(&mut self, v: u32, bits: u32) {
        debug_assert!(bits <= 32 && (bits == 32 || v >> bits == 0));
        self.acc |= (v as u64) << self.n;
        self.n += bits;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.out
    }
}

pub(super) struct HuffCode {
    pub lengths: Vec<u8>,
    pub codes: Vec<u16>,
    pub emit: Vec<u8>,
}

impl HuffCode {
    pub fn from_histogram(hist: &[u32], max_len: u8) -> HuffCode {
        let mut lengths = vec![0u8; hist.len()];
        lengths_for(hist, max_len, &mut lengths);
        let used = lengths.iter().filter(|&&l| l > 0).count();
        let mut codes = vec![0u16; hist.len()];
        let emit = if used > 1 {
            canonical(&lengths, &mut codes);
            lengths.clone()
        } else {
            vec![0; hist.len()]
        };
        HuffCode {
            lengths,
            codes,
            emit,
        }
    }

    #[inline(always)]
    pub fn put(&self, w: &mut BitWriter, sym: usize) {
        w.put(self.codes[sym] as u32, self.emit[sym] as u32);
    }
}

fn canonical(lengths: &[u8], codes: &mut [u16]) {
    let mut count = [0u16; 17];
    for &l in lengths {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 17];
    let mut code = 0u16;
    for l in 1..17 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    for (s, &l) in lengths.iter().enumerate() {
        if l > 0 {
            let c = next[l as usize];
            next[l as usize] += 1;
            codes[s] = c.reverse_bits() >> (16 - l as u32);
        }
    }
}

fn lengths_for(hist: &[u32], max_len: u8, out: &mut [u8]) {
    let used: Vec<usize> = (0..hist.len()).filter(|&i| hist[i] > 0).collect();
    match used.len() {
        0 => return,
        1 => {
            out[used[0]] = 1;
            return;
        }
        _ => {}
    }
    let mut floor = 1u32;
    loop {
        let mut nodes: Vec<(u32, i32, i32)> = Vec::with_capacity(used.len() * 2);
        let mut leaves: Vec<(u32, usize)> = used.iter().map(|&s| (hist[s].max(floor), s)).collect();
        leaves.sort_unstable();
        for &(w, _) in &leaves {
            nodes.push((w, -1, -1));
        }
        let n = leaves.len();
        let mut q1 = 0usize;
        let mut q2 = n;
        let pick = |nodes: &Vec<(u32, i32, i32)>, q1: &mut usize, q2: &mut usize| -> usize {
            if *q1 < n && (*q2 >= nodes.len() || nodes[*q1].0 <= nodes[*q2].0) {
                *q1 += 1;
                *q1 - 1
            } else {
                *q2 += 1;
                *q2 - 1
            }
        };
        for _ in 0..n - 1 {
            let a = pick(&nodes, &mut q1, &mut q2);
            let b = pick(&nodes, &mut q1, &mut q2);
            let w = nodes[a].0.saturating_add(nodes[b].0);
            nodes.push((w, a as i32, b as i32));
        }
        let mut depth = vec![0u8; nodes.len()];
        let mut max = 0;
        for i in (n..nodes.len()).rev() {
            let (_, a, b) = nodes[i];
            depth[a as usize] = depth[i] + 1;
            depth[b as usize] = depth[i] + 1;
        }
        for (i, &(_, s)) in leaves.iter().enumerate() {
            out[s] = depth[i];
            max = max.max(depth[i]);
        }
        if max <= max_len {
            return;
        }
        floor *= 2;
    }
}

const ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

pub(super) fn write_code(w: &mut BitWriter, code: &HuffCode) {
    let used: Vec<usize> = (0..code.lengths.len())
        .filter(|&i| code.lengths[i] > 0)
        .collect();
    if used.is_empty() {
        w.put(0b0001, 4);
        return;
    }
    if used.len() <= 2 && used.iter().all(|&s| s < 256) {
        w.put(1, 1);
        w.put(used.len() as u32 - 1, 1);
        if used[0] < 2 {
            w.put(0, 1);
            w.put(used[0] as u32, 1);
        } else {
            w.put(1, 1);
            w.put(used[0] as u32, 8);
        }
        if used.len() == 2 {
            w.put(used[1] as u32, 8);
        }
        return;
    }
    w.put(0, 1);
    let mut tokens: Vec<(u8, u8)> = Vec::with_capacity(code.lengths.len());
    let lens = &code.lengths;
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        i += run;
        if v == 0 {
            let mut r = run;
            while r >= 11 {
                let k = r.min(138);
                tokens.push((18, (k - 11) as u8));
                r -= k;
            }
            if r >= 3 {
                tokens.push((17, (r - 3) as u8));
                r = 0;
            }
            for _ in 0..r {
                tokens.push((0, 0));
            }
        } else {
            tokens.push((v, 0));
            let mut r = run - 1;
            while r >= 3 {
                let k = r.min(6);
                tokens.push((16, (k - 3) as u8));
                r -= k;
            }
            for _ in 0..r {
                tokens.push((v, 0));
            }
        }
    }
    let mut keep = tokens.len();
    while keep > 0 && matches!(tokens[keep - 1].0, 0 | 17 | 18) {
        keep -= 1;
    }
    let trimmed = keep < tokens.len() && keep >= 2;
    let tokens = if trimmed {
        &tokens[..keep]
    } else {
        &tokens[..]
    };
    let mut hist = [0u32; 19];
    for &(t, _) in tokens {
        hist[t as usize] += 1;
    }
    let cl = HuffCode::from_histogram(&hist, 7);
    let single = cl.lengths.iter().filter(|&&l| l > 0).count() == 1;
    let mut n = 19;
    while n > 4 && cl.lengths[ORDER[n - 1]] == 0 {
        n -= 1;
    }
    w.put(n as u32 - 4, 4);
    for &o in &ORDER[..n] {
        w.put(cl.lengths[o] as u32, 3);
    }
    if trimmed {
        w.put(1, 1);
        let m = tokens.len() - 2;
        let mut k = 0;
        while m >> (2 + 2 * k) != 0 {
            k += 1;
        }
        w.put(k as u32, 3);
        w.put(m as u32, 2 + 2 * k as u32);
    } else {
        w.put(0, 1);
    }
    for &(t, extra) in tokens {
        if !single {
            cl.put(w, t as usize);
        }
        match t {
            16 => w.put(extra as u32, 2),
            17 => w.put(extra as u32, 3),
            18 => w.put(extra as u32, 7),
            _ => {}
        }
    }
}
