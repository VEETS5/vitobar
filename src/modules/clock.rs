use chrono::Local;

pub fn get_time_string() -> String {
    Local::now().format("%a %d %b  %H:%M").to_string()
}
