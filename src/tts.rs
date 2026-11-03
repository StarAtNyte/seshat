//! Read-aloud using each OS's built-in speech engine, sentence by sentence,
//! reporting the sentence being spoken so the reader can highlight and follow it.
//!
//! * Linux: speech-dispatcher (`spd-say`), falling back to `espeak-ng`/`espeak`
//! * macOS: `say`
//! * Windows: System.Speech via one long-lived PowerShell process

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::book::{Book, Loc};

pub enum Event {
    At(Loc, usize),
    Done,
    Error(String),
}

pub struct Speaker {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Speaker {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = crate::platform::command("spd-say").arg("-C").stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn available() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        return Some("say");
    }
    #[cfg(windows)]
    {
        return Some("powershell");
    }
    #[allow(unreachable_code)]
    {
        for p in ["spd-say", "espeak-ng", "espeak"] {
            if crate::platform::which(p) {
                return Some(p);
            }
        }
        None
    }
}

/// Split a block's text into sentences: (start char, end char).
pub fn sentences(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let terminal = matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？' | ';');
        if terminal {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j], '.' | '!' | '?' | '"' | '\'' | '”' | '’' | ')' | ']' | '»') {
                j += 1;
            }
            if j >= chars.len() || chars[j].is_whitespace() || matches!(c, '。' | '！' | '？') {
                if j - start > 1 {
                    out.push((start, j));
                }
                start = j;
                while start < chars.len() && chars[start].is_whitespace() {
                    start += 1;
                }
                i = start;
                continue;
            }
        }
        // hard wrap very long runs at commas
        if i - start > 320 && (c == ',' || c == ' ') {
            out.push((start, i + 1));
            start = i + 1;
        }
        i += 1;
    }
    if start < chars.len() && chars[start..].iter().any(|c| !c.is_whitespace()) {
        out.push((start, chars.len()));
    }
    out
}

fn clean(s: &str) -> String {
    let t: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    t.trim().trim_start_matches('-').trim().to_string()
}

pub fn start(book: Arc<Book>, from: Loc, rate: f32, on: impl Fn(Event) + Send + 'static) -> Option<Speaker> {
    let engine = available()?;
    let stop = Arc::new(AtomicBool::new(false));
    let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let (stop2, child2) = (stop.clone(), child.clone());
    std::thread::spawn(move || {
        let Some(flow) = book.flow() else {
            on(Event::Done);
            return;
        };
        // Windows: one persistent synthesizer process fed line by line.
        let mut ps: Option<(std::process::ChildStdin, BufReader<std::process::ChildStdout>)> = None;
        if engine == "powershell" {
            let r = ((rate - 1.0) * 6.0).round().clamp(-10.0, 10.0) as i32;
            let script = format!(
                "[Console]::InputEncoding=[Text.Encoding]::UTF8; Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Rate={r}; while(($l=[Console]::In.ReadLine()) -ne $null){{ $s.Speak($l); [Console]::Out.WriteLine('ok'); [Console]::Out.Flush() }}"
            );
            match crate::platform::command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
                Ok(mut c) => {
                    let si = c.stdin.take();
                    let so = c.stdout.take();
                    *child2.lock().unwrap() = Some(c);
                    if let (Some(si), Some(so)) = (si, so) {
                        ps = Some((si, BufReader::new(so)));
                    }
                }
                Err(e) => {
                    on(Event::Error(format!("speech unavailable: {e}")));
                    return;
                }
            }
        }
        let mut first = true;
        for si in from.section..flow.sections.len() {
            let sec = &flow.sections[si];
            let b0 = if si == from.section { from.block } else { 0 };
            for bi in b0..sec.blocks.len() {
                let text = sec.blocks[bi].text();
                for (s, e) in sentences(&text) {
                    if first && e <= from.offset && bi == from.block && si == from.section {
                        continue;
                    }
                    first = false;
                    if stop2.load(Ordering::Relaxed) {
                        return;
                    }
                    let sentence: String = text.chars().skip(s).take(e - s).collect();
                    let sentence = clean(&sentence);
                    if sentence.chars().all(|c| !c.is_alphanumeric()) {
                        continue;
                    }
                    on(Event::At(Loc::new(si, bi, s), e));
                    let ok = match engine {
                        "powershell" => match ps.as_mut() {
                            Some((si, so)) => {
                                let _ = writeln!(si, "{sentence}");
                                let _ = si.flush();
                                let mut l = String::new();
                                so.read_line(&mut l).map(|n| n > 0).unwrap_or(false)
                            }
                            None => false,
                        },
                        _ => {
                            let mut cmd = crate::platform::command(engine);
                            match engine {
                                "spd-say" => {
                                    let r = ((rate - 1.0) * 60.0).round().clamp(-100.0, 100.0) as i32;
                                    cmd.args(["-w", "-r", &r.to_string(), "--"]);
                                }
                                "say" => {
                                    cmd.args(["-r", &((175.0 * rate) as i32).to_string(), "--"]);
                                }
                                _ => {
                                    cmd.args(["-s", &((170.0 * rate) as i32).to_string(), "--"]);
                                }
                            }
                            cmd.arg(&sentence).stdout(Stdio::null()).stderr(Stdio::null());
                            match cmd.spawn() {
                                Ok(c) => {
                                    *child2.lock().unwrap() = Some(c);
                                    // wait without holding the lock so stop() can kill it
                                    loop {
                                        if stop2.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        let mut g = child2.lock().unwrap();
                                        match g.as_mut().map(|c| c.try_wait()) {
                                            Some(Ok(Some(_))) | None => break,
                                            Some(Ok(None)) => {}
                                            Some(Err(_)) => break,
                                        }
                                        drop(g);
                                        std::thread::sleep(std::time::Duration::from_millis(25));
                                    }
                                    true
                                }
                                Err(e) => {
                                    on(Event::Error(format!("speech failed: {e}")));
                                    return;
                                }
                            }
                        }
                    };
                    if !ok && !stop2.load(Ordering::Relaxed) {
                        on(Event::Error("speech engine stopped unexpectedly".into()));
                        return;
                    }
                }
            }
        }
        on(Event::Done);
    });
    Some(Speaker { stop, child })
}
