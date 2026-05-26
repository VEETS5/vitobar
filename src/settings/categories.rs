use crate::render::glyph_exists;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Appearance,
    Display,
    Audio,
    Network,
    Bluetooth,
    Wallpaper,
    Power,
    System,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::Appearance,
        Category::Display,
        Category::Audio,
        Category::Network,
        Category::Bluetooth,
        Category::Wallpaper,
        Category::Power,
        Category::System,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Category::Appearance => "Appearance",
            Category::Display    => "Display",
            Category::Audio      => "Audio",
            Category::Network    => "Network",
            Category::Bluetooth  => "Bluetooth",
            Category::Wallpaper  => "Wallpaper",
            Category::Power      => "Power",
            Category::System     => "System",
        }
    }

    /// Candidate icon codepoints in priority order. The first one the loaded
    /// font actually has a glyph for is used; if none exist the icon is dropped.
    fn icon_candidates(&self) -> &'static [char] {
        match self {
            // f53f (FA palette) is missing from some Nerd Font builds — try the
            // widely-present paint-brush first, then fall back to the original.
            Category::Appearance => &['\u{f1fc}', '\u{f53f}'],
            Category::Display    => &['\u{f878}', '\u{f108}'],
            Category::Audio      => &['\u{f028}'],
            Category::Network    => &['\u{f1eb}'],
            Category::Bluetooth  => &['\u{f294}'],
            Category::Wallpaper  => &['\u{f03e}', '\u{f1c5}'],
            Category::Power      => &['\u{f011}'],
            Category::System     => &['\u{f085}', '\u{f013}'],
        }
    }

    fn icon(&self) -> Option<char> {
        self.icon_candidates().iter().copied().find(|&c| glyph_exists(c))
    }

    pub fn label(&self) -> String {
        match self.icon() {
            Some(c) => format!("{}  {}", c, self.name()),
            None    => self.name().to_string(),
        }
    }
}
