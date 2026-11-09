//! Opaque RGB colors and the operations used by theme palettes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const WHITE: Self = Self::new(255, 255, 255);
    pub const BLACK: Self = Self::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#RRGGBB` (the # may be omitted).
    pub fn parse(text: &str) -> Option<Self> {
        let s = text.trim().trim_start_matches('#');
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Blend by ratio: `t` of 0 gives self, 1 gives `other`.
    pub fn mix(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let channel = |from: u8, to: u8| {
            (f64::from(from) + (f64::from(to) - f64::from(from)) * t).round() as u8
        };
        Self::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
        )
    }

    /// Blend `amount` (0 to 1) toward white.
    pub fn lighten(self, amount: f64) -> Self {
        self.mix(Self::WHITE, amount)
    }

    /// `h` is 0 to 360; `s` and `v` are 0 to 1.
    pub fn from_hsv(h: f64, s: f64, v: f64) -> Self {
        let h = h.rem_euclid(360.0);
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let c = v * s;
        let x = c * (1.0 - (h / 60.0 % 2.0 - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match (h / 60.0) as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let to_byte = |value: f64| ((value + m).clamp(0.0, 1.0) * 255.0).round() as u8;
        Self::new(to_byte(r), to_byte(g), to_byte(b))
    }

    /// Returns (H, S, V): H is 0 to 360, S and V are 0 to 1. Gray has hue 0.
    pub fn to_hsv(self) -> (f64, f64, f64) {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;
        let max = r.max(g).max(b);
        let d = max - r.min(g).min(b);
        let mut h = if d <= 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / d % 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        if h < 0.0 {
            h += 360.0;
        }
        (h, if max == 0.0 { 0.0 } else { d / max }, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(Rgb::parse("#0099FF"), Some(Rgb::new(0x00, 0x99, 0xFF)));
        assert_eq!(Rgb::parse("e37318"), Some(Rgb::new(0xE3, 0x73, 0x18)));
        for text in ["", "#FFF", "#GG0000", "+12345"] {
            assert_eq!(Rgb::parse(text), None, "{text}");
        }
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(Rgb::parse("#0A1B2C").unwrap().to_hex(), "#0A1B2C");
    }

    #[test]
    fn mix_endpoints_and_middle() {
        let a = Rgb::new(0, 100, 200);
        assert_eq!(a.mix(Rgb::WHITE, 0.0), a);
        assert_eq!(a.mix(Rgb::WHITE, 1.0), Rgb::WHITE);
        assert_eq!(a.mix(Rgb::WHITE, 0.5), Rgb::new(128, 178, 228));
        assert_eq!(a.mix(Rgb::BLACK, 0.5), Rgb::new(0, 50, 100));
    }

    #[test]
    fn lighten_is_mix_with_white() {
        let c = Rgb::new(0, 153, 255);
        assert_eq!(c.lighten(0.1), c.mix(Rgb::WHITE, 0.1));
    }

    #[test]
    fn hsv_primaries() {
        for (h, expected) in [
            (0.0, Rgb::new(255, 0, 0)),
            (120.0, Rgb::new(0, 255, 0)),
            (240.0, Rgb::new(0, 0, 255)),
            (360.0, Rgb::new(255, 0, 0)),
            (60.0, Rgb::new(255, 255, 0)),
        ] {
            assert_eq!(Rgb::from_hsv(h, 1.0, 1.0), expected);
        }
    }

    #[test]
    fn hsv_round_trip() {
        for hex in ["#0099FF", "#E37318", "#8E56DD", "#4B4B4B"] {
            let c = Rgb::parse(hex).unwrap();
            let (h, s, v) = c.to_hsv();
            assert_eq!(Rgb::from_hsv(h, s, v), c);
        }
    }

    #[test]
    fn gray_has_zero_saturation() {
        let (h, s, v) = Rgb::new(128, 128, 128).to_hsv();
        assert_eq!(h, 0.0);
        assert_eq!(s, 0.0);
        assert!((v - 128.0 / 255.0).abs() < 1e-6);
    }
}
