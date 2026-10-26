use std::path::{Path, PathBuf};

use pito_codecs::media::{self, AudioBuffer, Bitstream, Frame, Packet, PixelFormat, TimeBase};
use pito_codecs::{aac, h264, mkv, mp4};

use super::Sound;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    #[cfg(feature = "vp9")]
    Vp9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    #[cfg(feature = "opus")]
    Opus,
}

#[derive(Debug, Clone, Copy)]
pub enum Picture {
    Pattern,
    Noisy(u8),
    Fn(fn(u64, &mut Frame)),
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub frames: u64,
    pub video: Option<VideoCodec>,
    pub picture: Picture,
    pub gop: u32,
    pub qp: u8,
    pub b_frames: u8,
    pub audio: Option<AudioCodec>,
    pub sounds: Vec<Sound>,
    pub rate: u32,
    pub channels: u16,
    pub kbps: u32,
}

impl Clip {
    pub fn new(size: (u32, u32), fps: (u32, u32), frames: u64) -> Clip {
        Clip {
            size,
            fps,
            frames,
            video: Some(VideoCodec::H264),
            picture: Picture::Pattern,
            gop: fps.0.div_ceil(fps.1.max(1)).clamp(1, 250),
            qp: 24,
            b_frames: 0,
            audio: Some(AudioCodec::Aac),
            sounds: vec![Sound::Sine {
                hz: 440.0,
                amp: 0.25,
            }],
            rate: 48_000,
            channels: 2,
            kbps: 128,
        }
    }

    pub fn video_only(mut self) -> Clip {
        self.audio = None;
        self.sounds.clear();
        self
    }

    pub fn audio_only(mut self) -> Clip {
        self.video = None;
        self
    }

    pub fn samples(&self) -> u64 {
        self.samples_at(self.frames)
    }

    fn samples_at(&self, frames: u64) -> u64 {
        let (num, den) = (u64::from(self.fps.0.max(1)), u64::from(self.fps.1));
        frames * den * u64::from(self.rate) / num
    }

    pub fn make(&self, area: &str, name: &str) -> PathBuf {
        let path = super::scratch(area).join(name);
        self.write(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        path
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let part = super::part_of(path);
        let made = self.write_to(&part, path);
        if made.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        made?;
        std::fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn write_to(&self, part: &Path, named: &Path) -> Result<(), String> {
        let ext = named
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let (w, h) = self.size;
        let frame_tb = TimeBase::new(self.fps.1, self.fps.0);
        let frame_ns = (1_000_000_000 * u64::from(self.fps.1) + u64::from(self.fps.0) / 2)
            / u64::from(self.fps.0.max(1));
        let mut venc = match self.video {
            None => None,
            Some(VideoCodec::H264) => Some(Venc::H264(Box::new(
                h264::Encoder::new(h264::EncoderConfig {
                    width: w,
                    height: h,
                    fps: self.fps,
                    rate: media::Rate::Qp(self.qp),
                    gop: self.gop,
                    b_frames: self.b_frames,
                    threads: 1,
                    bitstream: Bitstream::LengthPrefixed,
                    ..Default::default()
                })
                .map_err(text)?,
            ))),
            #[cfg(feature = "vp9")]
            Some(VideoCodec::Vp9) => Some(Venc::Vp9(Box::new(
                pito_codecs::vp9::Encoder::new(pito_codecs::vp9::EncoderConfig {
                    width: w,
                    height: h,
                    fps: self.fps,
                    rate: pito_codecs::vp9::Rate::Q(self.qp),
                    gop: self.gop,
                    threads: 1,
                    aq: false,
                })
                .map_err(text)?,
            ))),
        };
        let mut sink = match ext.as_str() {
            "mp4" | "m4a" | "mov" => {
                let options = mp4::Options {
                    brand: if ext == "mov" {
                        mp4::Brand::Mov
                    } else {
                        mp4::Brand::Mp4
                    },
                    ..Default::default()
                };
                let mut w4 = mp4::Writer::create_with(part, options).map_err(text)?;
                w4.set_movie_timescale(if self.audio.is_some() {
                    self.rate
                } else {
                    1000
                });
                Sink::Mp4(Box::new(w4))
            }
            "mkv" | "mka" | "webm" => {
                let options = if ext == "webm" {
                    mkv::Options::webm()
                } else {
                    mkv::Options::default()
                };
                Sink::Mkv(Box::new(
                    mkv::Writer::create_with(part, options).map_err(text)?,
                ))
            }
            _ => {
                return Err(format!(
                    "a clip is mp4, m4a, mov, mkv, mka or webm, not '{ext}'"
                ));
            }
        };
        let step = i64::from(self.fps.1) * 64;
        let video = match (&mut sink, &venc) {
            (_, None) => None,
            (Sink::Mp4(m), Some(Venc::H264(e))) => {
                let entry = mp4::SampleEntry::h264(
                    mp4::AvcConfig::parse(&e.avcc()).map_err(text)?,
                    w as u16,
                    h as u16,
                );
                let scale = self
                    .fps
                    .0
                    .checked_mul(64)
                    .ok_or("the frame rate is out of range")?;
                let track = m
                    .add_track(mp4::TrackConfig::new(entry, scale))
                    .map_err(text)?;
                let one = u32::try_from(step).map_err(text)?;
                m.set_frame_duration(track, one).map_err(text)?;
                Some(track)
            }
            (Sink::Mkv(m), Some(Venc::H264(e))) => Some(
                m.add_track(mkv::TrackConfig {
                    default_duration: Some(frame_ns),
                    ..mkv::TrackConfig::h264(e.avcc(), w, h, frame_tb)
                })
                .map_err(text)?,
            ),
            #[cfg(feature = "vp9")]
            (Sink::Mkv(m), Some(Venc::Vp9(_))) => Some(
                m.add_track(mkv::TrackConfig {
                    default_duration: Some(frame_ns),
                    ..mkv::TrackConfig::vp9(w, h, frame_tb)
                })
                .map_err(text)?,
            ),
            #[cfg(feature = "vp9")]
            (Sink::Mp4(_), Some(Venc::Vp9(_))) => {
                return Err("VP9 clips go in mkv or webm".into());
            }
        };
        let total = self.samples() as i64;
        let mut lanes = Vec::new();
        if let Some(codec) = self.audio {
            for (k, sound) in self.sounds.iter().enumerate() {
                lanes.push(self.lane(codec, *sound, &mut sink, total, k)?);
            }
        }
        let mut frame = Frame::new(PixelFormat::Yuv420p8, w.max(2), h.max(2), 0, frame_tb);
        let mut done = 0u64;
        for n in 0..self.frames {
            if let Some(v) = venc.as_mut() {
                match self.picture {
                    Picture::Pattern => super::draw_yuv(n, 0, &mut frame),
                    Picture::Noisy(amp) => super::draw_yuv(n, amp, &mut frame),
                    Picture::Fn(f) => f(n, &mut frame),
                }
                frame.pts = n as i64;
                frame.time_base = frame_tb;
                for p in v.encode(&frame)? {
                    sink.video(video, step, p)?;
                }
            }
            let upto = self.samples_at(n + 1);
            while !lanes.is_empty() && done < upto {
                let count = (upto - done).min(4096) as usize;
                for lane in &mut lanes {
                    let buf = AudioBuffer {
                        samples: super::pcm(lane.sound, self.rate, self.channels, done, count),
                        rate: self.rate,
                        channels: self.channels,
                    };
                    let packets = lane.enc.encode(&buf)?;
                    sink.audio(lane, packets)?;
                }
                done += count as u64;
            }
        }
        if let Some(v) = venc.as_mut() {
            for p in v.flush()? {
                sink.video(video, step, p)?;
            }
        }
        for lane in &mut lanes {
            let packets = lane.enc.flush()?;
            sink.audio(lane, packets)?;
            sink.last_audio(lane)?;
        }
        sink.finish()
    }

    fn lane(
        &self,
        codec: AudioCodec,
        sound: Sound,
        sink: &mut Sink,
        total: i64,
        k: usize,
    ) -> Result<Lane, String> {
        let aac_enc = || {
            aac::Encoder::new(aac::EncoderConfig {
                rate: self.rate,
                channels: self.channels,
                bitrate: self.kbps * 1000,
            })
            .map_err(text)
        };
        let ch = u32::from(self.channels);
        let (enc, track, shift, end) = match (codec, sink) {
            (AudioCodec::Aac, Sink::Mp4(m)) => {
                let e = aac_enc()?;
                let priming = i64::from(e.priming());
                let entry = mp4::SampleEntry::aac(e.extradata(), ch, self.rate, self.kbps * 1000);
                let mut cfg = mp4::TrackConfig::new(entry, self.rate);
                cfg.media_start = Some(priming);
                let track = m.add_track(cfg).map_err(text)?;
                (Aenc::Aac(Box::new(e)), track, 0, priming + total)
            }
            (AudioCodec::Aac, Sink::Mkv(m)) => {
                let e = aac_enc()?;
                let priming = i64::from(e.priming());
                let mut cfg = mkv::TrackConfig::aac(e.extradata(), self.rate, ch);
                cfg.codec_delay = priming as u64 * 1_000_000_000 / u64::from(self.rate);
                let track = m.add_track(cfg).map_err(text)?;
                (Aenc::Aac(Box::new(e)), track, -priming, total)
            }
            #[cfg(feature = "opus")]
            (AudioCodec::Opus, Sink::Mkv(m)) => {
                let e = pito_codecs::opus::enc::Encoder::new(
                    pito_codecs::opus::enc::EncoderConfig::new(self.channels, self.kbps * 1000),
                )
                .map_err(text)?;
                let track = m
                    .add_track(mkv::TrackConfig::opus(e.codec_private()))
                    .map_err(text)?;
                let shift = -i64::from(e.pre_skip());
                (Aenc::Opus(Box::new(e)), track, shift, total)
            }
            #[cfg(feature = "opus")]
            (AudioCodec::Opus, Sink::Mp4(_)) => {
                return Err(format!("track {k}: Opus clips go in mkv, mka or webm"));
            }
        };
        Ok(Lane {
            sound,
            enc,
            track,
            shift,
            held: None,
            end,
        })
    }
}

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

enum Venc {
    H264(Box<h264::Encoder>),
    #[cfg(feature = "vp9")]
    Vp9(Box<pito_codecs::vp9::Encoder>),
}

impl Venc {
    fn encode(&mut self, f: &Frame) -> Result<Vec<Packet>, String> {
        match self {
            Venc::H264(e) => e.encode(f).map_err(text),
            #[cfg(feature = "vp9")]
            Venc::Vp9(e) => e.encode(f).map_err(text),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>, String> {
        match self {
            Venc::H264(e) => e.flush().map_err(text),
            #[cfg(feature = "vp9")]
            Venc::Vp9(e) => e.flush().map_err(text),
        }
    }
}

enum Aenc {
    Aac(Box<aac::Encoder>),
    #[cfg(feature = "opus")]
    Opus(Box<pito_codecs::opus::enc::Encoder>),
}

impl Aenc {
    fn encode(&mut self, b: &AudioBuffer) -> Result<Vec<Packet>, String> {
        match self {
            Aenc::Aac(e) => e.encode(b).map_err(text),
            #[cfg(feature = "opus")]
            Aenc::Opus(e) => e.encode(b).map_err(text),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>, String> {
        match self {
            Aenc::Aac(e) => Ok(e.flush()),
            #[cfg(feature = "opus")]
            Aenc::Opus(e) => e.flush().map_err(text),
        }
    }
}

struct Lane {
    sound: Sound,
    enc: Aenc,
    track: usize,
    shift: i64,
    held: Option<Packet>,
    end: i64,
}

enum Sink {
    Mp4(Box<mp4::Writer>),
    Mkv(Box<mkv::Writer>),
}

impl Sink {
    fn push(&mut self, track: usize, p: &Packet) -> Result<(), String> {
        match self {
            Sink::Mp4(w) => w.push(track, p).map_err(text),
            Sink::Mkv(w) => w.push(track, p).map_err(text),
        }
    }

    fn video(&mut self, track: Option<usize>, step: i64, p: Packet) -> Result<(), String> {
        let track = track.ok_or("a video packet with no video track")?;
        match self {
            Sink::Mp4(_) => self.push(
                track,
                &Packet {
                    pts: p.pts.saturating_mul(step),
                    dts: p.dts.saturating_mul(step),
                    ..p
                },
            ),
            Sink::Mkv(_) => self.push(track, &p),
        }
    }

    fn audio(&mut self, lane: &mut Lane, packets: Vec<Packet>) -> Result<(), String> {
        for mut p in packets {
            p.pts += lane.shift;
            p.dts += lane.shift;
            if let Some(prev) = lane.held.replace(p) {
                self.push(lane.track, &prev)?;
            }
        }
        Ok(())
    }

    fn last_audio(&mut self, lane: &mut Lane) -> Result<(), String> {
        let Some(p) = lane.held.take() else {
            return Ok(());
        };
        let left = u32::try_from(lane.end.saturating_sub(p.pts).max(1)).unwrap_or(u32::MAX);
        match self {
            Sink::Mp4(w) => w.push_timed(lane.track, &p, left).map_err(text),
            Sink::Mkv(w) => w.push_timed(lane.track, &p, left).map_err(text),
        }
    }

    fn finish(self) -> Result<(), String> {
        match self {
            Sink::Mp4(w) => w.finish_faststart().map(drop).map_err(text),
            Sink::Mkv(w) => w.finish().map(drop).map_err(text),
        }
    }
}
