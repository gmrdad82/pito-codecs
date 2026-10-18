use super::json::Json;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub integrated: f64,
    pub range: f64,
    pub true_peak: f64,
    pub threshold: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub path: String,
    pub sha256: String,
    pub loudness: Option<Measured>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Encoder {
    pub encoder: String,
    pub codec: Option<String>,
    pub settings: Vec<(&'static str, Json)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quality {
    Crf(u32),
    Qp(u32),
    Webp(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub integrated: f64,
    pub true_peak: f64,
    pub mode: String,
    pub linear: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub encoder: String,
    pub codec: Option<String>,
    pub kbps: Option<u32>,
    pub layout: Option<String>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub profile: String,
    pub encoder: Encoder,
    pub quality: Option<Quality>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub tune: Option<String>,
    pub loudness: Option<Target>,
    pub audio: Option<Audio>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    pub loudness: Option<Measured>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub method: String,
    pub reason: String,
    pub clips: usize,
    pub loudness: Option<Measured>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub sources: Vec<Source>,
    pub params: Params,
    pub device: String,
    pub seconds: f64,
    pub files: Vec<Output>,
    pub join: Option<Join>,
}

impl Measured {
    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("integrated", self.integrated.into()),
            ("range", self.range.into()),
            ("true_peak", self.true_peak.into()),
            ("threshold", self.threshold.into()),
        ])
    }
}

fn measured(m: Option<Measured>) -> Json {
    m.map_or(Json::Null, |m| m.json())
}

impl Encoder {
    pub fn json(&self) -> Json {
        let mut fields = vec![("encoder", self.encoder.as_str().into())];
        if let Some(codec) = &self.codec {
            fields.push(("codec", codec.as_str().into()));
        }
        fields.extend(self.settings.iter().cloned());
        Json::Obj(fields)
    }
}

impl Quality {
    pub fn json(self) -> Json {
        let (key, value) = match self {
            Quality::Crf(v) => ("crf", v),
            Quality::Qp(v) => ("qp", v),
            Quality::Webp(v) => ("quality", v),
        };
        Json::Obj(vec![(key, value.into())])
    }
}

impl Audio {
    pub fn json(&self) -> Json {
        let mut fields = vec![("encoder", self.encoder.as_str().into())];
        if let Some(codec) = &self.codec {
            fields.push(("codec", codec.as_str().into()));
        }
        if let Some(kbps) = self.kbps {
            fields.push(("kbps", kbps.into()));
        }
        if let Some(layout) = &self.layout {
            fields.push(("layout", layout.as_str().into()));
        }
        if let Some(rate) = self.sample_rate {
            fields.push(("sample_rate", rate.into()));
        }
        Json::Obj(fields)
    }
}

impl Params {
    pub fn json(&self) -> Json {
        Json::Obj(vec![
            ("profile", self.profile.as_str().into()),
            ("encoder", self.encoder.json()),
            ("quality", self.quality.map_or(Json::Null, Quality::json)),
            (
                "size",
                Json::Obj(vec![
                    ("width", self.width.into()),
                    ("height", self.height.into()),
                ]),
            ),
            ("fps", self.fps.into()),
            ("tune", self.tune.as_deref().into()),
            (
                "loudness",
                self.loudness.as_ref().map_or(Json::Null, |t| {
                    Json::Obj(vec![
                        ("integrated", t.integrated.into()),
                        ("true_peak", t.true_peak.into()),
                        ("mode", t.mode.as_str().into()),
                        ("linear", Json::Bool(t.linear)),
                    ])
                }),
            ),
            ("audio", self.audio.as_ref().map_or(Json::Null, Audio::json)),
        ])
    }
}

impl Manifest {
    pub fn to_json(&self) -> String {
        let mut fields = vec![(
            "tool",
            Json::Obj(vec![
                ("name", "pcod".into()),
                ("version", env!("CARGO_PKG_VERSION").into()),
            ]),
        )];
        match &self.join {
            None => {
                let source = self.sources.first().map_or(Json::Null, |s| {
                    Json::Obj(vec![
                        ("path", s.path.as_str().into()),
                        ("sha256", s.sha256.as_str().into()),
                        ("loudness", measured(s.loudness)),
                    ])
                });
                fields.push(("source", source));
            }
            Some(_) => {
                let sources = self
                    .sources
                    .iter()
                    .map(|s| {
                        let mut o = vec![
                            ("path", s.path.as_str().into()),
                            ("sha256", s.sha256.as_str().into()),
                        ];
                        if let Some(m) = s.loudness {
                            o.push(("loudness", m.json()));
                        }
                        Json::Obj(o)
                    })
                    .collect();
                fields.push(("sources", Json::Arr(sources)));
            }
        }
        fields.push(("params", self.params.json()));
        fields.push(("device", self.device.as_str().into()));
        fields.push(("seconds", self.seconds.into()));
        let files = self
            .files
            .iter()
            .map(|f| {
                Json::Obj(vec![
                    ("file", f.file.as_str().into()),
                    ("sha256", f.sha256.as_str().into()),
                    ("bytes", f.bytes.into()),
                    ("loudness", measured(f.loudness)),
                ])
            })
            .collect();
        fields.push(("files", Json::Arr(files)));
        if let Some(j) = &self.join {
            fields.push((
                "join",
                Json::Obj(vec![
                    ("method", j.method.as_str().into()),
                    ("reason", j.reason.as_str().into()),
                    ("clips", j.clips.into()),
                    ("loudness", measured(j.loudness)),
                ]),
            ));
        }
        Json::Obj(fields).text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_keeps_its_json_shape() {
        let loud = Measured {
            integrated: -14.21,
            range: 0.2,
            true_peak: -2.58,
            threshold: -24.21,
        };
        let mut m = Manifest {
            sources: vec![Source {
                path: "talk \"1\".mp4".into(),
                sha256: "ab".into(),
                loudness: Some(loud),
            }],
            params: Params {
                profile: "balanced".into(),
                encoder: Encoder {
                    encoder: "h264".into(),
                    codec: Some("h264".into()),
                    settings: vec![("crf", 20u32.into())],
                },
                quality: Some(Quality::Crf(20)),
                width: 1280,
                height: 720,
                fps: Some(30.0),
                tune: None,
                loudness: Some(Target {
                    integrated: -14.0,
                    true_peak: -2.0,
                    mode: "linear".into(),
                    linear: false,
                }),
                audio: Some(Audio {
                    encoder: "aac".into(),
                    codec: None,
                    kbps: Some(320),
                    layout: Some("stereo".into()),
                    sample_rate: Some(48000),
                }),
            },
            device: "CPU".into(),
            seconds: 0.93,
            files: vec![Output {
                file: "upload.mp4".into(),
                sha256: "cd".into(),
                bytes: 2259013,
                loudness: Some(loud),
            }],
            join: None,
        };
        let v: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(v["tool"]["name"], "pcod");
        assert_eq!(v["source"]["path"], "talk \"1\".mp4");
        assert_eq!(v["source"]["loudness"]["true_peak"], -2.58);
        assert_eq!(v["params"]["encoder"]["encoder"], "h264");
        assert_eq!(v["params"]["quality"]["crf"], 20);
        assert_eq!(v["params"]["size"]["width"], 1280);
        assert_eq!(v["params"]["fps"], 30.0);
        assert!(v["params"]["tune"].is_null());
        assert_eq!(v["params"]["loudness"]["mode"], "linear");
        assert_eq!(v["params"]["audio"]["sample_rate"], 48000);
        assert_eq!(v["files"][0]["bytes"], 2259013);
        assert!(v.get("join").is_none());
        m.sources.push(m.sources[0].clone());
        m.join = Some(Join {
            method: "copy".into(),
            reason: "every clip matches".into(),
            clips: 2,
            loudness: None,
        });
        m.seconds = f64::NAN;
        let v: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        assert!(v.get("source").is_none());
        assert_eq!(v["sources"].as_array().map(Vec::len), Some(2));
        assert_eq!(v["join"]["clips"], 2);
        assert!(v["seconds"].is_null());
    }
}
