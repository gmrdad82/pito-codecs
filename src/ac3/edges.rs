use super::tests::{LIMIT, against_a52dec, every_frame, ours, pinned_plain, refs};
use super::{info, layout};
use crate::testref::Refs;

#[test]
fn every_layout_and_rate_matches_a52dec() {
    let r = Refs::open(refs::AREA);
    let mut failed = Vec::new();
    for c in &refs::EDGES {
        let data = r.bytes(&c.stream());
        let i = info(&data).unwrap();
        assert_eq!(
            (i.rate, i.channels, i.eac3),
            (c.rate, c.channels(), false),
            "{}",
            c.name
        );
        let (plain, _) = pinned_plain(&r, c);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, layout(c.channels()), "{}", c.name);
        assert_eq!(got.len(), every_frame(&plain, ch), "{}: length", c.name);
        let n = ch as usize;
        let silent = (0..n)
            .filter(|&k| got.iter().skip(k).step_by(n).all(|&v| v == 0.0))
            .count();
        let spread = usize::from(c.spread());
        assert!(
            silent >= n - c.channels() as usize - spread,
            "{}: {silent} silent slots",
            c.name
        );
        let d = against_a52dec(&r, c, &got, ch);
        if !d.within(LIMIT, LIMIT) {
            failed.push(format!(
                "{}: {} samples against {}, max difference {}",
                c.name, d.ours, d.theirs, d.max
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
