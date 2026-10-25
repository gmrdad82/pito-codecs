use super::tests::{LIMIT, against_ffmpeg, ours, pinned_plain, refs};
use super::{info, layout};
use crate::testref::Refs;

#[test]
fn every_layout_and_rate_matches_ffmpeg() {
    let r = Refs::open(refs::AREA);
    let mut failed = Vec::new();
    for c in &refs::EDGES {
        let data = r.bytes(&c.stream());
        let i = info(&data).unwrap();
        assert_eq!(
            (i.rate, i.channels, i.eac3),
            (c.rate, c.channels, c.codec == "eac3"),
            "{}",
            c.name
        );
        let (plain, _) = pinned_plain(&r, c);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, layout(c.channels), "{}", c.name);
        let n = ch as usize;
        let silent = (0..n)
            .filter(|&k| got.iter().skip(k).step_by(n).all(|&v| v == 0.0))
            .count();
        let spread = matches!(c.layout, "3.0(back)" | "4.0" | "4.1") as usize;
        assert!(
            silent >= n - c.channels as usize - spread,
            "{}: {silent} silent slots",
            c.name
        );
        let d = against_ffmpeg(&r, c, &got, ch);
        if !d.within(LIMIT, LIMIT) {
            failed.push(format!(
                "{}: {} samples against {}, max difference {}",
                c.name, d.ours, d.theirs, d.max
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn every_edge_target_has_the_slots_our_decoder_fills() {
    for c in &refs::EDGES {
        let want = match c.target() {
            "mono" => 1,
            "stereo" => 2,
            _ => 6,
        };
        assert_eq!(layout(c.channels), want, "{}", c.name);
    }
}
