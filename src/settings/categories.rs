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

    pub fn label(&self) -> &'static str {
        match self {
            Category::Appearance => "\u{f53f}  Appearance",
            Category::Display    => "\u{f878}  Display",
            Category::Audio      => "\u{f028}  Audio",
            Category::Network    => "\u{f1eb}  Network",
            Category::Bluetooth  => "\u{f294}  Bluetooth",
            Category::Wallpaper  => "\u{f03e}  Wallpaper",
            Category::Power      => "\u{f011}  Power",
            Category::System     => "\u{f085}  System",
        }
    }
}
