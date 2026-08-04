const DEFAULT_PIN_NAMES: [&str; 8] = ["A", "B", "C", "D", "E", "F", "G", "H"];

fn get_pin_name(prefix: &str, idx: usize) -> String {
    DEFAULT_PIN_NAMES
        .get(idx)
        .map(|s| format!("{}_{}", prefix, s).to_string())
        .unwrap_or_else(|| format!("{}_{}", prefix, idx)) // Generate the fallback String if it doesn't
}

pub fn get_in_pin_name(idx: usize) -> String {
    get_pin_name("in", idx)
}
pub fn get_out_pin_name(idx: usize) -> String {
    get_pin_name("out", idx)
}
