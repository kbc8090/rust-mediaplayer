fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/picture.ico");
    println!("cargo:rerun-if-changed=assets/music.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MPV_DLL_NAME");

    // libmpv is "delay-loaded": Windows loads the DLL when the player first uses
    // it rather than before the player starts. So if libmpv-2.dll is missing, the
    // player can say so itself (see main) instead of Windows refusing to start the
    // .exe with a system error. (This needs an MSVC-style mpv.lib; the build
    // workflow makes one from the DLL and sets MPV_DLL_NAME to the DLL's name.)
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dll = std::env::var("MPV_DLL_NAME").unwrap_or_else(|_| "libmpv-2.dll".to_owned());
        println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        println!("cargo:rustc-link-lib=delayimp");
        println!("cargo:rustc-env=MPV_DLL_NAME={dll}");
    }

    #[cfg(target_os = "windows")]
    {
        // Hide the console window on Windows in release builds
        println!("cargo:rustc-link-arg=/SUBSYSTEM:WINDOWS");
        println!("cargo:rustc-link-arg=/ENTRY:mainCRTStartup");

        // Embed the icons in the .exe: the app's own (also videos'), then
        // pictures' and music's (Explorer shows them for those files, and the
        // window switches to them; see taskbar::register_file_types). If the
        // resource compiler isn't available, just build without them.
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set_icon_with_id("assets/picture.ico", "2");
        res.set_icon_with_id("assets/music.ico", "3");
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
