#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Exact(u32, u32),
    Height(u32),
    Width(u32),
}

impl Size {
    pub const FORMS: &str = "use an even width N, even WxH, or 720p, 1080p, 1440p, 2160p";

    pub fn parse(text: &str) -> Result<Size, String> {
        let number = |part: &str| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse::<u32>()
                .ok()
                .filter(|value| *value > 0 && value.is_multiple_of(2))
        };
        let size = if let Some((width, height)) = text.split_once('x') {
            number(width)
                .zip(number(height))
                .map(|(width, height)| Size::Exact(width, height))
        } else if let Some(height) = text.strip_suffix('p') {
            match height {
                "720" | "1080" | "1440" | "2160" => number(height).map(Size::Height),
                _ => None,
            }
        } else {
            number(text).map(Size::Width)
        };
        size.ok_or_else(|| format!("invalid size {text:?}: {}", Size::FORMS))
    }

    pub fn resolve(self, width: u32, height: u32) -> Result<(u32, u32), String> {
        if width == 0 || height == 0 {
            return Err(format!("invalid source shape {width}x{height}"));
        }
        match self {
            Size::Height(target_height) => {
                if target_height == 0 || !target_height.is_multiple_of(2) {
                    return Err(format!(
                        "invalid output height {target_height}: use an even height"
                    ));
                }
                let numerator = u128::from(width) * u128::from(target_height);
                let denominator = 2 * u128::from(height);
                let even_width = 2 * ((numerator + denominator / 2) / denominator);
                if even_width == 0 || even_width > u128::from(u32::MAX) {
                    return Err(format!(
                        "{width}x{height} at height {target_height} cannot make an even output width"
                    ));
                }
                Ok((even_width as u32, target_height))
            }
            Size::Width(target_width) => {
                if target_width == 0 || !target_width.is_multiple_of(2) {
                    return Err(format!(
                        "invalid output width {target_width}: use an even width"
                    ));
                }
                let numerator = u128::from(height) * u128::from(target_width);
                let denominator = 2 * u128::from(width);
                let even_height = 2 * ((numerator + denominator / 2) / denominator);
                if even_height == 0 || even_height > u128::from(u32::MAX) {
                    return Err(format!(
                        "{width}x{height} at width {target_width} cannot make an even output height"
                    ));
                }
                Ok((target_width, even_height as u32))
            }
            Size::Exact(target_width, target_height) => {
                validate_shape(width, height, target_width, target_height)
            }
        }
    }
}

fn validate_shape(
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<(u32, u32), String> {
    if target_width == 0
        || target_height == 0
        || !target_width.is_multiple_of(2)
        || !target_height.is_multiple_of(2)
    {
        return Err(format!(
            "invalid output shape {target_width}x{target_height}: both sides must be positive and even"
        ));
    }
    let actual = i128::from(target_width) * i128::from(height);
    let expected = i128::from(width) * i128::from(target_height);
    if (actual - expected).unsigned_abs() * 100 > expected as u128 {
        return Err(format!(
            "output shape {target_width}x{target_height} differs by more than 1% from source shape {width}x{height}"
        ));
    }
    Ok((target_width, target_height))
}

#[cfg(test)]
mod tests {
    use super::Size;

    #[test]
    fn parses_every_spelling() {
        for (text, expected) in [
            ("720", Size::Width(720)),
            ("160x90", Size::Exact(160, 90)),
            ("720p", Size::Height(720)),
            ("1080p", Size::Height(1080)),
            ("1440p", Size::Height(1440)),
            ("2160p", Size::Height(2160)),
        ] {
            assert_eq!(Size::parse(text), Ok(expected));
        }
    }

    #[test]
    fn rejects_invalid_spellings() {
        for text in [
            "",
            "0",
            "1",
            "321x180",
            "320x0",
            "320x181",
            "720P",
            "480p",
            "320X180",
            "320x180x2",
            "-2",
            "+2",
            "99999999999999999999",
        ] {
            let error = Size::parse(text).unwrap_err().to_string();
            assert!(error.contains("WxH") && error.contains("720p"), "{error}");
        }
    }

    #[test]
    fn resolves_shapes_without_stretching() {
        assert_eq!(Size::Height(720).resolve(1920, 1080), Ok((1280, 720)));
        assert_eq!(Size::Height(720).resolve(640, 480), Ok((960, 720)));
        assert_eq!(Size::Height(90).resolve(321, 181), Ok((160, 90)));
        assert_eq!(Size::Exact(160, 90).resolve(320, 180), Ok((160, 90)));
        assert_eq!(Size::Width(800).resolve(1280, 720), Ok((800, 450)));
        assert_eq!(Size::Width(800).resolve(1080, 1920), Ok((800, 1422)));
        assert_eq!(Size::Width(200).resolve(400, 400), Ok((200, 200)));
        assert!(Size::Width(2).resolve(4000, 10).is_err());
        let error = Size::Exact(160, 90)
            .resolve(640, 480)
            .unwrap_err()
            .to_string();
        assert!(error.contains("160x90") && error.contains("640x480"));
        assert!(Size::Height(720).resolve(0, 1080).is_err());
    }
}
