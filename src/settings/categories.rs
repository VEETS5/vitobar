#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Display,
    Audio,
    Bluetooth,
    Network,
    Power,
    NixOS,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::Display,
        Category::Audio,
        Category::Bluetooth,
        Category::Network,
        Category::Power,
        Category::NixOS,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Category::Display   => "\u{f878} Display",
            Category::Audio     => "\u{f028} Audio",
            Category::Bluetooth => "\u{f294} Bluetooth",
            Category::Network   => "\u{f1eb} Network",
            Category::Power     => "\u{f011} Power",
            Category::NixOS     => "\u{f313} NixOS",
        }
    }
}
