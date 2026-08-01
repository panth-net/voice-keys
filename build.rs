fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Voice Keys");
        res.set("FileDescription", "Voice Keys – push-to-talk transcription");
        res.compile().expect("failed to compile Windows resources");
    }
}
