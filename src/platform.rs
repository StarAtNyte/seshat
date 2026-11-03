//! OS integration: opening files/folders/URLs and spawning helper processes
//! without flashing console windows on Windows.

use std::path::Path;
use std::process::Command;

pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub fn open_path(path: &Path) {
    #[cfg(target_os = "linux")]
    let _ = command("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = command("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = command("explorer").arg(path).spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let _ = command("xdg-open").arg(path).spawn();
}

pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = command("open").arg("-R").arg(path).spawn();
        return;
    }
    #[cfg(windows)]
    {
        let _ = command("explorer").arg(format!("/select,{}", path.display())).spawn();
        return;
    }
    #[allow(unreachable_code)]
    {
        open_path(path.parent().unwrap_or(Path::new(".")));
    }
}

pub fn open_url(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = command("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = command("open").arg(url).spawn();
    #[cfg(windows)]
    let _ = command("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let _ = command("xdg-open").arg(url).spawn();
}

pub fn which(program: &str) -> bool {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| exts.iter().any(|e| d.join(format!("{program}{e}")).is_file())))
        .unwrap_or(false)
}
