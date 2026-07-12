pub const APP_NAME: &str = "repartee";

pub fn storage_key(suffix: &str) -> String {
    format!("{APP_NAME}-{suffix}")
}
