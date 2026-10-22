use super::ebml;
use super::ids::*;
use super::parse;
use super::*;
use crate::media::Error;

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    ebml::write_id(&mut o, id);
    ebml::write_size(&mut o, body.len() as u64, 1);
    o.extend_from_slice(body);
    o
}

fn unknown(id: u32, body: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    ebml::write_id(&mut o, id);
    ebml::write_unknown_size(&mut o, 8);
    o.extend_from_slice(body);
    o
}

fn u(id: u32, v: u64) -> Vec<u8> {
    let b = v.to_be_bytes();
    let skip = (v.leading_zeros() / 8).min(7) as usize;
    el(id, &b[skip..])
}

fn f32e(id: u32, v: f32) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

fn f64e(id: u32, v: f64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn block(track: u8, rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(payload);
    b
}

fn fill(n: usize, v: u8) -> Vec<u8> {
    vec![v; n]
}

fn header() -> Vec<u8> {
    el(EBML, &cat(&[u(EBML_VERSION, 1), el(DOC_TYPE, b"matroska")]))
}

fn tracks() -> Vec<u8> {
    let colour = cat(&[
        u(MATRIX_COEFFICIENTS, 9),
        u(BITS_PER_CHANNEL, 10),
        u(CHROMA_SITING_HORZ, 1),
        u(CHROMA_SITING_VERT, 2),
        u(RANGE, 2),
        u(TRANSFER_CHARACTERISTICS, 16),
        u(PRIMARIES, 9),
        u(MAX_CLL, 1000),
        u(MAX_FALL, 400),
        el(
            MASTERING_METADATA,
            &cat(&[
                f32e(PRIMARY_R_X, 0.5),
                f64e(PRIMARY_R_Y, 0.25),
                f32e(PRIMARY_G_X, 0.125),
                f32e(PRIMARY_G_Y, 0.75),
                f32e(PRIMARY_B_X, 0.0625),
                f32e(PRIMARY_B_Y, 0.03125),
                f64e(WHITE_POINT_X, 0.3125),
                f64e(WHITE_POINT_Y, 0.328125),
                f64e(LUMINANCE_MAX, 1000.0),
                f64e(LUMINANCE_MIN, 0.0),
                el(LUMINANCE_MIN, &[1, 2, 3]),
            ]),
        ),
    ]);
    let video = cat(&[
        u(PIXEL_WIDTH, 64),
        u(PIXEL_HEIGHT, 48),
        u(DISPLAY_WIDTH, 128),
        u(DISPLAY_HEIGHT, 48),
        u(PIXEL_CROP_TOP, 0),
        u(ALPHA_MODE, 1),
        el(COLOUR, &colour),
        el(
            PROJECTION,
            &cat(&[u(PROJECTION_TYPE, 0), f64e(PROJECTION_POSE_ROLL, -90.0)]),
        ),
    ]);
    let v = el(
        TRACK_ENTRY,
        &cat(&[
            u(TRACK_NUMBER, 1),
            u(TRACK_UID, 11),
            u(TRACK_TYPE, 1),
            el(CODEC_ID, b"V_VP9"),
            el(NAME, b"picture"),
            el(VIDEO, &video),
        ]),
    );
    let a = el(
        TRACK_ENTRY,
        &cat(&[
            u(TRACK_NUMBER, 2),
            u(TRACK_TYPE, 2),
            el(CODEC_ID, b"A_OPUS"),
            u(DEFAULT_DURATION, 20_000_000),
            u(CODEC_DELAY, 6_500_000),
            u(SEEK_PRE_ROLL, 80_000_000),
            el(LANGUAGE, b"spa"),
            u(FLAG_DEFAULT, 0),
            el(
                AUDIO,
                &cat(&[
                    f32e(SAMPLING_FREQUENCY, 48_000.0),
                    f64e(OUTPUT_SAMPLING_FREQUENCY, 96_000.0),
                    u(CHANNELS, 2),
                    u(BIT_DEPTH, 16),
                ]),
            ),
        ]),
    );
    let strip = el(
        CONTENT_ENCODINGS,
        &el(
            CONTENT_ENCODING,
            &cat(&[
                u(CONTENT_ENCODING_SCOPE, 1),
                el(
                    CONTENT_COMPRESSION,
                    &cat(&[u(CONTENT_COMP_ALGO, 3), el(CONTENT_COMP_SETTINGS, b"AB")]),
                ),
            ]),
        ),
    );
    let s = el(
        TRACK_ENTRY,
        &cat(&[
            u(TRACK_NUMBER, 3),
            u(TRACK_TYPE, 0x11),
            el(CODEC_ID, b"S_TEXT/UTF8"),
            strip,
        ]),
    );
    let zlib = el(
        CONTENT_ENCODINGS,
        &el(
            CONTENT_ENCODING,
            &el(CONTENT_COMPRESSION, &u(CONTENT_COMP_ALGO, 0)),
        ),
    );
    let z = el(
        TRACK_ENTRY,
        &cat(&[
            u(TRACK_NUMBER, 4),
            u(TRACK_TYPE, 2),
            el(CODEC_ID, b"A_VORBIS"),
            zlib,
        ]),
    );
    let nameless = el(TRACK_ENTRY, &cat(&[u(TRACK_TYPE, 1)]));
    el(TRACKS, &cat(&[v, a, s, z, nameless]))
}

fn first_cluster() -> Vec<u8> {
    let mut xiph = vec![2, 255, 45, 2];
    xiph.extend(fill(300, 1));
    xiph.extend(fill(2, 2));
    xiph.extend(fill(5, 3));
    let mut ebml_lace = vec![2, 0x8A, 0xBC];
    ebml_lace.extend(fill(10, 4));
    ebml_lace.extend(fill(7, 5));
    ebml_lace.extend(fill(4, 6));
    let mut fixed = vec![1];
    fixed.extend(fill(6, 7));
    fixed.extend(fill(6, 8));
    let alpha = el(
        BLOCK_ADDITIONS,
        &el(
            BLOCK_MORE,
            &cat(&[u(BLOCK_ADD_ID, 1), el(BLOCK_ADDITIONAL, b"alpha")]),
        ),
    );
    let group = el(
        BLOCK_GROUP,
        &cat(&[
            el(BLOCK, &block(1, 40, 0, b"delta")),
            u(BLOCK_DURATION, 33),
            el(REFERENCE_BLOCK, &[0xD8]),
            el(DISCARD_PADDING, &[0x01, 0x00]),
            alpha,
        ]),
    );
    let subtitle = el(
        BLOCK_GROUP,
        &cat(&[el(BLOCK, &block(3, 50, 0, b"hi")), u(BLOCK_DURATION, 500)]),
    );
    unknown(
        CLUSTER,
        &cat(&[
            u(TIMESTAMP, 100),
            el(SIMPLE_BLOCK, &block(1, 0, 0x80, b"key")),
            el(SIMPLE_BLOCK, &block(2, 0, 0x80 | 0x02, &xiph)),
            el(SIMPLE_BLOCK, &block(9, 10, 0x80, b"stray")),
            group,
            el(SIMPLE_BLOCK, &block(2, 60, 0x80 | 0x06, &ebml_lace)),
            el(SIMPLE_BLOCK, &block(2, 120, 0x80 | 0x04, &fixed)),
            subtitle,
            el(SIMPLE_BLOCK, &block(4, 0, 0x80, b"zz")),
        ]),
    )
}

fn file() -> Vec<u8> {
    let info = el(
        INFO,
        &cat(&[
            u(TIMESTAMP_SCALE, 1_000_000),
            f64e(DURATION, 400.0),
            el(TITLE, b"edge\0junk"),
            el(MUXING_APP, b"mux"),
            el(WRITING_APP, b"write"),
        ]),
    );
    let second = el(
        CLUSTER,
        &cat(&[
            u(TIMESTAMP, 300),
            el(SIMPLE_BLOCK, &block(1, 0, 0x80, b"late")),
        ]),
    );
    let body = cat(&[info, tracks(), first_cluster(), second]);
    cat(&[header(), unknown(SEGMENT, &body)])
}

#[test]
fn hand_built_file_reads_every_lacing_and_block_group() {
    let d = file();
    let r = Reader::new(d.as_slice()).unwrap();
    assert_eq!(r.doc_type, "matroska");
    assert_eq!(r.info.title.as_deref(), Some("edge"));
    assert_eq!(
        (r.info.muxing_app.as_str(), r.info.writing_app.as_str()),
        ("mux", "write")
    );
    assert!((r.duration_seconds() - 0.4).abs() < 1e-9);
    assert!(r.cues.is_empty());
    assert_eq!(r.clusters.len(), 2);
    assert_eq!(r.clusters[0].timestamp, 100);
    assert_eq!(
        r.tracks.iter().map(|t| t.number).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );

    let v = &r.tracks[0];
    let got: Vec<(i64, u32, bool)> = v
        .samples
        .iter()
        .map(|s| (s.pts, s.duration, s.keyframe))
        .collect();
    assert_eq!(got, [(100, 40, true), (140, 33, false), (300, 33, true)]);
    let discard: Vec<i64> = v.samples.iter().map(|s| s.discard).collect();
    assert_eq!(discard, [0, 256, 0]);
    assert_eq!(r.read(0, 0).unwrap(), b"key");
    assert_eq!(r.read(0, 1).unwrap(), b"delta");
    assert_eq!(r.read(0, 2).unwrap(), b"late");
    assert_eq!(v.rotation(), 90);
    assert!(v.alpha());
    assert_eq!(v.video.as_ref().unwrap().pixel_aspect(), (2, 1));

    let a = &r.tracks[1];
    let pts: Vec<i64> = a.samples.iter().map(|s| s.pts).collect();
    assert_eq!(pts, [93, 113, 133, 153, 173, 193, 213, 233]);
    assert!(a.samples.iter().all(|s| s.duration == 20));
    let sizes: Vec<u32> = a.samples.iter().map(|s| s.size).collect();
    assert_eq!(sizes, [300, 2, 5, 10, 7, 4, 6, 6]);
    for (i, want) in [1u8, 2, 3, 4, 5, 6, 7, 8].into_iter().enumerate() {
        let data = r.read(1, i).unwrap();
        assert!(data.iter().all(|&b| b == want), "frame {i}");
    }
    let lace: Vec<u16> = a.samples.iter().map(|s| s.lace).collect();
    assert_eq!(lace, [0, 1, 2, 0, 1, 2, 0, 1]);
    assert!(a.samples.iter().all(|s| s.discard == 0));
    assert_eq!((a.channels(), a.sample_rate()), (2, 48_000));
    assert_eq!(a.language, "spa");
    assert!(!a.default);

    let s = &r.tracks[2];
    assert_eq!(s.kind, TrackKind::Subtitle);
    assert_eq!((s.samples[0].pts, s.samples[0].duration), (150, 500));
    assert_eq!(s.stripped_header(), Some(&b"AB"[..]));
    assert_eq!(r.read(2, 0).unwrap(), b"ABhi");

    assert!(!r.tracks[3].frames_readable());
    assert!(matches!(r.read(3, 0), Err(Error::Unsupported(_))));
}

#[test]
fn block_additional_alpha_reads_only_from_its_block_group() {
    let d = file();
    let r = Reader::new(d.as_slice()).unwrap();
    let mut alpha = Vec::new();
    assert!(r.read_alpha(0, 1, &mut alpha).unwrap());
    assert_eq!(alpha, b"alpha");
    assert!(!r.read_alpha(0, 0, &mut alpha).unwrap());
    assert!(alpha.is_empty());
    assert!(!r.read_alpha(1, 1, &mut alpha).unwrap());
    assert!(r.read_alpha(0, 99, &mut alpha).is_err());
}

#[test]
fn colour_mastering_and_projection_parse_from_the_track() {
    let d = file();
    let r = Reader::new(d.as_slice()).unwrap();
    let v = r.tracks[0].video.as_ref().unwrap();
    let c = v.colour.as_ref().unwrap();
    assert_eq!(
        (
            c.matrix,
            c.bits_per_channel,
            c.range,
            c.transfer,
            c.primaries
        ),
        (Some(9), Some(10), Some(2), Some(16), Some(9))
    );
    assert_eq!(c.chroma_siting, (Some(1), Some(2)));
    assert_eq!((c.max_cll, c.max_fall), (Some(1000), Some(400)));
    assert!(c.full_range());
    let m = c.mastering.as_ref().unwrap();
    assert_eq!(m.primaries, [(0.5, 0.25), (0.125, 0.75), (0.0625, 0.03125)]);
    assert_eq!(m.white_point, (0.3125, 0.328125));
    assert_eq!((m.luminance_max, m.luminance_min), (1000.0, 0.0));
    assert_eq!((v.display_width, v.display_height), (Some(128), Some(48)));
    let p = v.projection.as_ref().unwrap();
    assert_eq!((p.kind, p.roll), (0, -90.0));
    let a = r.tracks[1].audio.as_ref().unwrap();
    assert_eq!(a.bit_depth, Some(16));
    assert_eq!(a.output_sampling_frequency, Some(96_000.0));
    assert_eq!(
        (r.tracks[1].codec_delay, r.tracks[1].seek_pre_roll),
        (6_500_000, 80_000_000)
    );
}

#[test]
fn a_stream_without_cues_still_walks_and_seeks() {
    let d = file();
    let r = Reader::new(d.as_slice()).unwrap();
    assert!(r.cue_keys().is_none());
    let mut s = r.stream();
    let mut walked = Vec::new();
    while let Some(b) = s.next_block(None).unwrap() {
        walked.push((b.track, b.pts, b.discard));
    }
    let mut indexed: Vec<(usize, i64, i64)> = r
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, t)| t.samples.iter().map(move |s| (ti, s.pts, s.discard)))
        .collect();
    walked.sort();
    indexed.sort();
    assert_eq!(walked, indexed);
    assert!(walked.contains(&(0, 140, 256)));
    assert_eq!(s.seek(0, 0.25).unwrap(), 0.1);
    let (track, p) = s.next_packet().unwrap().unwrap();
    assert_eq!(
        (track, p.pts, p.keyframe, p.data.as_slice()),
        (0, 100, true, &b"key"[..])
    );
}

#[test]
fn info_keeps_defaults_for_out_of_range_values() {
    let i = parse::info(&cat(&[
        u(TIMESTAMP_SCALE, 0),
        el(DURATION, &f64::NAN.to_be_bytes()),
    ]));
    assert_eq!((i.timestamp_scale, i.duration), (1_000_000, None));
    let i = parse::info(&cat(&[
        u(TIMESTAMP_SCALE, u32::MAX as u64 + 1),
        el(DURATION, &[1, 2, 3]),
    ]));
    assert_eq!((i.timestamp_scale, i.duration), (1_000_000, None));
    let i = parse::info(&cat(&[u(TIMESTAMP_SCALE, 1000), f64e(DURATION, -1.0)]));
    assert_eq!((i.timestamp_scale, i.duration), (1000, None));
    let i = parse::info(&el(DURATION, &[]));
    assert_eq!(i.duration, Some(0.0));
}

#[test]
fn time_base_reduces_or_falls_back_to_milliseconds() {
    use crate::media::TimeBase;
    assert_eq!(parse::time_base(1_000_000), TimeBase::new(1, 1000));
    assert_eq!(parse::time_base(1), TimeBase::new(1, 1_000_000_000));
    assert_eq!(parse::time_base(3), TimeBase::new(3, 1_000_000_000));
    assert_eq!(parse::time_base(1 << 40), TimeBase::new(1 << 31, 1_953_125));
    assert_eq!(parse::time_base(u64::MAX), TimeBase::new(1, 1000));
}

#[test]
fn cues_drop_points_without_a_time_track_or_cluster() {
    let pos = |track: u64, cluster: Option<u64>| {
        let mut b = u(CUE_TRACK, track);
        if let Some(c) = cluster {
            b.extend(u(CUE_CLUSTER_POSITION, c));
        }
        el(CUE_TRACK_POSITIONS, &b)
    };
    let d = cat(&[
        el(
            CUE_POINT,
            &cat(&[u(CUE_TIME, 5), pos(1, Some(10)), pos(2, Some(20))]),
        ),
        el(CUE_POINT, &cat(&[pos(1, Some(30))])),
        el(
            CUE_POINT,
            &cat(&[u(CUE_TIME, 7), pos(0, Some(40)), pos(1, None)]),
        ),
        el(VOID, &[0]),
    ]);
    let mut out = Vec::new();
    parse::cues(&d, &mut out);
    assert_eq!(
        out,
        [
            CuePoint {
                time: 5,
                track: 1,
                cluster: 10
            },
            CuePoint {
                time: 5,
                track: 2,
                cluster: 20
            },
        ]
    );
}

#[test]
fn seek_head_needs_both_id_and_position() {
    let seek = |id: Option<u32>, pos: Option<u64>| {
        let mut b = Vec::new();
        if let Some(i) = id {
            b.extend(el(SEEK_ID, &i.to_be_bytes()));
        }
        if let Some(p) = pos {
            b.extend(u(SEEK_POSITION, p));
        }
        el(SEEK, &b)
    };
    let d = cat(&[
        seek(Some(CUES), Some(99)),
        seek(Some(INFO), None),
        seek(None, Some(3)),
    ]);
    let out = parse::seek_head(&d);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].id, out[0].position), (CUES, 99));
}

#[test]
fn encodings_sort_by_order_and_strip_into_codec_private() {
    let enc = |order: u64, scope: u64, kind: u64, comp: Option<(u64, &[u8])>| {
        let mut b = cat(&[
            u(CONTENT_ENCODING_ORDER, order),
            u(CONTENT_ENCODING_SCOPE, scope),
            u(CONTENT_ENCODING_TYPE, kind),
        ]);
        if let Some((algo, settings)) = comp {
            b.extend(el(
                CONTENT_COMPRESSION,
                &cat(&[
                    u(CONTENT_COMP_ALGO, algo),
                    el(CONTENT_COMP_SETTINGS, settings),
                ]),
            ));
        }
        el(CONTENT_ENCODING, &b)
    };
    let te = el(
        TRACK_ENTRY,
        &cat(&[
            u(TRACK_NUMBER, 5),
            u(TRACK_TYPE, 2),
            el(CODEC_PRIVATE, b"cp"),
            el(
                CONTENT_ENCODINGS,
                &cat(&[
                    enc(0, 3, 0, Some((3, b"HS"))),
                    enc(2, 1, 1, None),
                    enc(1, 1, 0, Some((2, b""))),
                ]),
            ),
        ]),
    );
    let t = parse::tracks(&te, 1_000_000);
    assert_eq!(t.len(), 1);
    assert_eq!(
        t[0].encodings,
        [
            ContentEncoding::Encryption { scope: 1 },
            ContentEncoding::Compression { algo: 2, scope: 1 },
            ContentEncoding::HeaderStripping {
                bytes: b"HS".to_vec(),
                scope: 3
            },
        ]
    );
    assert_eq!(t[0].codec_private, b"HScp");
    assert_eq!(t[0].audio, Some(AudioInfo::default()));
    assert!(!t[0].frames_readable());
}

#[test]
fn a_cluster_with_only_an_unknown_track_yields_no_samples() {
    let body = cat(&[
        el(INFO, &u(TIMESTAMP_SCALE, 1_000_000)),
        tracks(),
        unknown(
            CLUSTER,
            &cat(&[u(TIMESTAMP, 0), el(SIMPLE_BLOCK, &block(7, 0, 0x80, b"x"))]),
        ),
    ]);
    let d = cat(&[header(), unknown(SEGMENT, &body)]);
    let r = Reader::new(d.as_slice()).unwrap();
    assert!(r.tracks.iter().all(|t| t.samples.is_empty()));
    assert_eq!(r.clusters.len(), 1);
}

#[test]
fn bad_lace_headers_drop_the_block_not_the_file() {
    let bad: [(u8, Vec<u8>); 4] = [
        (0x02, vec![3, 255, 255]),
        (0x06, vec![2, 0x40]),
        (0x06, vec![2, 0x82, 0x81]),
        (0x04, vec![2, 1, 2]),
    ];
    for (flags, lace) in bad {
        let body = cat(&[
            tracks(),
            el(
                CLUSTER,
                &cat(&[
                    u(TIMESTAMP, 0),
                    el(SIMPLE_BLOCK, &block(2, 0, 0x80 | flags, &lace)),
                    el(SIMPLE_BLOCK, &block(1, 0, 0x80, b"ok")),
                ]),
            ),
        ]);
        let d = cat(&[header(), unknown(SEGMENT, &body)]);
        let r = Reader::new(d.as_slice()).unwrap();
        assert!(r.tracks[1].samples.is_empty(), "flags {flags:#x} {lace:?}");
        assert_eq!(r.read(0, 0).unwrap(), b"ok");
    }
}

#[test]
fn headers_that_are_not_matroska_are_refused() {
    let wrong = cat(&[el(EBML, &el(DOC_TYPE, b"mp4")), unknown(SEGMENT, &[])]);
    assert!(matches!(
        Reader::new(wrong.as_slice()),
        Err(Error::Invalid(_))
    ));
    let wide = cat(&[
        el(EBML, &cat(&[u(EBML_MAX_ID_LENGTH, 5)])),
        unknown(SEGMENT, &[]),
    ]);
    assert!(matches!(
        Reader::new(wide.as_slice()),
        Err(Error::Unsupported(_))
    ));
    let long = cat(&[
        el(EBML, &cat(&[u(EBML_MAX_SIZE_LENGTH, 9)])),
        unknown(SEGMENT, &[]),
    ]);
    assert!(matches!(
        Reader::new(long.as_slice()),
        Err(Error::Unsupported(_))
    ));
    let before = cat(&[header(), unknown(VOID, &[0; 4]), unknown(SEGMENT, &[])]);
    assert!(Reader::new(before.as_slice()).is_err());
    let none = header();
    assert!(Reader::new(none.as_slice()).is_err());
    assert!(Reader::new(&b"RIFF\0\0\0\0WEBPVP8 "[..]).is_err());
}

#[test]
fn truncated_and_corrupted_copies_never_panic() {
    let d = file();
    let start = header().len() + 12;
    for cut in 0..d.len() {
        let r = Reader::new(&d[..cut]);
        if cut < start {
            assert!(r.is_err(), "cut {cut}");
        }
        if let Ok(r) = r {
            for (ti, t) in r.tracks.iter().enumerate() {
                for (i, s) in t.samples.iter().enumerate() {
                    let got = r.read(ti, i);
                    if t.frames_readable() && s.offset + s.size as u64 > cut as u64 {
                        assert!(
                            got.is_err(),
                            "cut {cut}: track {ti} sample {i} past the end"
                        );
                    }
                    let _ = r.read_alpha(ti, i, &mut Vec::new());
                }
            }
        }
    }
    for at in 0..d.len() {
        for v in [0x00, 0xFF, d[at] ^ 0x80] {
            let mut c = d.clone();
            c[at] = v;
            if let Ok(r) = Reader::new(c.as_slice()) {
                for (ti, t) in r.tracks.iter().enumerate() {
                    for i in 0..t.samples.len() {
                        let _ = r.read(ti, i);
                        let _ = r.read_alpha(ti, i, &mut Vec::new());
                    }
                }
                let mut s = r.stream();
                let mut n = 0;
                while let Ok(Some(_)) = s.next_packet() {
                    n += 1;
                    assert!(n <= c.len());
                }
            }
        }
    }
}

#[test]
fn signed_ints_read_as_twos_complement() {
    assert_eq!(ebml::sint(&[]), 0);
    assert_eq!(ebml::sint(&[0xFF]), -1);
    assert_eq!(ebml::sint(&[0x01, 0x00]), 256);
    assert_eq!(ebml::sint(&[0xFF, 0x00]), -256);
    assert_eq!(ebml::sint(&[0x80, 0, 0, 0, 0, 0, 0, 0]), i64::MIN);
    assert_eq!(
        ebml::sint(&[0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x12]),
        i64::MAX
    );
}

#[test]
fn discard_padding_lands_on_the_first_lace_when_negative_and_the_last_when_positive() {
    let mut fixed = vec![1];
    fixed.extend(fill(6, 7));
    fixed.extend(fill(6, 8));
    let group = |pad: Vec<u8>| {
        el(
            BLOCK_GROUP,
            &cat(&[el(BLOCK, &block(2, 0, 0x04, &fixed)), pad]),
        )
    };
    let body = cat(&[
        el(INFO, &u(TIMESTAMP_SCALE, 1_000_000)),
        tracks(),
        el(
            CLUSTER,
            &cat(&[
                u(TIMESTAMP, 0),
                group(u(DISCARD_PADDING, 6_500_000)),
                group(el(DISCARD_PADDING, &[0xFF, 0x00])),
                group(Vec::new()),
            ]),
        ),
    ]);
    let d = cat(&[header(), unknown(SEGMENT, &body)]);
    let r = Reader::new(d.as_slice()).unwrap();
    let got: Vec<i64> = r.tracks[1].samples.iter().map(|s| s.discard).collect();
    assert_eq!(got, [0, 6_500_000, -256, 0, 0, 0]);
    let mut s = r.stream();
    let mut walked = Vec::new();
    while let Some(b) = s.next_block(Some(1)).unwrap() {
        walked.push(b.discard);
    }
    assert_eq!(walked, got);
}
