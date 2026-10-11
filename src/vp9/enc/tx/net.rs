pub trait Lane: Copy {
    type W: Copy;
    fn add(a: Self, b: Self) -> Self;
    fn sub(a: Self, b: Self) -> Self;
    fn neg(a: Self) -> Self;
    fn rot(a: Self, b: Self, c0: i32, c1: i32) -> Self;
    #[allow(clippy::too_many_arguments)]
    fn rot4(a: Self, b: Self, c: Self, d: Self, c0: i32, c1: i32, c2: i32, c3: i32) -> Self;
    fn mulr(a: Self, c: i32) -> Self;
    fn prod(a: Self, b: Self, c0: i32, c1: i32) -> Self::W;
    fn rsum(a: Self::W, b: Self::W) -> Self;
    fn rdiff(a: Self::W, b: Self::W) -> Self;
}

const S1: i32 = 5283;
const S2: i32 = 9929;
const S3: i32 = 13377;
const S4: i32 = 15212;

#[inline(always)]
pub fn fadst4<L: Lane>(y: [L; 4]) -> [L; 4] {
    let t = L::sub(L::add(y[0], y[1]), y[3]);
    [
        L::rot4(y[0], y[1], y[2], y[3], S1, S2, S3, S4),
        L::mulr(t, S3),
        L::rot4(y[0], y[1], y[2], y[3], S4, -S1, -S3, S2),
        L::rot4(y[0], y[1], y[2], y[3], S2, -S4, S3, -S1),
    ]
}

#[inline(always)]
pub fn iadst4<L: Lane>(x: [L; 4]) -> [L; 4] {
    [
        L::rot4(x[0], x[1], x[2], x[3], S1, S3, S4, S2),
        L::rot4(x[0], x[1], x[2], x[3], S2, S3, -S1, -S4),
        L::rot4(x[0], x[1], x[2], x[3], S3, 0, -S3, S3),
        L::rot4(x[0], x[1], x[2], x[3], S4, -S3, S2, -S1),
    ]
}

#[inline(always)]
pub fn fdct4<L: Lane>(y: [L; 4]) -> [L; 4] {
    let a3 = L::sub(y[3], y[0]);
    let a2 = L::sub(y[2], y[1]);
    let a1 = L::add(y[3], y[0]);
    let a0 = L::add(y[2], y[1]);
    let c0 = L::rot(a1, a0, 11585, 11585);
    let c1 = L::rot(a3, a2, -15137, -6270);
    let c2 = L::rot(a1, a0, 11585, -11585);
    let c3 = L::rot(a3, a2, -6270, 15137);
    [c0, c1, c2, c3]
}

#[inline(always)]
pub fn fdct8<L: Lane>(y: [L; 8]) -> [L; 8] {
    let a17 = L::sub(y[6], y[1]);
    let a16 = L::sub(y[5], y[2]);
    let a15 = L::rot(a17, a16, -11585, -11585);
    let a14 = L::sub(y[7], y[0]);
    let a13 = L::rot(a17, a16, -11585, 11585);
    let a12 = L::sub(y[4], y[3]);
    let a11 = L::add(a15, a14);
    let a10 = L::add(a13, a12);
    let a9 = L::sub(a15, a14);
    let a8 = L::sub(a13, a12);
    let a7 = L::add(y[5], y[2]);
    let a6 = L::add(y[6], y[1]);
    let a5 = L::add(y[4], y[3]);
    let a4 = L::add(y[7], y[0]);
    let a3 = L::sub(a5, a4);
    let a2 = L::sub(a7, a6);
    let a1 = L::add(a5, a4);
    let a0 = L::add(a7, a6);
    let c0 = L::rot(a1, a0, 11585, 11585);
    let c1 = L::rot(a9, a8, 16069, 3196);
    let c2 = L::rot(a3, a2, -15137, -6270);
    let c3 = L::rot(a11, a10, -13623, 9102);
    let c4 = L::rot(a1, a0, 11585, -11585);
    let c5 = L::rot(a11, a10, -9102, -13623);
    let c6 = L::rot(a3, a2, -6270, 15137);
    let c7 = L::rot(a9, a8, 3196, -16069);
    [c0, c1, c2, c3, c4, c5, c6, c7]
}

#[inline(always)]
pub fn fdct16<L: Lane>(y: [L; 16]) -> [L; 16] {
    let a57 = L::sub(y[12], y[3]);
    let a56 = L::sub(y[11], y[4]);
    let a55 = L::sub(y[13], y[2]);
    let a54 = L::sub(y[10], y[5]);
    let a53 = L::rot(a55, a54, -11585, -11585);
    let a52 = L::sub(y[14], y[1]);
    let a51 = L::rot(a55, a54, -11585, 11585);
    let a50 = L::sub(y[9], y[6]);
    let a49 = L::rot(a57, a56, -11585, -11585);
    let a48 = L::sub(y[15], y[0]);
    let a47 = L::rot(a57, a56, -11585, 11585);
    let a46 = L::sub(y[8], y[7]);
    let a45 = L::add(a51, a50);
    let a44 = L::add(a53, a52);
    let a43 = L::sub(a53, a52);
    let a42 = L::sub(a51, a50);
    let a41 = L::rot(a43, a42, 15137, 6270);
    let a40 = L::sub(a49, a48);
    let a39 = L::rot(a45, a44, 15137, -6270);
    let a38 = L::add(a49, a48);
    let a37 = L::rot(a45, a44, 6270, 15137);
    let a36 = L::add(a47, a46);
    let a35 = L::rot(a43, a42, 6270, -15137);
    let a34 = L::sub(a47, a46);
    let a33 = L::sub(a39, a38);
    let a32 = L::sub(a37, a36);
    let a31 = L::add(a39, a38);
    let a30 = L::add(a37, a36);
    let a29 = L::sub(a41, a40);
    let a28 = L::sub(a35, a34);
    let a27 = L::add(a41, a40);
    let a26 = L::add(a35, a34);
    let a25 = L::add(y[11], y[4]);
    let a24 = L::add(y[12], y[3]);
    let a23 = L::add(y[10], y[5]);
    let a22 = L::add(y[13], y[2]);
    let a21 = L::add(y[9], y[6]);
    let a20 = L::add(y[14], y[1]);
    let a19 = L::add(y[8], y[7]);
    let a18 = L::add(y[15], y[0]);
    let a17 = L::sub(a21, a20);
    let a16 = L::sub(a23, a22);
    let a15 = L::rot(a17, a16, -11585, -11585);
    let a14 = L::sub(a19, a18);
    let a13 = L::rot(a17, a16, -11585, 11585);
    let a12 = L::sub(a25, a24);
    let a11 = L::add(a15, a14);
    let a10 = L::add(a13, a12);
    let a9 = L::sub(a15, a14);
    let a8 = L::sub(a13, a12);
    let a7 = L::add(a23, a22);
    let a6 = L::add(a21, a20);
    let a5 = L::add(a25, a24);
    let a4 = L::add(a19, a18);
    let a3 = L::sub(a5, a4);
    let a2 = L::sub(a7, a6);
    let a1 = L::add(a5, a4);
    let a0 = L::add(a7, a6);
    let c0 = L::rot(a1, a0, 11585, 11585);
    let c1 = L::rot(a27, a26, 16305, 1606);
    let c2 = L::rot(a9, a8, 16069, 3196);
    let c3 = L::rot(a33, a32, 15679, -4756);
    let c4 = L::rot(a3, a2, -15137, -6270);
    let c5 = L::rot(a31, a30, -14449, -7723);
    let c6 = L::rot(a11, a10, -13623, 9102);
    let c7 = L::rot(a29, a28, -12665, 10394);
    let c8 = L::rot(a1, a0, 11585, -11585);
    let c9 = L::rot(a29, a28, -10394, -12665);
    let c10 = L::rot(a11, a10, -9102, -13623);
    let c11 = L::rot(a31, a30, -7723, 14449);
    let c12 = L::rot(a3, a2, -6270, 15137);
    let c13 = L::rot(a33, a32, 4756, 15679);
    let c14 = L::rot(a9, a8, 3196, -16069);
    let c15 = L::rot(a27, a26, 1606, -16305);
    [
        c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15,
    ]
}

#[inline(always)]
pub fn fdct32<L: Lane>(y: [L; 32]) -> [L; 32] {
    let a161 = L::sub(y[24], y[7]);
    let a160 = L::sub(y[23], y[8]);
    let a159 = L::sub(y[25], y[6]);
    let a158 = L::sub(y[22], y[9]);
    let a157 = L::sub(y[26], y[5]);
    let a156 = L::sub(y[21], y[10]);
    let a155 = L::sub(y[27], y[4]);
    let a154 = L::sub(y[20], y[11]);
    let a153 = L::rot(a155, a154, -11585, -11585);
    let a152 = L::sub(y[28], y[3]);
    let a151 = L::rot(a155, a154, -11585, 11585);
    let a150 = L::sub(y[19], y[12]);
    let a149 = L::rot(a157, a156, -11585, -11585);
    let a148 = L::sub(y[29], y[2]);
    let a147 = L::rot(a157, a156, -11585, 11585);
    let a146 = L::sub(y[18], y[13]);
    let a145 = L::rot(a159, a158, -11585, -11585);
    let a144 = L::sub(y[30], y[1]);
    let a143 = L::rot(a159, a158, -11585, 11585);
    let a142 = L::sub(y[17], y[14]);
    let a141 = L::rot(a161, a160, -11585, -11585);
    let a140 = L::sub(y[31], y[0]);
    let a139 = L::rot(a161, a160, -11585, 11585);
    let a138 = L::sub(y[16], y[15]);
    let a137 = L::add(a151, a150);
    let a136 = L::add(a153, a152);
    let a135 = L::sub(a153, a152);
    let a134 = L::sub(a151, a150);
    let a133 = L::add(a147, a146);
    let a132 = L::add(a149, a148);
    let a131 = L::sub(a149, a148);
    let a130 = L::sub(a147, a146);
    let a129 = L::rot(a131, a130, 15137, 6270);
    let a128 = L::sub(a145, a144);
    let a127 = L::rot(a133, a132, 15137, -6270);
    let a126 = L::add(a145, a144);
    let a125 = L::rot(a133, a132, 6270, 15137);
    let a124 = L::add(a143, a142);
    let a123 = L::rot(a131, a130, 6270, -15137);
    let a122 = L::sub(a143, a142);
    let a121 = L::rot(a135, a134, 15137, 6270);
    let a120 = L::sub(a141, a140);
    let a119 = L::rot(a137, a136, 15137, -6270);
    let a118 = L::add(a141, a140);
    let a117 = L::rot(a137, a136, 6270, 15137);
    let a116 = L::add(a139, a138);
    let a115 = L::rot(a135, a134, 6270, -15137);
    let a114 = L::sub(a139, a138);
    let a113 = L::sub(a123, a122);
    let a112 = L::sub(a129, a128);
    let a111 = L::add(a127, a126);
    let a110 = L::add(a125, a124);
    let a109 = L::sub(a125, a124);
    let a108 = L::sub(a127, a126);
    let a107 = L::add(a129, a128);
    let a106 = L::add(a123, a122);
    let a105 = L::rot(a107, a106, 16069, 3196);
    let a104 = L::add(a121, a120);
    let a103 = L::rot(a113, a112, 16069, -3196);
    let a102 = L::sub(a121, a120);
    let a101 = L::rot(a111, a110, -9102, -13623);
    let a100 = L::add(a119, a118);
    let a99 = L::rot(a109, a108, -9102, 13623);
    let a98 = L::sub(a119, a118);
    let a97 = L::rot(a109, a108, -13623, -9102);
    let a96 = L::sub(a117, a116);
    let a95 = L::rot(a111, a110, -13623, 9102);
    let a94 = L::add(a117, a116);
    let a93 = L::rot(a113, a112, 3196, 16069);
    let a92 = L::sub(a115, a114);
    let a91 = L::rot(a107, a106, 3196, -16069);
    let a90 = L::add(a115, a114);
    let a89 = L::add(a99, a98);
    let a88 = L::add(a97, a96);
    let a87 = L::sub(a99, a98);
    let a86 = L::sub(a97, a96);
    let a85 = L::add(a101, a100);
    let a84 = L::add(a95, a94);
    let a83 = L::sub(a101, a100);
    let a82 = L::sub(a95, a94);
    let a81 = L::sub(a103, a102);
    let a80 = L::sub(a93, a92);
    let a79 = L::add(a103, a102);
    let a78 = L::add(a93, a92);
    let a77 = L::sub(a105, a104);
    let a76 = L::sub(a91, a90);
    let a75 = L::add(a105, a104);
    let a74 = L::add(a91, a90);
    let a73 = L::add(y[23], y[8]);
    let a72 = L::add(y[24], y[7]);
    let a71 = L::add(y[22], y[9]);
    let a70 = L::add(y[25], y[6]);
    let a69 = L::add(y[21], y[10]);
    let a68 = L::add(y[26], y[5]);
    let a67 = L::add(y[20], y[11]);
    let a66 = L::add(y[27], y[4]);
    let a65 = L::add(y[19], y[12]);
    let a64 = L::add(y[28], y[3]);
    let a63 = L::add(y[18], y[13]);
    let a62 = L::add(y[29], y[2]);
    let a61 = L::add(y[17], y[14]);
    let a60 = L::add(y[30], y[1]);
    let a59 = L::add(y[16], y[15]);
    let a58 = L::add(y[31], y[0]);
    let a57 = L::sub(a65, a64);
    let a56 = L::sub(a67, a66);
    let a55 = L::sub(a63, a62);
    let a54 = L::sub(a69, a68);
    let a53 = L::rot(a55, a54, -11585, -11585);
    let a52 = L::sub(a61, a60);
    let a51 = L::rot(a55, a54, -11585, 11585);
    let a50 = L::sub(a71, a70);
    let a49 = L::rot(a57, a56, -11585, -11585);
    let a48 = L::sub(a59, a58);
    let a47 = L::rot(a57, a56, -11585, 11585);
    let a46 = L::sub(a73, a72);
    let a45 = L::add(a51, a50);
    let a44 = L::add(a53, a52);
    let a43 = L::sub(a53, a52);
    let a42 = L::sub(a51, a50);
    let a41 = L::rot(a43, a42, 15137, 6270);
    let a40 = L::sub(a49, a48);
    let a39 = L::rot(a45, a44, 15137, -6270);
    let a38 = L::add(a49, a48);
    let a37 = L::rot(a45, a44, 6270, 15137);
    let a36 = L::add(a47, a46);
    let a35 = L::rot(a43, a42, 6270, -15137);
    let a34 = L::sub(a47, a46);
    let a33 = L::sub(a39, a38);
    let a32 = L::sub(a37, a36);
    let a31 = L::add(a39, a38);
    let a30 = L::add(a37, a36);
    let a29 = L::sub(a41, a40);
    let a28 = L::sub(a35, a34);
    let a27 = L::add(a41, a40);
    let a26 = L::add(a35, a34);
    let a25 = L::add(a67, a66);
    let a24 = L::add(a65, a64);
    let a23 = L::add(a69, a68);
    let a22 = L::add(a63, a62);
    let a21 = L::add(a71, a70);
    let a20 = L::add(a61, a60);
    let a19 = L::add(a73, a72);
    let a18 = L::add(a59, a58);
    let a17 = L::sub(a21, a20);
    let a16 = L::sub(a23, a22);
    let a15 = L::rot(a17, a16, -11585, -11585);
    let a14 = L::sub(a19, a18);
    let a13 = L::rot(a17, a16, -11585, 11585);
    let a12 = L::sub(a25, a24);
    let a11 = L::add(a15, a14);
    let a10 = L::add(a13, a12);
    let a9 = L::sub(a15, a14);
    let a8 = L::sub(a13, a12);
    let a7 = L::add(a23, a22);
    let a6 = L::add(a21, a20);
    let a5 = L::add(a25, a24);
    let a4 = L::add(a19, a18);
    let a3 = L::sub(a5, a4);
    let a2 = L::sub(a7, a6);
    let a1 = L::add(a5, a4);
    let a0 = L::add(a7, a6);
    let c0 = L::rot(a1, a0, 11585, 11585);
    let c1 = L::rot(a75, a74, 16364, 804);
    let c2 = L::rot(a27, a26, 16305, 1606);
    let c3 = L::rot(a89, a88, 16207, -2404);
    let c4 = L::rot(a9, a8, 16069, 3196);
    let c5 = L::rot(a83, a82, 15893, 3981);
    let c6 = L::rot(a33, a32, 15679, -4756);
    let c7 = L::rot(a81, a80, 15426, -5520);
    let c8 = L::rot(a3, a2, -15137, -6270);
    let c9 = L::rot(a79, a78, -14811, -7005);
    let c10 = L::rot(a31, a30, -14449, -7723);
    let c11 = L::rot(a85, a84, -14053, 8423);
    let c12 = L::rot(a11, a10, -13623, 9102);
    let c13 = L::rot(a87, a86, -13160, -9760);
    let c14 = L::rot(a29, a28, -12665, 10394);
    let c15 = L::rot(a77, a76, -12140, 11003);
    let c16 = L::rot(a1, a0, 11585, -11585);
    let c17 = L::rot(a77, a76, -11003, -12140);
    let c18 = L::rot(a29, a28, -10394, -12665);
    let c19 = L::rot(a87, a86, -9760, 13160);
    let c20 = L::rot(a11, a10, -9102, -13623);
    let c21 = L::rot(a85, a84, -8423, -14053);
    let c22 = L::rot(a31, a30, -7723, 14449);
    let c23 = L::rot(a79, a78, -7005, 14811);
    let c24 = L::rot(a3, a2, -6270, 15137);
    let c25 = L::rot(a81, a80, 5520, 15426);
    let c26 = L::rot(a33, a32, 4756, 15679);
    let c27 = L::rot(a83, a82, 3981, -15893);
    let c28 = L::rot(a9, a8, 3196, -16069);
    let c29 = L::rot(a89, a88, 2404, 16207);
    let c30 = L::rot(a27, a26, 1606, -16305);
    let c31 = L::rot(a75, a74, 804, -16364);
    [
        c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15, c16, c17, c18, c19,
        c20, c21, c22, c23, c24, c25, c26, c27, c28, c29, c30, c31,
    ]
}

#[inline(always)]
pub fn fadst8<L: Lane>(y: [L; 8]) -> [L; 8] {
    let a27 = L::rot(y[3], y[4], -11585, -11585);
    let a25 = L::rot(y[3], y[4], -11585, 11585);
    let a23 = L::rot(y[2], y[5], 11585, 11585);
    let a21 = L::rot(y[2], y[5], 11585, -11585);
    let a19 = L::sub(a23, y[6]);
    let a18 = L::add(a21, y[1]);
    let a17 = L::sub(a21, y[1]);
    let a16 = L::add(a23, y[6]);
    let a15 = L::rot(a19, a18, -6270, -15137);
    let a14 = L::add(a27, y[7]);
    let a13 = L::rot(a19, a18, -15137, 6270);
    let a12 = L::sub(a25, y[0]);
    let a11 = L::rot(a17, a16, 6270, -15137);
    let a10 = L::sub(a27, y[7]);
    let a9 = L::rot(a17, a16, 15137, 6270);
    let a8 = L::add(a25, y[0]);
    let a7 = L::add(a13, a12);
    let a6 = L::add(a15, a14);
    let a5 = L::sub(a9, a8);
    let a4 = L::sub(a11, a10);
    let a3 = L::sub(a13, a12);
    let a2 = L::sub(a15, a14);
    let a1 = L::add(a9, a8);
    let a0 = L::add(a11, a10);
    let c0 = L::rot(a1, a0, 1606, -16305);
    let c1 = L::rot(a7, a6, -4756, -15679);
    let c2 = L::rot(a3, a2, 7723, -14449);
    let c3 = L::rot(a5, a4, -10394, -12665);
    let c4 = L::rot(a5, a4, -12665, 10394);
    let c5 = L::rot(a3, a2, 14449, 7723);
    let c6 = L::rot(a7, a6, -15679, 4756);
    let c7 = L::rot(a1, a0, 16305, 1606);
    [c0, c1, c2, c3, c4, c5, c6, c7]
}

#[inline(always)]
pub fn fadst16<L: Lane>(y: [L; 16]) -> [L; 16] {
    let a79 = L::rot(y[9], y[6], 11585, 11585);
    let a77 = L::rot(y[8], y[7], -11585, -11585);
    let a75 = L::rot(y[9], y[6], -11585, 11585);
    let a73 = L::rot(y[8], y[7], 11585, -11585);
    let a71 = L::rot(y[10], y[5], -11585, -11585);
    let a69 = L::rot(y[11], y[4], 11585, 11585);
    let a67 = L::rot(y[10], y[5], 11585, -11585);
    let a65 = L::rot(y[11], y[4], -11585, 11585);
    let a63 = L::add(a71, y[13]);
    let a62 = L::sub(a67, y[2]);
    let a61 = L::add(a67, y[2]);
    let a60 = L::sub(a71, y[13]);
    let a59 = L::sub(a69, y[12]);
    let a58 = L::add(a65, y[3]);
    let a57 = L::sub(a65, y[3]);
    let a56 = L::add(a69, y[12]);
    let a55 = L::rot(a59, a58, -6270, -15137);
    let a54 = L::add(a77, y[15]);
    let a53 = L::rot(a59, a58, -15137, 6270);
    let a52 = L::sub(a73, y[0]);
    let a51 = L::rot(a57, a56, 6270, -15137);
    let a50 = L::sub(a77, y[15]);
    let a49 = L::rot(a57, a56, 15137, 6270);
    let a48 = L::add(a73, y[0]);
    let a47 = L::rot(a63, a62, -6270, -15137);
    let a46 = L::sub(a79, y[14]);
    let a45 = L::rot(a63, a62, -15137, 6270);
    let a44 = L::add(a75, y[1]);
    let a43 = L::rot(a61, a60, 6270, -15137);
    let a42 = L::add(a79, y[14]);
    let a41 = L::rot(a61, a60, 15137, 6270);
    let a40 = L::sub(a75, y[1]);
    let a39 = L::add(a45, a44);
    let a38 = L::add(a47, a46);
    let a37 = L::sub(a41, a40);
    let a36 = L::sub(a43, a42);
    let a35 = L::sub(a45, a44);
    let a34 = L::sub(a47, a46);
    let a33 = L::add(a41, a40);
    let a32 = L::add(a43, a42);
    let a31 = L::rot(a39, a38, -9102, -13623);
    let a30 = L::add(a55, a54);
    let a29 = L::rot(a39, a38, 13623, -9102);
    let a28 = L::add(a53, a52);
    let a27 = L::rot(a37, a36, -16069, -3196);
    let a26 = L::sub(a51, a50);
    let a25 = L::rot(a37, a36, 3196, -16069);
    let a24 = L::sub(a49, a48);
    let a23 = L::rot(a35, a34, 13623, -9102);
    let a22 = L::sub(a55, a54);
    let a21 = L::rot(a35, a34, 9102, 13623);
    let a20 = L::sub(a53, a52);
    let a19 = L::rot(a33, a32, 3196, -16069);
    let a18 = L::add(a51, a50);
    let a17 = L::rot(a33, a32, 16069, 3196);
    let a16 = L::add(a49, a48);
    let a15 = L::add(a29, a28);
    let a14 = L::add(a31, a30);
    let a13 = L::add(a25, a24);
    let a12 = L::add(a27, a26);
    let a11 = L::sub(a21, a20);
    let a10 = L::sub(a23, a22);
    let a9 = L::sub(a17, a16);
    let a8 = L::sub(a19, a18);
    let a7 = L::sub(a29, a28);
    let a6 = L::sub(a31, a30);
    let a5 = L::sub(a25, a24);
    let a4 = L::sub(a27, a26);
    let a3 = L::add(a21, a20);
    let a2 = L::add(a23, a22);
    let a1 = L::add(a17, a16);
    let a0 = L::add(a19, a18);
    let c0 = L::rot(a1, a0, 804, -16364);
    let c1 = L::rot(a15, a14, -2404, -16207);
    let c2 = L::rot(a3, a2, 3981, -15893);
    let c3 = L::rot(a13, a12, -5520, -15426);
    let c4 = L::rot(a5, a4, 7005, -14811);
    let c5 = L::rot(a11, a10, -8423, -14053);
    let c6 = L::rot(a7, a6, 9760, -13160);
    let c7 = L::rot(a9, a8, -11003, -12140);
    let c8 = L::rot(a9, a8, -12140, 11003);
    let c9 = L::rot(a7, a6, 13160, 9760);
    let c10 = L::rot(a11, a10, -14053, 8423);
    let c11 = L::rot(a5, a4, 14811, 7005);
    let c12 = L::rot(a13, a12, -15426, 5520);
    let c13 = L::rot(a3, a2, 15893, 3981);
    let c14 = L::rot(a15, a14, -16207, 2404);
    let c15 = L::rot(a1, a0, 16364, 804);
    [
        c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15,
    ]
}

#[inline(always)]
pub fn idct4<L: Lane>(x: [L; 4]) -> [L; 4] {
    let v0 = L::rot(x[0], x[2], 11585, -11585);
    let v1 = L::rot(x[0], x[2], 11585, 11585);
    let v2 = L::rot(x[1], x[3], 6270, -15137);
    let v3 = L::rot(x[1], x[3], 15137, 6270);
    let v4 = L::add(v1, v3);
    let v5 = L::sub(v1, v3);
    let v6 = L::add(v0, v2);
    let v7 = L::sub(v0, v2);
    [v4, v6, v7, v5]
}

#[inline(always)]
pub fn idct8<L: Lane>(x: [L; 8]) -> [L; 8] {
    let v0 = L::rot(x[0], x[4], 11585, -11585);
    let v1 = L::rot(x[0], x[4], 11585, 11585);
    let v2 = L::rot(x[2], x[6], 6270, -15137);
    let v3 = L::rot(x[2], x[6], 15137, 6270);
    let v4 = L::add(v1, v3);
    let v5 = L::sub(v1, v3);
    let v6 = L::add(v0, v2);
    let v7 = L::sub(v0, v2);
    let v8 = L::rot(x[1], x[7], 3196, -16069);
    let v9 = L::rot(x[1], x[7], 16069, 3196);
    let v10 = L::rot(x[5], x[3], 13623, -9102);
    let v11 = L::rot(x[5], x[3], 9102, 13623);
    let v12 = L::add(v8, v10);
    let v13 = L::sub(v8, v10);
    let v14 = L::add(v9, v11);
    let v15 = L::sub(v9, v11);
    let v16 = L::rot(v15, v13, 11585, -11585);
    let v17 = L::rot(v15, v13, 11585, 11585);
    let v18 = L::add(v4, v14);
    let v19 = L::sub(v4, v14);
    let v20 = L::add(v6, v17);
    let v21 = L::sub(v6, v17);
    let v22 = L::add(v7, v16);
    let v23 = L::sub(v7, v16);
    let v24 = L::add(v5, v12);
    let v25 = L::sub(v5, v12);
    [v18, v20, v22, v24, v25, v23, v21, v19]
}

#[inline(always)]
pub fn idct16<L: Lane>(x: [L; 16]) -> [L; 16] {
    let v0 = L::rot(x[0], x[8], 11585, -11585);
    let v1 = L::rot(x[0], x[8], 11585, 11585);
    let v2 = L::rot(x[4], x[12], 6270, -15137);
    let v3 = L::rot(x[4], x[12], 15137, 6270);
    let v4 = L::add(v1, v3);
    let v5 = L::sub(v1, v3);
    let v6 = L::add(v0, v2);
    let v7 = L::sub(v0, v2);
    let v8 = L::rot(x[2], x[14], 3196, -16069);
    let v9 = L::rot(x[2], x[14], 16069, 3196);
    let v10 = L::rot(x[10], x[6], 13623, -9102);
    let v11 = L::rot(x[10], x[6], 9102, 13623);
    let v12 = L::add(v8, v10);
    let v13 = L::sub(v8, v10);
    let v14 = L::add(v9, v11);
    let v15 = L::sub(v9, v11);
    let v16 = L::rot(v15, v13, 11585, -11585);
    let v17 = L::rot(v15, v13, 11585, 11585);
    let v18 = L::add(v4, v14);
    let v19 = L::sub(v4, v14);
    let v20 = L::add(v6, v17);
    let v21 = L::sub(v6, v17);
    let v22 = L::add(v7, v16);
    let v23 = L::sub(v7, v16);
    let v24 = L::add(v5, v12);
    let v25 = L::sub(v5, v12);
    let v26 = L::rot(x[1], x[15], 1606, -16305);
    let v27 = L::rot(x[1], x[15], 16305, 1606);
    let v28 = L::rot(x[9], x[7], 12665, -10394);
    let v29 = L::rot(x[9], x[7], 10394, 12665);
    let v30 = L::rot(x[5], x[11], 7723, -14449);
    let v31 = L::rot(x[5], x[11], 14449, 7723);
    let v32 = L::rot(x[13], x[3], 15679, -4756);
    let v33 = L::rot(x[13], x[3], 4756, 15679);
    let v34 = L::add(v26, v28);
    let v35 = L::sub(v26, v28);
    let v36 = L::add(v32, v30);
    let v37 = L::sub(v32, v30);
    let v38 = L::add(v33, v31);
    let v39 = L::sub(v33, v31);
    let v40 = L::add(v27, v29);
    let v41 = L::sub(v27, v29);
    let v42 = L::rot(v41, v35, 6270, -15137);
    let v43 = L::rot(v41, v35, 15137, 6270);
    let v44 = L::rot(v37, v39, -15137, 6270);
    let v45 = L::rot(v37, v39, -6270, -15137);
    let v46 = L::add(v34, v36);
    let v47 = L::sub(v34, v36);
    let v48 = L::add(v40, v38);
    let v49 = L::sub(v40, v38);
    let v50 = L::add(v42, v45);
    let v51 = L::sub(v42, v45);
    let v52 = L::add(v43, v44);
    let v53 = L::sub(v43, v44);
    let v54 = L::rot(v53, v51, 11585, -11585);
    let v55 = L::rot(v53, v51, 11585, 11585);
    let v56 = L::rot(v49, v47, 11585, -11585);
    let v57 = L::rot(v49, v47, 11585, 11585);
    let v58 = L::add(v18, v48);
    let v59 = L::sub(v18, v48);
    let v60 = L::add(v20, v52);
    let v61 = L::sub(v20, v52);
    let v62 = L::add(v22, v55);
    let v63 = L::sub(v22, v55);
    let v64 = L::add(v24, v57);
    let v65 = L::sub(v24, v57);
    let v66 = L::add(v25, v56);
    let v67 = L::sub(v25, v56);
    let v68 = L::add(v23, v54);
    let v69 = L::sub(v23, v54);
    let v70 = L::add(v21, v50);
    let v71 = L::sub(v21, v50);
    let v72 = L::add(v19, v46);
    let v73 = L::sub(v19, v46);
    [
        v58, v60, v62, v64, v66, v68, v70, v72, v73, v71, v69, v67, v65, v63, v61, v59,
    ]
}

#[inline(always)]
pub fn idct32<L: Lane>(x: [L; 32]) -> [L; 32] {
    let v0 = L::rot(x[0], x[16], 11585, -11585);
    let v1 = L::rot(x[0], x[16], 11585, 11585);
    let v2 = L::rot(x[8], x[24], 6270, -15137);
    let v3 = L::rot(x[8], x[24], 15137, 6270);
    let v4 = L::add(v1, v3);
    let v5 = L::sub(v1, v3);
    let v6 = L::add(v0, v2);
    let v7 = L::sub(v0, v2);
    let v8 = L::rot(x[4], x[28], 3196, -16069);
    let v9 = L::rot(x[4], x[28], 16069, 3196);
    let v10 = L::rot(x[20], x[12], 13623, -9102);
    let v11 = L::rot(x[20], x[12], 9102, 13623);
    let v12 = L::add(v8, v10);
    let v13 = L::sub(v8, v10);
    let v14 = L::add(v9, v11);
    let v15 = L::sub(v9, v11);
    let v16 = L::rot(v15, v13, 11585, -11585);
    let v17 = L::rot(v15, v13, 11585, 11585);
    let v18 = L::add(v4, v14);
    let v19 = L::sub(v4, v14);
    let v20 = L::add(v6, v17);
    let v21 = L::sub(v6, v17);
    let v22 = L::add(v7, v16);
    let v23 = L::sub(v7, v16);
    let v24 = L::add(v5, v12);
    let v25 = L::sub(v5, v12);
    let v26 = L::rot(x[2], x[30], 1606, -16305);
    let v27 = L::rot(x[2], x[30], 16305, 1606);
    let v28 = L::rot(x[18], x[14], 12665, -10394);
    let v29 = L::rot(x[18], x[14], 10394, 12665);
    let v30 = L::rot(x[10], x[22], 7723, -14449);
    let v31 = L::rot(x[10], x[22], 14449, 7723);
    let v32 = L::rot(x[26], x[6], 15679, -4756);
    let v33 = L::rot(x[26], x[6], 4756, 15679);
    let v34 = L::add(v26, v28);
    let v35 = L::sub(v26, v28);
    let v36 = L::add(v32, v30);
    let v37 = L::sub(v32, v30);
    let v38 = L::add(v33, v31);
    let v39 = L::sub(v33, v31);
    let v40 = L::add(v27, v29);
    let v41 = L::sub(v27, v29);
    let v42 = L::rot(v41, v35, 6270, -15137);
    let v43 = L::rot(v41, v35, 15137, 6270);
    let v44 = L::rot(v37, v39, -15137, 6270);
    let v45 = L::rot(v37, v39, -6270, -15137);
    let v46 = L::add(v34, v36);
    let v47 = L::sub(v34, v36);
    let v48 = L::add(v40, v38);
    let v49 = L::sub(v40, v38);
    let v50 = L::add(v42, v45);
    let v51 = L::sub(v42, v45);
    let v52 = L::add(v43, v44);
    let v53 = L::sub(v43, v44);
    let v54 = L::rot(v53, v51, 11585, -11585);
    let v55 = L::rot(v53, v51, 11585, 11585);
    let v56 = L::rot(v49, v47, 11585, -11585);
    let v57 = L::rot(v49, v47, 11585, 11585);
    let v58 = L::add(v18, v48);
    let v59 = L::sub(v18, v48);
    let v60 = L::add(v20, v52);
    let v61 = L::sub(v20, v52);
    let v62 = L::add(v22, v55);
    let v63 = L::sub(v22, v55);
    let v64 = L::add(v24, v57);
    let v65 = L::sub(v24, v57);
    let v66 = L::add(v25, v56);
    let v67 = L::sub(v25, v56);
    let v68 = L::add(v23, v54);
    let v69 = L::sub(v23, v54);
    let v70 = L::add(v21, v50);
    let v71 = L::sub(v21, v50);
    let v72 = L::add(v19, v46);
    let v73 = L::sub(v19, v46);
    let v74 = L::rot(x[1], x[31], 804, -16364);
    let v75 = L::rot(x[1], x[31], 16364, 804);
    let v76 = L::rot(x[17], x[15], 12140, -11003);
    let v77 = L::rot(x[17], x[15], 11003, 12140);
    let v78 = L::rot(x[9], x[23], 7005, -14811);
    let v79 = L::rot(x[9], x[23], 14811, 7005);
    let v80 = L::rot(x[25], x[7], 15426, -5520);
    let v81 = L::rot(x[25], x[7], 5520, 15426);
    let v82 = L::rot(x[5], x[27], 3981, -15893);
    let v83 = L::rot(x[5], x[27], 15893, 3981);
    let v84 = L::rot(x[21], x[11], 14053, -8423);
    let v85 = L::rot(x[21], x[11], 8423, 14053);
    let v86 = L::rot(x[13], x[19], 9760, -13160);
    let v87 = L::rot(x[13], x[19], 13160, 9760);
    let v88 = L::rot(x[29], x[3], 16207, -2404);
    let v89 = L::rot(x[29], x[3], 2404, 16207);
    let v90 = L::add(v74, v76);
    let v91 = L::sub(v74, v76);
    let v92 = L::add(v80, v78);
    let v93 = L::sub(v80, v78);
    let v94 = L::add(v82, v84);
    let v95 = L::sub(v82, v84);
    let v96 = L::add(v88, v86);
    let v97 = L::sub(v88, v86);
    let v98 = L::add(v89, v87);
    let v99 = L::sub(v89, v87);
    let v100 = L::add(v83, v85);
    let v101 = L::sub(v83, v85);
    let v102 = L::add(v81, v79);
    let v103 = L::sub(v81, v79);
    let v104 = L::add(v75, v77);
    let v105 = L::sub(v75, v77);
    let v106 = L::rot(v105, v91, 3196, -16069);
    let v107 = L::rot(v105, v91, 16069, 3196);
    let v108 = L::rot(v97, v99, -9102, 13623);
    let v109 = L::rot(v97, v99, -13623, -9102);
    let v110 = L::rot(v101, v95, 13623, -9102);
    let v111 = L::rot(v101, v95, 9102, 13623);
    let v112 = L::rot(v93, v103, -16069, 3196);
    let v113 = L::rot(v93, v103, -3196, -16069);
    let v114 = L::add(v90, v92);
    let v115 = L::sub(v90, v92);
    let v116 = L::add(v96, v94);
    let v117 = L::sub(v96, v94);
    let v118 = L::add(v98, v100);
    let v119 = L::sub(v98, v100);
    let v120 = L::add(v104, v102);
    let v121 = L::sub(v104, v102);
    let v122 = L::add(v106, v113);
    let v123 = L::sub(v106, v113);
    let v124 = L::add(v109, v110);
    let v125 = L::sub(v109, v110);
    let v126 = L::add(v108, v111);
    let v127 = L::sub(v108, v111);
    let v128 = L::add(v107, v112);
    let v129 = L::sub(v107, v112);
    let v130 = L::rot(v129, v123, 6270, -15137);
    let v131 = L::rot(v129, v123, 15137, 6270);
    let v132 = L::rot(v125, v127, -15137, 6270);
    let v133 = L::rot(v125, v127, -6270, -15137);
    let v134 = L::rot(v121, v115, 6270, -15137);
    let v135 = L::rot(v121, v115, 15137, 6270);
    let v136 = L::rot(v117, v119, -15137, 6270);
    let v137 = L::rot(v117, v119, -6270, -15137);
    let v138 = L::add(v114, v116);
    let v139 = L::sub(v114, v116);
    let v140 = L::add(v120, v118);
    let v141 = L::sub(v120, v118);
    let v142 = L::add(v122, v124);
    let v143 = L::sub(v122, v124);
    let v144 = L::add(v128, v126);
    let v145 = L::sub(v128, v126);
    let v146 = L::add(v130, v133);
    let v147 = L::sub(v130, v133);
    let v148 = L::add(v131, v132);
    let v149 = L::sub(v131, v132);
    let v150 = L::add(v134, v137);
    let v151 = L::sub(v134, v137);
    let v152 = L::add(v135, v136);
    let v153 = L::sub(v135, v136);
    let v154 = L::rot(v153, v151, 11585, -11585);
    let v155 = L::rot(v153, v151, 11585, 11585);
    let v156 = L::rot(v149, v147, 11585, -11585);
    let v157 = L::rot(v149, v147, 11585, 11585);
    let v158 = L::rot(v145, v143, 11585, -11585);
    let v159 = L::rot(v145, v143, 11585, 11585);
    let v160 = L::rot(v141, v139, 11585, -11585);
    let v161 = L::rot(v141, v139, 11585, 11585);
    let v162 = L::add(v58, v140);
    let v163 = L::sub(v58, v140);
    let v164 = L::add(v60, v144);
    let v165 = L::sub(v60, v144);
    let v166 = L::add(v62, v148);
    let v167 = L::sub(v62, v148);
    let v168 = L::add(v64, v152);
    let v169 = L::sub(v64, v152);
    let v170 = L::add(v66, v155);
    let v171 = L::sub(v66, v155);
    let v172 = L::add(v68, v157);
    let v173 = L::sub(v68, v157);
    let v174 = L::add(v70, v159);
    let v175 = L::sub(v70, v159);
    let v176 = L::add(v72, v161);
    let v177 = L::sub(v72, v161);
    let v178 = L::add(v73, v160);
    let v179 = L::sub(v73, v160);
    let v180 = L::add(v71, v158);
    let v181 = L::sub(v71, v158);
    let v182 = L::add(v69, v156);
    let v183 = L::sub(v69, v156);
    let v184 = L::add(v67, v154);
    let v185 = L::sub(v67, v154);
    let v186 = L::add(v65, v150);
    let v187 = L::sub(v65, v150);
    let v188 = L::add(v63, v146);
    let v189 = L::sub(v63, v146);
    let v190 = L::add(v61, v142);
    let v191 = L::sub(v61, v142);
    let v192 = L::add(v59, v138);
    let v193 = L::sub(v59, v138);
    [
        v162, v164, v166, v168, v170, v172, v174, v176, v178, v180, v182, v184, v186, v188, v190,
        v192, v193, v191, v189, v187, v185, v183, v181, v179, v177, v175, v173, v171, v169, v167,
        v165, v163,
    ]
}

#[inline(always)]
pub fn iadst8<L: Lane>(x: [L; 8]) -> [L; 8] {
    let v0 = L::prod(x[7], x[0], 1606, -16305);
    let v1 = L::prod(x[7], x[0], 16305, 1606);
    let v2 = L::prod(x[5], x[2], 7723, -14449);
    let v3 = L::prod(x[5], x[2], 14449, 7723);
    let v4 = L::prod(x[3], x[4], 12665, -10394);
    let v5 = L::prod(x[3], x[4], 10394, 12665);
    let v6 = L::prod(x[1], x[6], 15679, -4756);
    let v7 = L::prod(x[1], x[6], 4756, 15679);
    let v8 = L::rsum(v1, v5);
    let v9 = L::rdiff(v1, v5);
    let v10 = L::rsum(v0, v4);
    let v11 = L::rdiff(v0, v4);
    let v12 = L::rsum(v3, v7);
    let v13 = L::rdiff(v3, v7);
    let v14 = L::rsum(v2, v6);
    let v15 = L::rdiff(v2, v6);
    let v16 = L::prod(v9, v11, 6270, -15137);
    let v17 = L::prod(v9, v11, 15137, 6270);
    let v18 = L::prod(v15, v13, 15137, -6270);
    let v19 = L::prod(v15, v13, 6270, 15137);
    let v20 = L::rsum(v17, v18);
    let v21 = L::rdiff(v17, v18);
    let v22 = L::rsum(v16, v19);
    let v23 = L::rdiff(v16, v19);
    let v24 = L::add(v8, v12);
    let v25 = L::sub(v8, v12);
    let v26 = L::add(v10, v14);
    let v27 = L::sub(v10, v14);
    let v28 = L::rot(v25, v27, 11585, -11585);
    let v29 = L::rot(v25, v27, 11585, 11585);
    let v30 = L::rot(v21, v23, 11585, -11585);
    let v31 = L::rot(v21, v23, 11585, 11585);
    let v32 = L::neg(v20);
    let v33 = L::neg(v29);
    let v34 = L::neg(v30);
    let v35 = L::neg(v26);
    [v24, v32, v31, v33, v28, v34, v22, v35]
}

#[inline(always)]
pub fn iadst16<L: Lane>(x: [L; 16]) -> [L; 16] {
    let v0 = L::prod(x[15], x[0], 804, -16364);
    let v1 = L::prod(x[15], x[0], 16364, 804);
    let v2 = L::prod(x[13], x[2], 3981, -15893);
    let v3 = L::prod(x[13], x[2], 15893, 3981);
    let v4 = L::prod(x[11], x[4], 7005, -14811);
    let v5 = L::prod(x[11], x[4], 14811, 7005);
    let v6 = L::prod(x[9], x[6], 9760, -13160);
    let v7 = L::prod(x[9], x[6], 13160, 9760);
    let v8 = L::prod(x[7], x[8], 12140, -11003);
    let v9 = L::prod(x[7], x[8], 11003, 12140);
    let v10 = L::prod(x[5], x[10], 14053, -8423);
    let v11 = L::prod(x[5], x[10], 8423, 14053);
    let v12 = L::prod(x[3], x[12], 15426, -5520);
    let v13 = L::prod(x[3], x[12], 5520, 15426);
    let v14 = L::prod(x[1], x[14], 16207, -2404);
    let v15 = L::prod(x[1], x[14], 2404, 16207);
    let v16 = L::rsum(v1, v9);
    let v17 = L::rdiff(v1, v9);
    let v18 = L::rsum(v0, v8);
    let v19 = L::rdiff(v0, v8);
    let v20 = L::rsum(v3, v11);
    let v21 = L::rdiff(v3, v11);
    let v22 = L::rsum(v2, v10);
    let v23 = L::rdiff(v2, v10);
    let v24 = L::rsum(v5, v13);
    let v25 = L::rdiff(v5, v13);
    let v26 = L::rsum(v4, v12);
    let v27 = L::rdiff(v4, v12);
    let v28 = L::rsum(v7, v15);
    let v29 = L::rdiff(v7, v15);
    let v30 = L::rsum(v6, v14);
    let v31 = L::rdiff(v6, v14);
    let v32 = L::prod(v17, v19, 3196, -16069);
    let v33 = L::prod(v17, v19, 16069, 3196);
    let v34 = L::prod(v21, v23, 13623, -9102);
    let v35 = L::prod(v21, v23, 9102, 13623);
    let v36 = L::prod(v25, v27, 16069, 3196);
    let v37 = L::prod(v25, v27, -3196, 16069);
    let v38 = L::prod(v29, v31, 9102, 13623);
    let v39 = L::prod(v29, v31, -13623, 9102);
    let v40 = L::rsum(v33, v37);
    let v41 = L::rdiff(v33, v37);
    let v42 = L::rsum(v32, v36);
    let v43 = L::rdiff(v32, v36);
    let v44 = L::rsum(v35, v39);
    let v45 = L::rdiff(v35, v39);
    let v46 = L::rsum(v34, v38);
    let v47 = L::rdiff(v34, v38);
    let v48 = L::add(v16, v24);
    let v49 = L::sub(v16, v24);
    let v50 = L::add(v18, v26);
    let v51 = L::sub(v18, v26);
    let v52 = L::add(v20, v28);
    let v53 = L::sub(v20, v28);
    let v54 = L::add(v22, v30);
    let v55 = L::sub(v22, v30);
    let v56 = L::prod(v49, v51, 6270, -15137);
    let v57 = L::prod(v49, v51, 15137, 6270);
    let v58 = L::prod(v55, v53, 15137, -6270);
    let v59 = L::prod(v55, v53, 6270, 15137);
    let v60 = L::prod(v41, v43, 6270, -15137);
    let v61 = L::prod(v41, v43, 15137, 6270);
    let v62 = L::prod(v47, v45, 15137, -6270);
    let v63 = L::prod(v47, v45, 6270, 15137);
    let v64 = L::rsum(v57, v58);
    let v65 = L::rdiff(v57, v58);
    let v66 = L::rsum(v61, v62);
    let v67 = L::rdiff(v61, v62);
    let v68 = L::rsum(v56, v59);
    let v69 = L::rdiff(v56, v59);
    let v70 = L::rsum(v60, v63);
    let v71 = L::rdiff(v60, v63);
    let v72 = L::add(v48, v52);
    let v73 = L::sub(v48, v52);
    let v74 = L::add(v40, v44);
    let v75 = L::sub(v40, v44);
    let v76 = L::add(v50, v54);
    let v77 = L::sub(v50, v54);
    let v78 = L::add(v42, v46);
    let v79 = L::sub(v42, v46);
    let v80 = L::rot(v73, v77, -11585, -11585);
    let v81 = L::rot(v73, v77, 11585, -11585);
    let v82 = L::rot(v65, v69, 11585, 11585);
    let v83 = L::rot(v65, v69, -11585, 11585);
    let v84 = L::rot(v75, v79, 11585, 11585);
    let v85 = L::rot(v75, v79, -11585, 11585);
    let v86 = L::rot(v67, v71, -11585, -11585);
    let v87 = L::rot(v67, v71, 11585, -11585);
    let v88 = L::neg(v74);
    let v89 = L::neg(v70);
    let v90 = L::neg(v64);
    let v91 = L::neg(v76);
    [
        v72, v88, v66, v90, v82, v86, v84, v80, v81, v85, v87, v83, v68, v89, v78, v91,
    ]
}
