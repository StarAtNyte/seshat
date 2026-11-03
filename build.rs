fn main() {
    // Embed the app icon into the Windows executable.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/seshat.ico");
        let _ = res.compile();
    }
}
