use std::fmt::Write as _;

pub fn quoted(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    push_quoted(&mut o, s);
    o
}

pub fn push_quoted(o: &mut String, s: &str) {
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
}

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    pub fn text(&self) -> String {
        let mut o = String::new();
        self.write(&mut o);
        o
    }

    pub fn write(&self, o: &mut String) {
        match self {
            Json::Null => o.push_str("null"),
            Json::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => {
                let _ = write!(o, "{n}");
            }
            Json::Num(x) => push_number(o, *x),
            Json::Str(s) => push_quoted(o, s),
            Json::Arr(items) => {
                o.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        o.push(',');
                    }
                    item.write(o);
                }
                o.push(']');
            }
            Json::Obj(fields) => {
                o.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        o.push(',');
                    }
                    push_quoted(o, key);
                    o.push(':');
                    value.write(o);
                }
                o.push('}');
            }
        }
    }
}

fn push_number(o: &mut String, x: f64) {
    if !x.is_finite() {
        o.push_str("null");
    } else if x == x.trunc() && x.abs() < 1e15 {
        let _ = write!(o, "{x:.1}");
    } else {
        let _ = write!(o, "{x}");
    }
}

impl From<bool> for Json {
    fn from(v: bool) -> Self {
        Json::Bool(v)
    }
}

impl From<u8> for Json {
    fn from(v: u8) -> Self {
        Json::Int(v.into())
    }
}

impl From<u16> for Json {
    fn from(v: u16) -> Self {
        Json::Int(v.into())
    }
}

impl From<u32> for Json {
    fn from(v: u32) -> Self {
        Json::Int(v.into())
    }
}

impl From<i64> for Json {
    fn from(v: i64) -> Self {
        Json::Int(v)
    }
}

impl From<u64> for Json {
    fn from(v: u64) -> Self {
        i64::try_from(v).map_or(Json::Num(v as f64), Json::Int)
    }
}

impl From<usize> for Json {
    fn from(v: usize) -> Self {
        Json::from(v as u64)
    }
}

impl From<f64> for Json {
    fn from(v: f64) -> Self {
        Json::Num(v)
    }
}

impl From<&str> for Json {
    fn from(v: &str) -> Self {
        Json::Str(v.to_string())
    }
}

impl From<String> for Json {
    fn from(v: String) -> Self {
        Json::Str(v)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Self {
        v.map_or(Json::Null, Into::into)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Self {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}
