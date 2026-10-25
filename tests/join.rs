#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]

#[path = "../src/testref.rs"]
mod testref;

#[allow(dead_code)]
#[path = "../src/join/refs.rs"]
mod refs;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pito_codecs::media::{
    Bitstream, Choice, Codec, Error, JoinAudio, JoinMethod, JoinOptions, Joined, Packet, Prefs,
    Rate, TimeBase, audio_pcm, decode_video, join, probe,
};
use pito_codecs::{h264, mp4};
use testref::{AudioCodec, Clip, Refs, VideoCodec};

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn seconds(path: &Path) -> f64 {
    probe(path, &Prefs::default()).unwrap().duration
}

fn decoding() -> Prefs {
    Prefs {
        decode: Choice::Cpu,
        encode: Choice::Cpu,
        ..Prefs::default()
    }
}

fn video_samples(path: &Path) -> Vec<Vec<u8>> {
    let r = mp4::Reader::open(path).unwrap();
    let v = r.video().unwrap();
    (0..r.tracks[v].samples.len())
        .map(|i| {
            let mut d = Vec::new();
            r.read_into(v, i, &mut d).unwrap();
            d
        })
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    Refs::open(refs::JOIN).path(name)
}

#[test]
fn a_join_copies_matching_clips_and_letterboxes_the_rest_with_silence() {
    let part = Clip::new((640, 360), (30, 1), 60).make(refs::JOIN, "join-part.mp4");
    let tall = Clip::new((360, 640), (30, 1), 30)
        .video_only()
        .make(refs::JOIN, "join-tall.mp4");
    let copied = dir().join("join-copied.mp4");
    let three = [part.clone(), part.clone(), part.clone()];
    let j = join(&three, &copied, cpu(), |_| true).unwrap();
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    let one = video_samples(&part);
    let all = video_samples(&copied);
    assert_eq!(all.len(), one.len() * 3);
    assert!(all.chunks(one.len()).all(|c| c == one.as_slice()));
    let (whole, single) = (seconds(&copied), seconds(&part));
    assert!((whole - 3.0 * single).abs() < 0.05, "{whole} vs {single}");

    let mixed = dir().join("join-mixed.mp4");
    let j = join(&[part, tall], &mixed, cpu(), |_| true).unwrap();
    assert_eq!(j.method, JoinMethod::Encode);
    assert!(j.reason.contains("is 360x640, not 640x360"), "{}", j.reason);
    assert_eq!((j.size, j.fps), ((640, 360), (30, 1)));
    let frame = decode_video(&mixed, 2.5, None, &decoding())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!((frame.width, frame.height), (640, 360));
    let y = &frame.planes[0];
    let column = |x: usize| {
        (0..360)
            .map(|r| y.data[r * y.stride + x] as f64)
            .sum::<f64>()
            / 360.0
    };
    assert!(column(10) < 20.0 && column(630) < 20.0);
    assert!((230..410).map(column).sum::<f64>() / 180.0 > 40.0);
    let pcm = audio_pcm(&mixed, 0, 48_000, false, &decoding())
        .unwrap()
        .samples;
    assert!(
        (pcm.len() as f64 / 96_000.0 - 3.0).abs() < 0.05,
        "{}",
        pcm.len()
    );
    let loud = |a: f64, b: f64| {
        pcm[(a * 96_000.0) as usize..(b * 96_000.0) as usize]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    assert!(loud(0.5, 1.5) > 0.05);
    assert!(loud(2.2, 2.9) < 0.001);
}

#[test]
fn a_10_bit_4_2_2_h264_joins_by_copy_and_is_refused_by_name_when_it_must_be_re_encoded() {
    let deep = fixture("h264-422-10.mp4");
    let plain = clips().silent.clone();
    let prefs = Prefs::default();
    let copied = dir().join("join-h264-422-10-copied.mp4");
    let j = join(&[deep.clone(), deep.clone()], &copied, cpu(), |_| true).unwrap();
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    let one = video_samples(&deep);
    let all = video_samples(&copied);
    assert_eq!(all.len(), one.len() * 2);
    assert!(all.chunks(one.len()).all(|c| c == one.as_slice()));
    let p = probe(&copied, &prefs).unwrap();
    assert_eq!(p.video_variant(), "10-bit 4:2:2 H.264");
    assert!(!p.video_native());

    let name = deep.display().to_string();
    for (inputs, opts) in [
        ([plain.clone(), deep.clone()], cpu()),
        (
            [deep.clone(), deep.clone()],
            JoinOptions {
                encode: true,
                ..cpu()
            },
        ),
    ] {
        let out = dir().join("join-h264-422-10-refused.mp4");
        let _ = std::fs::remove_file(&out);
        let e = join(&inputs, &out, opts, |_| true).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("unsupported: {name}: 10-bit 4:2:2 H.264 video is not supported")
        );
        assert!(
            !e.replace(env!("CARGO_MANIFEST_DIR"), "")
                .to_lowercase()
                .contains("ffmpeg"),
            "{e}"
        );
        assert!(!out.exists());
    }
}

fn story(name: &str) -> PathBuf {
    let d = dir().join("join-story").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip(frames: u64) -> Clip {
    Clip::new((352, 240), (30, 1), frames)
}

fn made(clip: Clip, name: &str) -> PathBuf {
    clip.make(refs::JOIN, name)
}

fn atomically(to: &Path, write: impl FnOnce(&Path)) -> PathBuf {
    let name = to.file_name().unwrap().to_string_lossy().into_owned();
    let part = to.with_file_name(format!(".{}-{name}", std::process::id()));
    write(&part);
    std::fs::rename(&part, to).unwrap();
    to.to_path_buf()
}

fn remuxed(parts: &[(&Path, bool)], name: &str) -> PathBuf {
    let to = testref::scratch(refs::JOIN).join(name);
    atomically(&to, |part| {
        let mut w = mp4::Writer::create(part).unwrap();
        let mut lanes = Vec::new();
        for (from, video) in parts {
            let r = mp4::Reader::open(from).unwrap();
            let k = if *video { r.video() } else { r.audio().first().copied() }.unwrap();
            let t = &r.tracks[k];
            let cfg = mp4::TrackConfig {
                media_start: t.edits.first().map(|e| e.media_time).filter(|m| *m > 0),
                ..mp4::TrackConfig::from_track(t).unwrap()
            };
            lanes.push((w.add_track(cfg).unwrap(), r, k));
        }
        for (out, r, k) in &lanes {
            for (i, s) in r.tracks[*k].samples.iter().enumerate() {
                w.push_timed(*out, &r.packet(*k, i).unwrap(), s.duration)
                    .unwrap();
            }
        }
        w.finish().unwrap();
    })
}

fn hdr_video(name: &str) -> PathBuf {
    let to = testref::scratch(refs::JOIN).join(name);
    atomically(&to, |part| {
        let (w, h) = (352, 240);
        let mut e = h264::Encoder::new(h264::EncoderConfig {
            width: w,
            height: h,
            fps: (30, 1),
            rate: Rate::Qp(24),
            gop: 30,
            b_frames: 0,
            threads: 1,
            bitstream: Bitstream::LengthPrefixed,
            color: h264::Color {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false,
            },
            ..Default::default()
        })
        .unwrap();
        let entry = mp4::SampleEntry::h264(
            mp4::AvcConfig::parse(&e.avcc()).unwrap(),
            w as u16,
            h as u16,
        );
        let mut out = mp4::Writer::create(part).unwrap();
        let track = out
            .add_track(mp4::TrackConfig::new(entry, 30 * 64))
            .unwrap();
        let mut packets = Vec::new();
        for n in 0..30 {
            let mut f = testref::yuv_frame(w, h, n, 0);
            f.time_base = TimeBase::new(1, 30);
            packets.extend(e.encode(&f).unwrap());
        }
        packets.extend(e.flush().unwrap());
        for p in packets {
            let p = Packet {
                pts: p.pts * 64,
                dts: p.dts * 64,
                ..p
            };
            out.push(track, &p).unwrap();
        }
        out.finish().unwrap();
    })
}

struct Clips {
    plain: PathBuf,
    plain_mkv: PathBuf,
    slower: PathBuf,
    silent: PathBuf,
    mono: PathBuf,
    cd_rate: PathBuf,
    swapped: PathBuf,
    hdr: PathBuf,
    sound: PathBuf,
}

fn clips() -> &'static Clips {
    static CLIPS: OnceLock<Clips> = OnceLock::new();
    CLIPS.get_or_init(|| {
        let plain = made(clip(30), "js-plain.mp4");
        let hdr = hdr_video("js-hdr-video.mp4");
        Clips {
            plain_mkv: made(clip(30), "js-plain.mkv"),
            slower: made(Clip::new((352, 240), (25, 1), 25), "js-25.mp4"),
            silent: made(clip(30).video_only(), "js-silent.mp4"),
            mono: made(
                Clip {
                    channels: 1,
                    ..clip(30)
                },
                "js-mono.mp4",
            ),
            cd_rate: made(
                Clip {
                    rate: 44_100,
                    ..clip(30)
                },
                "js-44k.mp4",
            ),
            swapped: remuxed(&[(&plain, false), (&plain, true)], "js-swapped.mp4"),
            hdr: remuxed(&[(&hdr, true), (&plain, false)], "js-hdr-tagged.mp4"),
            sound: made(clip(30).audio_only(), "js-sound.m4a"),
            plain,
        }
    })
}

fn leftovers(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("partial") || n.contains("joining"))
        .collect()
}

fn cpu() -> JoinOptions {
    JoinOptions {
        prefs: Prefs {
            encode: Choice::Cpu,
            ..Prefs::default()
        },
        ..JoinOptions::default()
    }
}

fn joined(inputs: &[PathBuf], out: &Path, opts: JoinOptions) -> Joined {
    let j = join(inputs, out, opts, |_| true).unwrap_or_else(|e| panic!("{e}"));
    assert!(out.exists());
    assert!(leftovers(out.parent().unwrap()).is_empty());
    j
}

fn refused(inputs: &[PathBuf], out: &Path, opts: JoinOptions) -> String {
    let e = join(inputs, out, opts, |_| true).unwrap_err().to_string();
    assert!(!out.exists(), "{e}");
    assert!(leftovers(out.parent().unwrap()).is_empty(), "{e}");
    e
}

fn audio_codecs(path: &Path) -> Vec<String> {
    probe(path, &Prefs::default())
        .unwrap()
        .audio
        .iter()
        .map(|a| a.codec.to_ascii_lowercase())
        .collect()
}

#[test]
fn the_same_streams_in_mp4_and_mkv_join_by_copy_into_either_container() {
    let c = clips();
    let folder = story("mixed-containers");
    for out in ["copied.mp4", "copied.mkv", "copied.mov"] {
        let out = folder.join(out);
        let j = joined(&[c.plain.clone(), c.plain_mkv.clone()], &out, cpu());
        assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
        assert_eq!(
            j.reason,
            "every clip is h264 352x240 at 30 fps with aac stereo at 48000 Hz"
        );
        assert_eq!(j.audio, JoinAudio::Copied);
        assert_eq!((j.clips, j.size, j.fps), (2, (352, 240), (30, 1)));
        assert_eq!(j.frames, 60);
        assert!((j.seconds - 2.0).abs() < 0.05, "{}", j.seconds);
        assert!(j.reasons.is_empty());
        let p = probe(&out, &Prefs::default()).unwrap();
        assert_eq!(p.video_codec_name, "h264");
        assert!((seconds(&out) - 2.0).abs() < 0.1, "{}", out.display());
        assert_eq!(audio_codecs(&out), ["aac"]);
    }
}

#[test]
fn each_mismatch_re_encodes_with_its_own_reason() {
    let c = clips();
    let folder = story("mismatches");
    let name = |p: &Path| p.display().to_string();
    let cases = [
        (
            &c.plain,
            &c.slower,
            format!(
                "{} runs at 25 fps, not 30 fps like {}",
                name(&c.slower),
                name(&c.plain)
            ),
        ),
        (
            &c.plain,
            &c.silent,
            format!(
                "{} has no audio and {} has",
                name(&c.silent),
                name(&c.plain)
            ),
        ),
        (
            &c.silent,
            &c.plain,
            format!(
                "{} has audio and {} has none",
                name(&c.plain),
                name(&c.silent)
            ),
        ),
        (
            &c.plain,
            &c.mono,
            format!(
                "{}'s audio is mono, not stereo like {}'s",
                name(&c.mono),
                name(&c.plain)
            ),
        ),
        (
            &c.plain,
            &c.cd_rate,
            format!(
                "{}'s audio is 44100 Hz, not 48000 Hz like {}'s",
                name(&c.cd_rate),
                name(&c.plain)
            ),
        ),
        (
            &c.plain,
            &c.swapped,
            format!(
                "{}'s streams are in another order than {}'s",
                name(&c.swapped),
                name(&c.plain)
            ),
        ),
    ];
    for (k, (first, other, reason)) in cases.into_iter().enumerate() {
        let out = folder.join(format!("{k}.mp4"));
        let j = joined(&[first.clone(), other.clone()], &out, cpu());
        assert_eq!(j.method, JoinMethod::Encode, "{reason}");
        assert_eq!(j.reason, reason);
        assert_eq!((j.size, j.fps), ((352, 240), (30, 1)), "{reason}");
        assert!((58..=62).contains(&j.frames), "{reason}: {}", j.frames);
        assert!((j.seconds - 2.0).abs() < 0.05, "{reason}: {}", j.seconds);
        assert_eq!(
            j.audio,
            JoinAudio::Encoded {
                codec: Codec::Aac,
                kbps: 320
            },
            "{reason}"
        );
        assert_eq!(audio_codecs(&out), ["aac"], "{reason}");
        assert!((seconds(&out) - 2.0).abs() < 0.15, "{reason}");
    }
}

#[test]
fn hdr_tags_differ_and_the_re_encode_refuses_to_tone_map() {
    let c = clips();
    let folder = story("hdr");
    let e = refused(
        &[c.plain.clone(), c.hdr.clone()],
        &folder.join("out.mp4"),
        cpu(),
    );
    assert_eq!(
        e,
        format!(
            "unsupported: {}: the source is HDR or wide gamut (bt2020, smpte2084): the join writes SDR BT.709 and does not tone-map",
            c.hdr.display()
        )
    );
    let j = joined(
        &[c.hdr.clone(), c.hdr.clone()],
        &folder.join("same.mkv"),
        cpu(),
    );
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
}

#[test]
fn silent_clips_stay_silent_whether_copied_or_re_encoded() {
    let c = clips();
    let folder = story("silent");
    let two = [c.silent.clone(), c.silent.clone()];
    let out = folder.join("copied.mp4");
    let j = joined(&two, &out, cpu());
    assert_eq!(j.method, JoinMethod::Copy);
    assert_eq!(
        j.reason,
        "every clip is h264 352x240 at 30 fps without audio"
    );
    assert_eq!(j.audio, JoinAudio::Silent);
    assert!(audio_codecs(&out).is_empty());
    for name in ["encoded.mp4", "encoded.mkv", "encoded.webm"] {
        let out = folder.join(name);
        let j = joined(
            &two,
            &out,
            JoinOptions {
                encode: true,
                ..cpu()
            },
        );
        assert_eq!(j.method, JoinMethod::Encode, "{name}");
        assert_eq!(j.reason, "a video option asks for a re-encode");
        assert_eq!(j.audio, JoinAudio::Silent, "{name}");
        assert!(audio_codecs(&out).is_empty(), "{name}");
    }
    let e = refused(
        &two,
        &folder.join("loud.mp4"),
        JoinOptions {
            audio_kbps: Some(128),
            ..cpu()
        },
    );
    assert_eq!(
        e,
        "invalid data: the clips have no audio to encode at 128 kbps"
    );
}

#[test]
fn an_audio_bitrate_re_encodes_only_the_audio_at_a_rate_its_encoder_takes() {
    let c = clips();
    let folder = story("audio-bitrate");
    let two = [c.plain.clone(), c.plain.clone()];
    for (kbps, got) in [(6, 32), (192, 192), (512, 512)] {
        let out = folder.join(format!("{kbps}.mp4"));
        let j = joined(
            &two,
            &out,
            JoinOptions {
                audio_kbps: Some(kbps),
                ..cpu()
            },
        );
        assert_eq!(j.method, JoinMethod::Copy, "{kbps}");
        assert_eq!(
            j.audio,
            JoinAudio::Encoded {
                codec: Codec::Aac,
                kbps: got
            }
        );
        assert_eq!(video_samples(&out).len(), 60);
        assert_eq!(audio_codecs(&out), ["aac"]);
    }
    for kbps in [5, 513] {
        let e = refused(
            &two,
            &folder.join("bad.mp4"),
            JoinOptions {
                audio_kbps: Some(kbps),
                ..cpu()
            },
        );
        assert_eq!(
            e,
            format!("invalid data: an audio bitrate of {kbps} kbps, not 6 to 512")
        );
    }
}

#[test]
fn h264_clips_into_webm_encode_vp9_and_opus_and_say_why() {
    let c = clips();
    let folder = story("into-webm");
    let out = folder.join("out.webm");
    let j = joined(&[c.plain.clone(), c.plain.clone()], &out, cpu());
    assert_eq!(j.method, JoinMethod::Encode);
    assert_eq!(j.reason, ".webm doesn't carry h264");
    assert!(
        j.reasons
            .iter()
            .any(|r| r == "WebM carries VP9, not H.264, so the join encodes VP9"),
        "{:?}",
        j.reasons
    );
    assert_eq!(j.video, Codec::Vp9);
    assert_eq!(
        j.audio,
        JoinAudio::Encoded {
            codec: Codec::Opus,
            kbps: 128
        }
    );
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!(p.video_codec_name, "vp9");
    assert_eq!(audio_codecs(&out), ["opus"]);
    assert!((seconds(&out) - 2.0).abs() < 0.15);
}

#[test]
fn vp9_clips_copy_into_webm_and_mp4_and_re_encode_into_mov() {
    let vp9 = made(
        Clip {
            video: Some(VideoCodec::Vp9),
            audio: Some(AudioCodec::Opus),
            ..clip(30)
        },
        "js-vp9.webm",
    );
    let folder = story("vp9");
    let two = [vp9.clone(), vp9.clone()];

    let out = folder.join("copied.webm");
    let j = joined(&two, &out, cpu());
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    assert_eq!(j.audio, JoinAudio::Copied);
    assert_eq!(j.video, Codec::Vp9);
    assert_eq!(audio_codecs(&out), ["opus"]);

    let out = folder.join("copied.mp4");
    let j = joined(&two, &out, cpu());
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    assert_eq!(
        j.audio,
        JoinAudio::Encoded {
            codec: Codec::Aac,
            kbps: 320
        }
    );
    assert_eq!(
        probe(&out, &Prefs::default()).unwrap().video_codec_name,
        "vp9"
    );
    assert_eq!(video_samples(&out).len(), 60);
    assert_eq!(audio_codecs(&out), ["aac"]);

    let out = folder.join("loudest.webm");
    let j = joined(
        &two,
        &out,
        JoinOptions {
            audio_kbps: Some(512),
            ..cpu()
        },
    );
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    assert_eq!(
        j.audio,
        JoinAudio::Encoded {
            codec: Codec::Opus,
            kbps: 510
        }
    );
    assert_eq!(audio_codecs(&out), ["opus"]);

    let out = folder.join("encoded.mov");
    let j = joined(
        &two,
        &out,
        JoinOptions {
            video: Codec::Vp9,
            ..cpu()
        },
    );
    assert_eq!(j.method, JoinMethod::Encode);
    assert_eq!(j.reason, ".mov doesn't carry vp9");
    assert!(
        j.reasons
            .iter()
            .any(|r| r == "MOV doesn't carry VP9, so the join encodes H.264"),
        "{:?}",
        j.reasons
    );
    assert_eq!(j.video, Codec::H264);
    assert_eq!(
        probe(&out, &Prefs::default()).unwrap().video_codec_name,
        "h264"
    );
    assert_eq!(audio_codecs(&out), ["aac"]);
}

#[test]
fn hevc_and_h264_clips_re_encode_to_h264() {
    let c = clips();
    let hevc = fixture("hevc.mp4");
    let out = story("hevc").join("out.mp4");
    let j = joined(&[c.plain.clone(), hevc.clone()], &out, cpu());
    assert_eq!(j.method, JoinMethod::Encode);
    assert_eq!(
        j.reason,
        format!(
            "{} is hevc, not h264 like {}",
            hevc.display(),
            c.plain.display()
        )
    );
    assert_eq!(j.video, Codec::H264);
    assert_eq!(
        probe(&out, &Prefs::default()).unwrap().video_codec_name,
        "h264"
    );
}

#[test]
fn video_options_re_encode_to_the_asked_even_size_and_rate() {
    let c = clips();
    let folder = story("options");
    let two = [c.plain.clone(), c.plain.clone()];
    let out = folder.join("sized.mp4");
    let j = joined(
        &two,
        &out,
        JoinOptions {
            size: Some((705, 481)),
            fps: Some((15, 1)),
            ..cpu()
        },
    );
    assert_eq!(j.method, JoinMethod::Encode);
    assert_eq!(j.reason, "a video option asks for a re-encode");
    assert_eq!((j.size, j.fps), ((704, 480), (15, 1)));
    assert!((29..=31).contains(&j.frames), "{}", j.frames);
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height, p.fps), (704, 480, (15, 1)));

    for (opts, want) in [
        (
            JoinOptions {
                size: Some((1, 1)),
                ..cpu()
            },
            "invalid data: a size of 1x1 cannot make an even output".to_string(),
        ),
        (
            JoinOptions {
                fps: Some((0, 1)),
                ..cpu()
            },
            "invalid data: a frame rate of 0/1".to_string(),
        ),
    ] {
        assert_eq!(refused(&two, &folder.join("bad.mp4"), opts), want);
    }
}

#[test]
fn a_join_refuses_bad_clips_and_outputs_by_name_and_leaves_nothing() {
    let c = clips();
    let folder = story("refused");
    let out = folder.join("out.mp4");
    let missing = folder.join("missing.mp4");
    let empty = folder.join("empty.mp4");
    std::fs::write(&empty, b"").unwrap();
    let junk = folder.join("junk.mp4");
    std::fs::write(&junk, b"this is not a video file at all").unwrap();
    let opts = JoinOptions::default;

    assert_eq!(
        refused(std::slice::from_ref(&c.plain), &out, opts()),
        "invalid data: a join takes at least two clips"
    );
    assert_eq!(
        refused(
            &[c.plain.clone(), c.plain.clone()],
            &folder.join("out.avi"),
            opts()
        ),
        "invalid data: join writes mp4, mov, mkv or webm, not .avi"
    );
    let inside = folder.join("inside.mp4");
    std::fs::copy(&c.plain, &inside).unwrap();
    let e = join(&[c.plain.clone(), inside.clone()], &inside, opts(), |_| {
        true
    })
    .unwrap_err()
    .to_string();
    assert_eq!(
        e,
        format!(
            "invalid data: {} is both a clip and the output",
            inside.display()
        )
    );
    assert_eq!(
        std::fs::read(&inside).unwrap(),
        std::fs::read(&c.plain).unwrap()
    );

    for bad in [&missing, &empty, &junk] {
        let e = refused(&[c.plain.clone(), bad.clone()], &out, opts());
        assert!(e.contains(&bad.display().to_string()), "{e}");
        assert!(!e.to_lowercase().contains("panic"), "{e}");
    }
    for inputs in [
        [c.plain.clone(), c.sound.clone()],
        [c.sound.clone(), c.plain.clone()],
    ] {
        assert_eq!(
            refused(&inputs, &out, opts()),
            format!("invalid data: {}: no video track", c.sound.display())
        );
    }
}

fn emptied(from: &Path, to: &Path) {
    let r = pito_codecs::mp4::Reader::open(from).unwrap();
    let v = r.video().unwrap();
    let mut w = pito_codecs::mp4::Writer::create(to).unwrap();
    w.add_track(pito_codecs::mp4::TrackConfig::from_track(&r.tracks[v]).unwrap())
        .unwrap();
    w.finish().unwrap();
}

#[test]
fn a_clip_with_no_frames_is_refused_by_name_on_both_paths() {
    let c = clips();
    let folder = story("zero-length");
    let empty = folder.join("empty.mp4");
    emptied(&c.silent, &empty);
    assert!(video_samples(&empty).is_empty());
    let name = empty.display();
    let encoded = format!("invalid data: {name} has no known length to join");
    for (inputs, opts, want) in [
        ([c.plain.clone(), empty.clone()], cpu(), &encoded),
        (
            [c.silent.clone(), empty.clone()],
            JoinOptions {
                encode: true,
                ..cpu()
            },
            &encoded,
        ),
        (
            [c.silent.clone(), empty.clone()],
            cpu(),
            &format!("invalid data: {name} has no video frames"),
        ),
    ] {
        for out in ["out.mp4", "out.mkv"] {
            assert_eq!(&refused(&inputs, &folder.join(out), opts.clone()), want);
        }
    }
}

#[test]
fn a_clip_shorter_than_one_output_frame_adds_no_time() {
    let c = clips();
    let one = made(clip(1).video_only(), "js-one-frame.mp4");
    let folder = story("sub-frame");
    let out = folder.join("out.mp4");
    let j = joined(
        &[c.silent.clone(), one.clone(), c.silent.clone()],
        &out,
        JoinOptions {
            fps: Some((10, 1)),
            prefs: Prefs {
                encode: Choice::Cpu,
                ..Prefs::default()
            },
            ..cpu()
        },
    );
    assert_eq!(j.method, JoinMethod::Encode);
    assert_eq!(j.reason, "a video option asks for a re-encode");
    assert_eq!((j.clips, j.fps), (3, (10, 1)));
    assert!((j.seconds - 2.0).abs() < 1e-9, "{}", j.seconds);
    assert!((20..=21).contains(&j.frames), "{}", j.frames);
    assert_eq!(j.audio, JoinAudio::Silent);
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height, p.fps), (352, 240, (10, 1)));
    assert!(audio_codecs(&out).is_empty());
    assert!((seconds(&out) - 2.0).abs() < 0.15, "{}", seconds(&out));
}

#[test]
fn clips_the_join_cannot_read_are_refused_by_name_and_4_4_4_only_when_re_encoded() {
    let c = clips();
    let folder = story("undecodable");
    let out = folder.join("out.mp4");
    let full = fixture("h264-444.mp4");
    let copied = folder.join("copied.mp4");
    let j = joined(&[full.clone(), full.clone()], &copied, cpu());
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    assert_eq!(video_samples(&copied).len(), 2 * video_samples(&full).len());
    let want = format!(
        "unsupported: {}: 8-bit 4:4:4 H.264 video is not supported",
        full.display()
    );
    for inputs in [
        [full.clone(), c.silent.clone()],
        [c.silent.clone(), full.clone()],
    ] {
        assert_eq!(refused(&inputs, &out, cpu()), want);
    }
    let hevc = fixture("hevc-422.mp4");
    assert_eq!(
        refused(&[c.silent.clone(), hevc.clone()], &out, cpu()),
        format!(
            "unsupported: {}: 8-bit 4:2:2 HEVC video is not supported",
            hevc.display()
        )
    );
    let part2 = fixture("mpeg4.mp4");
    for inputs in [
        [c.silent.clone(), part2.clone()],
        [part2.clone(), part2.clone()],
    ] {
        assert_eq!(
            refused(&inputs, &out, cpu()),
            format!(
                "unsupported: {}: MPEG-4 Part 2 video is not supported",
                part2.display()
            )
        );
    }
}

#[test]
fn a_cancelled_join_stops_and_leaves_nothing() {
    let c = clips();
    let folder = story("cancelled");
    let two = [c.plain.clone(), c.plain.clone()];
    for (name, encode) in [
        ("copy.mp4", false),
        ("encode.mp4", true),
        ("encode.mkv", true),
    ] {
        let out = folder.join(name);
        let mut asked = 0;
        let e = join(&two, &out, JoinOptions { encode, ..cpu() }, |_| {
            asked += 1;
            false
        })
        .unwrap_err();
        assert!(matches!(e, Error::Cancelled), "{name}: {e}");
        assert!(asked > 0, "{name}");
        assert!(!out.exists(), "{name}");
        assert!(
            leftovers(&folder).is_empty(),
            "{name}: {:?}",
            leftovers(&folder)
        );
    }
}
