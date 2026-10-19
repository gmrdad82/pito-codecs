use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const SUB: &str = "RYZEN 9 9950X, RADEON RX 9060 XT (VA-API), FFMPEG 9.0.1";
const US: &str = "PITO-CODECS";
const FF: &str = "FFMPEG 9.0.1";
const NOW: &str = "2026-10-14";
const RES: [&str; 2] = ["1080", "1440"];
const LIMIT: f64 = 0.05;

#[derive(Serialize)]
struct Chart<'a> {
    title: &'a str,
    unit: &'a str,
    better: &'a str,
    subtitle: &'a str,
    date: &'a str,
    parties: Vec<Party<'a>>,
    groups: Vec<Group>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suffix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<Vec<&'a str>>,
}

#[derive(Serialize)]
struct Party<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct Group {
    name: String,
    rows: Vec<Bar>,
}

#[derive(Serialize)]
struct Bar {
    label: String,
    values: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mark: Option<&'static str>,
}

struct Rows {
    by_id: HashMap<String, Value>,
    order: Vec<String>,
    date: String,
    commit: String,
}

impl Rows {
    fn read(path: &Path) -> Res<Rows> {
        let doc: Value = serde_json::from_slice(&fs::read(path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let rows = doc["rows"]
            .as_array()
            .ok_or_else(|| format!("{}: no rows", path.display()))?;
        let mut by_id = HashMap::new();
        let mut order = Vec::new();
        for r in rows {
            if let Some(id) = r["id"].as_str() {
                order.push(id.to_string());
                by_id.insert(id.to_string(), r.clone());
            }
        }
        let text = |k: &str| doc[k].as_str().unwrap_or("none").to_string();
        Ok(Rows {
            by_id,
            order,
            date: text("date"),
            commit: text("commit"),
        })
    }

    fn num(&self, id: &str, key: &str) -> Res<f64> {
        self.by_id
            .get(id)
            .and_then(|r| r[key].as_f64())
            .ok_or_else(|| format!("no {key} for {id}").into())
    }

    fn ours(&self, id: &str) -> Option<f64> {
        self.by_id.get(id).and_then(|r| r["ours"].as_f64())
    }
}

fn v(x: f64) -> Value {
    if x < 100.0 {
        Value::from(x)
    } else {
        Value::from(x.round() as i64)
    }
}

fn group(name: &str, rows: Vec<Bar>) -> Group {
    Group {
        name: name.to_string(),
        rows,
    }
}

fn bar(label: &str, values: Vec<Value>, mark: Option<&'static str>) -> Bar {
    Bar {
        label: label.to_string(),
        values,
        mark,
    }
}

fn fixed(label: &str, ours: f64, other: f64) -> Bar {
    let n = |x: f64| {
        if x.fract() == 0.0 {
            Value::from(x as i64)
        } else {
            Value::from(x)
        }
    };
    bar(label, vec![n(ours), n(other)], None)
}

struct Docs<'a> {
    rows: &'a Rows,
    out: &'a Path,
}

impl Docs<'_> {
    fn row(&self, id: &str, label: &str) -> Res<Bar> {
        Ok(bar(
            label,
            vec![
                v(self.rows.num(id, "ours")?),
                v(self.rows.num(id, "ffmpeg")?),
            ],
            None,
        ))
    }

    fn inv(&self, id: &str, label: &str) -> Res<Bar> {
        let r = |k: &str| -> Res<Value> { Ok(Value::from(1000.0 / self.rows.num(id, k)?)) };
        Ok(bar(label, vec![r("ours")?, r("ffmpeg")?], None))
    }

    fn write(&self, name: &str, chart: Chart) -> Res<()> {
        let path = self.out.join(format!("{name}.json"));
        let mut text = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(
            &mut text,
            serde_json::ser::PrettyFormatter::with_indent(b" "),
        );
        chart.serialize(&mut ser)?;
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    }

    fn chart<'a>(&'a self, title: &'a str, unit: &'a str, better: &'a str) -> Chart<'a> {
        Chart {
            title,
            unit,
            better,
            subtitle: SUB,
            date: &self.rows.date,
            parties: vec![Party { name: US }, Party { name: FF }],
            groups: Vec::new(),
            suffix: None,
            notes: None,
        }
    }

    fn decode(&self, route: &[(&str, &str)]) -> Res<Vec<Group>> {
        let mut gs = Vec::new();
        for (c, cn) in [("h264", "H.264"), ("hevc", "HEVC")] {
            let mut rows = Vec::new();
            for p in RES {
                for (k, kl) in route {
                    rows.push(self.row(&format!("decode.{c}.{p}.{k}"), &format!("{p}P {kl}"))?);
                }
            }
            gs.push(group(cn, rows));
        }
        Ok(gs)
    }

    fn all(&self) -> Res<()> {
        let fps = Some(" FPS");
        let parties = |names: &[&'static str]| names.iter().map(|&name| Party { name }).collect();
        self.write(
            "decode-cpu",
            Chart {
                groups: self.decode(&[("cpu-1t", "1 THREAD"), ("cpu-all", "ALL THREADS")])?,
                suffix: fps,
                ..self.chart("CPU DECODE", "FRAMES PER SECOND", "higher")
            },
        )?;
        self.write(
            "decode-gpu",
            Chart {
                groups: self
                    .decode(&[("gpu-surface", "ON THE GPU"), ("gpu-readback", "READ BACK")])?,
                suffix: fps,
                ..self.chart("GPU DECODE (VA-API)", "FRAMES PER SECOND", "higher")
            },
        )?;
        self.write(
            "encode-gpu",
            Chart {
                groups: vec![
                    group(
                        "ENCODE, QP 23",
                        vec![
                            self.row("encode.1080.h264.gpu", "H.264 1080P")?,
                            self.row("encode.1080.hevc.gpu", "HEVC 1080P")?,
                            self.row("encode.1440.h264.gpu", "H.264 1440P")?,
                            self.row("encode.1440.hevc.gpu", "HEVC 1440P")?,
                        ],
                    ),
                    group(
                        "TRANSCODE, FRAMES STAY ON THE GPU",
                        vec![
                            self.row("transcode.h264.1440.gpu", "H.264 1440P")?,
                            self.row("transcode.hevc.1440.gpu", "HEVC 1440P")?,
                        ],
                    ),
                ],
                suffix: fps,
                ..self.chart(
                    "GPU ENCODE AND TRANSCODE (VA-API)",
                    "FRAMES PER SECOND",
                    "higher",
                )
            },
        )?;
        let x264 = |p: &str, label: &str| -> Res<Bar> {
            let medium = format!("encode.{p}.h264.cpu.medium");
            Ok(bar(
                label,
                vec![
                    v(self.rows.num(&medium, "ours")?),
                    v(self.rows.num(&medium, "ffmpeg")?),
                    v(self
                        .rows
                        .num(&format!("encode.{p}.h264.cpu.veryfast"), "ffmpeg")?),
                ],
                Some("*"),
            ))
        };
        self.write(
            "encode-cpu",
            Chart {
                groups: vec![group("CRF 23", vec![x264("1080", "1080P")?, x264("1440", "1440P")?])],
                suffix: fps,
                parties: parties(&[US, "LIBX264 MEDIUM", "LIBX264 VERYFAST"]),
                notes: Some(vec![
                    "* LIBX264 IS HAND-WRITTEN ASSEMBLY; OUR CPU H.264 ENCODER HAS NO HAND-WRITTEN SIMD.",
                ]),
                ..self.chart("CPU H.264 ENCODE", "FRAMES PER SECOND", "higher")
            },
        )?;
        self.write(
            "first-frame",
            Chart {
                groups: vec![
                    group(
                        "CPU",
                        vec![
                            self.row("first-frame.h264.1440.cpu", "H.264")?,
                            self.row("first-frame.hevc.1440.cpu", "HEVC")?,
                        ],
                    ),
                    group(
                        "GPU",
                        vec![
                            self.row("first-frame.h264.1440.gpu", "H.264")?,
                            self.row("first-frame.hevc.1440.gpu", "HEVC")?,
                        ],
                    ),
                ],
                suffix: Some(" MS"),
                notes: Some(vec![
                    "A COLD FILE EACH TIME: OPENED, PARSED, FIRST FRAME DECODED. FFMPEG'S TIME INCLUDES STARTING ITS PROCESS.",
                ]),
                ..self.chart("TIME TO FIRST FRAME, 1440P", "MILLISECONDS", "lower")
            },
        )?;
        let mut strips = Vec::new();
        for (c, cn) in [("h264", "H.264"), ("hevc", "HEVC")] {
            let mut rows = Vec::new();
            for p in RES {
                for (k, kl) in [("cpu", "CPU"), ("auto", "GPU FIRST")] {
                    rows.push(self.row(&format!("filmstrip.{c}.{p}.{k}"), &format!("{p}P {kl}"))?);
                }
            }
            strips.push(group(cn, rows));
        }
        self.write(
            "filmstrip",
            Chart {
                groups: strips,
                notes: Some(vec![
                    "FFMPEG RUNS ONE PROCESS PER STILL, AS A FILMSTRIP WOULD CALL IT.",
                ]),
                ..self.chart(
                    "FILMSTRIP, 12 STILLS AT 240 PX",
                    "STILLS PER SECOND",
                    "higher",
                )
            },
        )?;
        let cut = |p: &str, label: &str| -> Res<Bar> {
            Ok(bar(
                label,
                vec![
                    v(self.rows.num(&format!("smartcut.{p}.copy"), "ours")?),
                    v(self.rows.num(&format!("smartcut.{p}.reencode"), "ffmpeg")?),
                    v(self.rows.num(&format!("smartcut.{p}.copy"), "ffmpeg")?),
                ],
                Some("†"),
            ))
        };
        self.write(
            "export",
            Chart {
                groups: vec![group("H.264", vec![cut("1080", "1080P")?, cut("1440", "1440P")?])],
                suffix: Some("×"),
                parties: parties(&[US, "FFMPEG RE-ENCODE", "FFMPEG STREAM COPY"]),
                notes: Some(vec![
                    "† OURS AND THE RE-ENCODE ARE FRAME-ACCURATE; A STREAM COPY ENCODES NOTHING AND CUTS ONLY ON KEYFRAMES.",
                ]),
                ..self.chart("SMART-CUT EXPORT, 3 CUTS", "TIMES REAL TIME", "higher")
            },
        )?;
        self.write(
            "scale",
            Chart {
                groups: vec![group(
                    "ALL THREADS",
                    vec![
                        self.row("scale.1440to1080", "1440P TO 1080P")?,
                        self.row("scale.1440to720", "1440P TO 720P")?,
                        self.row("scale.1080to720", "1080P TO 720P")?,
                        self.row("scale.1080to1440", "1080P TO 1440P")?,
                    ],
                )],
                suffix: fps,
                ..self.chart("SCALE, BICUBIC, 8-BIT 4:2:0", "FRAMES PER SECOND", "higher")
            },
        )?;
        self.write(
            "audio",
            Chart {
                groups: vec![
                    group(
                        "AAC",
                        vec![
                            self.row("audio.aac.decode", "DECODE")?,
                            self.row("audio.aac.encode", "ENCODE 128 KBPS")?,
                        ],
                    ),
                    group("LOUDNESS", vec![self.row("audio.r128", "EBU R128")?]),
                ],
                suffix: Some("×"),
                ..self.chart("AUDIO, 48 KHZ STEREO", "TIMES REAL TIME", "higher")
            },
        )?;
        let smart = bar(
            "SMART CUT 1440P †",
            vec![
                v(self.rows.num("smartcut.1440.reencode", "ours")?),
                v(self.rows.num("smartcut.1440.reencode", "ffmpeg")?),
            ],
            None,
        );
        self.write(
            "overview",
            Chart {
                groups: vec![
                    group(
                        "DECODE AND EXPORT",
                        vec![
                            self.inv("first-frame.h264.1440.cpu", "FIRST FRAME H.264 CPU")?,
                            self.inv("first-frame.hevc.1440.gpu", "FIRST FRAME HEVC GPU")?,
                            self.row("filmstrip.hevc.1440.cpu", "FILMSTRIP HEVC 1440P")?,
                            self.row("filmstrip.h264.1080.cpu", "FILMSTRIP H.264 1080P")?,
                            smart,
                            self.row("decode.h264.1080.gpu-readback", "H.264 1080P DECODE GPU")?,
                            self.row("decode.hevc.1080.cpu-1t", "HEVC 1080P DECODE CPU 1T")?,
                            self.row("decode.h264.1080.cpu-all", "H.264 1080P DECODE CPU")?,
                        ],
                    ),
                    group(
                        "ENCODE, FILTERS, AUDIO",
                        vec![
                            self.row("encode.1080.h264.gpu", "H.264 1080P ENCODE GPU")?,
                            self.row("encode.1080.hevc.gpu", "HEVC 1080P ENCODE GPU")?,
                            self.row("scale.1080to720", "SCALE 1080P TO 720P")?,
                            self.row("audio.r128", "EBU R128 LOUDNESS")?,
                            self.row("audio.aac.encode", "AAC ENCODE")?,
                            self.row("audio.aac.decode", "AAC DECODE")?,
                        ],
                    ),
                ],
                notes: Some(vec![
                    "† FRAME-ACCURATE SMART CUT AGAINST FFMPEG RE-ENCODING EVERY FRAME.",
                ]),
                ..self.chart(
                    "PITO-CODECS AGAINST FFMPEG",
                    "TIMES FASTER OR SLOWER",
                    "higher",
                )
            },
        )?;
        self.write(
            "overlay",
            Chart {
                groups: vec![
                    group(
                        "CAPTION BAND, 8-BIT",
                        vec![
                            fixed("1920X804", 0.73, 32.6),
                            fixed("1080P", 0.60, 25.9),
                            fixed("4K", 1.65, 135.0),
                        ],
                    ),
                    group(
                        "CAPTION BAND, 10-BIT",
                        vec![
                            fixed("1920X804", 0.51, 22.5),
                            fixed("1080P", 0.90, 29.8),
                            fixed("4K", 2.25, 125.0),
                        ],
                    ),
                    group(
                        "FULLY PAINTED, 8-BIT",
                        vec![fixed("1080P", 6.7, 46.5), fixed("4K", 20.8, 138.0)],
                    ),
                ],
                suffix: Some(" MS"),
                parties: parties(&["NOW", "1.2.1"]),
                date: NOW,
                subtitle: "RYZEN 9 9950X, ONE RUN EACH, BACK TO BACK, MACHINE AT LOAD 42 TO 61",
                notes: Some(vec!["BLENDED FRAMES ARE BIT-EXACT WITH 1.2.1."]),
                ..self.chart(
                    "CAPTION LAYER REBUILD",
                    "MILLISECONDS PER LAYER CHANGE",
                    "lower",
                )
            },
        )?;
        self.write(
            "gpu-export",
            Chart {
                groups: vec![
                    group(
                        "WALL TIME",
                        vec![
                            fixed("1920X804 130 S, MARK", 10.16, 12.80),
                            fixed("1920X804 130 S, CAPTIONS", 10.06, 12.97),
                            fixed("1080P 30 S, MARK", 2.85, 3.08),
                        ],
                    ),
                    group(
                        "CPU TIME",
                        vec![
                            fixed("1920X804 130 S, MARK", 1.9, 43.6),
                            fixed("1920X804 130 S, CAPTIONS", 2.4, 44.3),
                            fixed("1080P 30 S, MARK", 0.5, 10.2),
                        ],
                    ),
                ],
                suffix: Some(" S"),
                parties: parties(&["ON THE CARD", "CPU DECODE AND BLEND"]),
                date: NOW,
                subtitle: "RYZEN 9 9950X, RADEON RX 9060 XT (VA-API), LOAD 90",
                notes: Some(vec![
                    "BOTH ENCODE ON THE GPU; ONE RUN EACH. THE CARD'S SINGLE VIDEO ENGINE DECODES AND ENCODES IN TURN, SO WALL TIME GAINS LESS THAN CPU TIME.",
                ]),
                ..self.chart("EXPORT WITH AN OVERLAY, H.264", "SECONDS", "lower")
            },
        )
    }
}

pub fn charts(results: &Path, out: &Path) -> Res<()> {
    let rows = Rows::read(results)?;
    fs::create_dir_all(out)?;
    Docs { rows: &rows, out }.all()
}

pub fn check(new: &Path, baseline: &Path) -> Res<()> {
    let base = Rows::read(baseline)?;
    let now = Rows::read(new)?;
    say!(
        "baseline {} ({}), new {} ({})",
        base.commit,
        base.date,
        now.commit,
        now.date
    )?;
    let mut worse = Vec::new();
    for id in &base.order {
        let Some(b) = base.ours(id) else {
            continue;
        };
        let Some(n) = now.ours(id) else {
            say!("  missing  {id}")?;
            continue;
        };
        let higher = base.by_id[id]["higher_is_better"].as_bool().unwrap_or(true);
        let change = if higher { n / b - 1.0 } else { b / n - 1.0 };
        let mark = if change < -LIMIT { "slower" } else { "ok" };
        if mark == "slower" {
            worse.push(id.as_str());
        }
        say!(
            "  {mark:7}  {id:40} {b:10.1} -> {n:10.1}  {:+6.1}%",
            change * 100.0
        )?;
    }
    if !worse.is_empty() {
        return Err(format!(
            "{} rows more than 5% slower than the baseline: {}",
            worse.len(),
            worse.join(", ")
        )
        .into());
    }
    say!("bench: no row more than 5% slower than the baseline")?;
    Ok(())
}
