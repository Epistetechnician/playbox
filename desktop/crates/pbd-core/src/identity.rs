//! One place for what the product is called and how it is identified, so the app, the installer, the updater and the URL handler cannot drift.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    /// Opt-in builds that come before a stable release. Never the default.
    Canary,
}

impl Channel {
    pub fn parse(s: &str) -> Channel {
        if s.trim().eq_ignore_ascii_case("canary") {
            Channel::Canary
        } else {
            Channel::Stable
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Canary => "canary",
        }
    }
}

pub struct Product;

impl Product {
    pub const NAME: &'static str = "Playbox";
    pub const BUNDLE_ID: &'static str = "com.playbox.desktop";
    /// Invite links, `playbox://…`, open this app.
    pub const URL_SCHEME: &'static str = "playbox";
    pub const HUB_HOST: &'static str = "localhost"; // not 127.0.0.1: passkeys only work on a name
    pub const HUB_PORT: u16 = 7717;

    pub fn hub_url(port: u16) -> String {
        format!("http://{}:{port}/", Self::HUB_HOST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_defaults_to_stable() {
        assert_eq!(Channel::parse(""), Channel::Stable);
        assert_eq!(Channel::parse("nightly"), Channel::Stable);
        assert_eq!(Channel::parse(" Canary "), Channel::Canary);
    }

    #[test]
    fn the_hub_url_uses_a_name_not_an_ip() {
        assert_eq!(Product::hub_url(7717), "http://localhost:7717/");
    }
}
