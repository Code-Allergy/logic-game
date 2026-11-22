//! The bundled fonts, embedded so native and web builds need no font
//! files at run time. Licences: `assets/fonts/OFL-*.txt` (SIL Open Font
//! License 1.1).
//!
//! - Nunito SemiBold: egui body text.
//! - Nunito ExtraBold: names, titles, buttons, and canvas labels.
//! - JetBrains Mono Bold: values (egui monospace, canvas values).

pub const NUNITO_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Nunito-SemiBold.ttf");
pub const NUNITO_EXTRABOLD: &[u8] = include_bytes!("../assets/fonts/Nunito-ExtraBold.ttf");
pub const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");
