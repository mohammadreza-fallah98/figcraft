//! Windows: embed the icon and version info in figiran.exe and size its main thread's stack
//! (no-op elsewhere).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(windows_slim_errors)");
    println!("cargo::rerun-if-changed=../../assets/app-icon/figiran.ico");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let mut res = winresource::WindowsResource::new();
        let ico = "../../assets/app-icon/figiran.ico";
        if std::path::Path::new(ico).exists() {
            res.set_icon(ico);
        }
        res.set("ProductName", "FigIran");
        res.set("FileDescription", "FigIran");
        res.set("CompanyName", "FigIran");
        if let Err(e) = res.compile() {
            println!("cargo::warning=winresource: {e}");
        }
        // Windows starts the main thread with 1 MB of stack, Linux and macOS with 8 MB. Layers
        // nest up to 256 deep in an export, and that recursion needs the same room everywhere.
        let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
        println!("cargo::rustc-link-arg-bins={}", if msvc { "/STACK:8388608" } else { "-Wl,--stack,8388608" });
    }
    let sha = std::env::var("FIGIRAN_BUILD_SHA").unwrap_or_default();
    println!("cargo::rustc-env=FIGIRAN_BUILD_SHA={sha}");
    println!("cargo::rerun-if-env-changed=FIGIRAN_BUILD_SHA");
}
