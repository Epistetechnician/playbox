//! Persisted window geometry (`window.json` in the app's data folder).
//!
//! Remembers size, position and maximized state across launches, clamped so an unplugged external monitor or corrupt file
//! never puts the window at a zero size or millions of pixels off-screen.

use std::io::{self, Write};
use std::path::Path;

const FILE: &str = "window.json";
pub const MIN_WIDTH: f64 = 420.0;
pub const MIN_HEIGHT: f64 = 520.0;
pub const DEFAULT_WIDTH: f64 = 1280.0;
pub const DEFAULT_HEIGHT: f64 = 840.0;
const MAX_WIDTH: f64 = 6000.0;
const MAX_HEIGHT: f64 = 4000.0;
const MIN_POS: i32 = -8000;
const MAX_POS: i32 = 16000;

#[derive(Clone, Debug, PartialEq)]
pub struct WindowBounds {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

impl Default for WindowBounds {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            maximized: false,
        }
    }
}

impl WindowBounds {
    pub fn clamped(mut self) -> Self {
        if !self.width.is_finite() {
            self.width = DEFAULT_WIDTH;
        }
        if !self.height.is_finite() {
            self.height = DEFAULT_HEIGHT;
        }
        self.width = self.width.clamp(MIN_WIDTH, MAX_WIDTH);
        self.height = self.height.clamp(MIN_HEIGHT, MAX_HEIGHT);
        if let (Some(x), Some(y)) = (self.x, self.y) {
            if (MIN_POS..=MAX_POS).contains(&x) && (MIN_POS..=MAX_POS).contains(&y) {
                self.x = Some(x);
                self.y = Some(y);
            } else {
                self.x = None;
                self.y = None;
            }
        } else {
            self.x = None;
            self.y = None;
        }
        self
    }

    pub fn load(data_dir: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(data_dir.join(FILE)) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let width = v
            .get("width")
            .and_then(|x| x.as_f64())
            .unwrap_or(DEFAULT_WIDTH);
        let height = v
            .get("height")
            .and_then(|x| x.as_f64())
            .unwrap_or(DEFAULT_HEIGHT);
        let x = v
            .get("x")
            .and_then(|n| n.as_i64())
            .and_then(|n| i32::try_from(n).ok());
        let y = v
            .get("y")
            .and_then(|n| n.as_i64())
            .and_then(|n| i32::try_from(n).ok());
        let maximized = v
            .get("maximized")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        Self {
            x,
            y,
            width,
            height,
            maximized,
        }
        .clamped()
    }

    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let c = self.clone().clamped();
        let json = serde_json::json!({
            "x": c.x,
            "y": c.y,
            "width": c.width,
            "height": c.height,
            "maximized": c.maximized,
        });
        let bytes = serde_json::to_vec(&json).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = data_dir.join(format!(".{FILE}.tmp.{}", std::process::id()));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        let dst = data_dir.join(FILE);
        if let Err(e) = std::fs::rename(&tmp, &dst) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_clamps_out_of_range_geometry() {
        let dir = std::env::temp_dir().join(format!("pbd-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let b = WindowBounds {
            x: Some(120),
            y: Some(80),
            width: 1440.0,
            height: 920.0,
            maximized: true,
        };
        b.save(&dir).unwrap();
        assert_eq!(WindowBounds::load(&dir), b);

        let tiny = WindowBounds {
            x: Some(999_999),
            y: Some(10),
            width: 50.0,
            height: f64::NAN,
            maximized: false,
        }
        .clamped();
        assert_eq!(tiny.width, MIN_WIDTH);
        assert_eq!(tiny.height, DEFAULT_HEIGHT);
        assert_eq!(tiny.x, None);
        assert_eq!(tiny.y, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
