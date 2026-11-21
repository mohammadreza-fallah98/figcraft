//! Windows: make `figiran:` links (the company portal's login and its "open in FigIran" buttons) start this program.
//!
//! The registration is per user (`HKEY_CURRENT_USER\Software\Classes\figiran`, no administrator rights) and is written
//! when the program starts, so a copy that was just unpacked or downloaded works without an installer. It is
//! written again when it points somewhere else (the program was moved or replaced), so running the program
//! once from its new place repairs the links. An installer that registers the same key is not disturbed: the
//! values are only changed when the command differs.
//!
//! Nothing is registered by a development build (a Cargo `target` folder, so `cargo run` doesn't take the
//! links over) or when `FIGIRAN_NO_PROTOCOL_REGISTRATION` is set.

/// The environment variable that turns the registration off.
pub const OPT_OUT_ENV: &str = "FIGIRAN_NO_PROTOCOL_REGISTRATION";

/// The command Windows runs for a `figiran:` link: this program with the link as its argument.
pub fn open_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// The icon entry: the program's own icon.
pub fn icon_value(exe: &str) -> String {
    format!("\"{exe}\",0")
}

/// A program running from a Cargo build folder is a development build.
pub fn is_development_build(exe: &str) -> bool {
    let lower = exe.replace('/', "\\").to_ascii_lowercase();
    lower.contains("\\target\\debug\\") || lower.contains("\\target\\release\\deps\\")
}

/// Whether this run should leave the registration alone.
pub fn skip(exe: &str, opted_out: bool) -> bool {
    opted_out || is_development_build(exe)
}

/// Register this program for `figiran:` links (once per start). Failures are logged and otherwise ignored: the program works
/// without the registration, only the browser buttons that hand a link over don't.
#[cfg(windows)]
pub fn register() {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            log::warn!("figiran: links aren't registered: the program's path is unknown ({e})");
            return;
        }
    };
    let Some(exe) = exe.to_str() else {
        log::warn!("figiran: links aren't registered: the program's path isn't valid text");
        return;
    };
    if skip(exe, std::env::var_os(OPT_OUT_ENV).is_some()) {
        return;
    }
    if let Err(e) = write(exe) {
        log::warn!("figiran: links aren't registered ({e})");
    }
}

#[cfg(windows)]
const KEY: &str = r"Software\Classes\figiran";

/// Write the registration unless the command already points at `exe`.
#[cfg(windows)]
fn write(exe: &str) -> winsafe::SysResult<()> {
    use winsafe::{HKEY, RegistryValue, co, prelude::*};

    let command = open_command(exe);
    let command_key = format!(r"{KEY}\shell\open\command");
    let current = HKEY::CURRENT_USER
        .RegOpenKeyEx(Some(&command_key), co::REG_OPTION::default(), co::KEY::READ)
        .and_then(|key| key.RegQueryValueEx(None));
    if matches!(&current, Ok(RegistryValue::Sz(existing)) if *existing == command) {
        return Ok(());
    }

    let set = |sub_key: &str, name: Option<&str>, value: String| -> winsafe::SysResult<()> {
        let (key, _) = HKEY::CURRENT_USER.RegCreateKeyEx(sub_key, None, co::REG_OPTION::NON_VOLATILE, co::KEY::WRITE, None)?;
        key.RegSetValueEx(name, RegistryValue::Sz(value))
    };
    set(KEY, None, "URL:FigIran Protocol".to_string())?;
    set(KEY, Some("URL Protocol"), String::new())?;
    set(&format!(r"{KEY}\DefaultIcon"), None, icon_value(exe))?;
    set(&command_key, None, command)
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
