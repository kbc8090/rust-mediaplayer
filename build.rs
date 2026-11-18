fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "windows")]
    {
        // Hide the console window on Windows in release builds
        println!("cargo:rustc-link-arg=/SUBSYSTEM:WINDOWS");
        println!("cargo:rustc-link-arg=/ENTRY:mainCRTStartup");

        // Embed the app icon in the .exe (Explorer, taskbar pins, Alt+Tab).
        // If the resource compiler isn't available, just build without it.
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // What Task Manager and the file's Properties dialog show
        res.set("FileDescription", "Rust Media Player");
        res.set("ProductName", "Rust Media Player");
        res.set("InternalName", "RustMediaPlayer");
        res.set("OriginalFilename", "RustMediaPlayer.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
