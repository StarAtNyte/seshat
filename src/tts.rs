//! Read-aloud, sentence by sentence, reporting the sentence being spoken so
//! the reader can highlight and follow it.
//!
//! Two kinds of engine:
//! * the system voice: speech-dispatcher (`spd-say`) or `espeak-ng`/`espeak`
//!   on Linux, `say` on macOS, System.Speech via PowerShell on Windows;
//! * the natural voice: Kokoro, run by the optional `seshat-voice` helper
//!   (see `voice.rs`). One helper process is shared by every reader window
//!   and kept alive between sentences, so changing speed or restarting is
//!   instant, and it prepares the next sentences while one is playing.

use crate::i18n::tr;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::book::{Book, Flow, Loc};
use crate::store::AppSettings;

pub enum Event {
    At(Loc, usize),
    Done,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Engine {
    System,
    /// Kokoro speaker `voice`, or the Piper voice `pack` when set.
    Natural { voice: i32, pack: Option<&'static str> },
}

impl Engine {
    /// The engine for a book in language `lang`: with the natural voice on,
    /// Kokoro for English, an installed Piper voice for other languages, and
    /// otherwise the system voice (which at least speaks the right language).
    pub fn pick(s: &AppSettings, lang: &str) -> Engine {
        if !s.tts_natural {
            return Engine::System;
        }
        let l = crate::voice::lang2(lang);
        let english = l.as_deref().is_none_or(|l| l == "en");
        if english && crate::voice::installed() {
            return Engine::Natural { voice: s.tts_voice, pack: None };
        }
        if let Some(pack) = crate::voice::piper_for(lang) {
            return Engine::Natural { voice: 0, pack: Some(pack) };
        }
        Engine::System
    }
}

pub struct Speaker {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    engine: &'static str,
}

impl Speaker {
    pub fn stop(&self) {
        // idempotent: Drop calls this again
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        match self.engine {
            "natural" => {
                let mut act = ACTIVE.lock().unwrap();
                if act.as_ref().is_some_and(|a| Arc::ptr_eq(a, &self.stop)) {
                    *act = None;
                    let _ = helper_send(serde_json::json!({"op": "stop"}));
                }
            }
            // speech-dispatcher keeps talking after its client dies; cancel it
            // without blocking the UI thread.
            "spd-say" => {
                if let Ok(mut c) = crate::platform::command("spd-say").arg("-C").stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                    std::thread::spawn(move || {
                        let _ = c.wait();
                    });
                }
            }
            _ => {}
        }
    }
}

impl Speaker {
    /// A speaker whose work runs elsewhere (the book's own narration, see
    /// `overlay.rs`) and watches `stop`.
    pub fn external(stop: Arc<AtomicBool>) -> Speaker {
        Speaker { stop, child: Arc::new(Mutex::new(None)), engine: "overlay" }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The system speech engine, if there is one.
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
/// Whether the word just before a '.' is an abbreviation or an initial
/// ("Mr.", "Dr.", "e.g.", "J. R. R."), so the full stop doesn't end the sentence.
fn abbreviation(before: &[char]) -> bool {
    let word: String = before.iter().rev().take_while(|c| !c.is_whitespace() && !matches!(c, '(' | '"' | '“' | '\'' | '‘')).collect::<Vec<_>>().into_iter().rev().collect();
    let w = word.trim_start_matches(|c: char| !c.is_alphanumeric());
    if w.chars().count() == 1 && w.chars().all(|c| c.is_uppercase()) {
        return true;
    }
    const ABBR: &[&str] = &["mr", "mrs", "ms", "dr", "st", "jr", "sr", "prof", "rev", "gen", "col", "capt", "lt", "sgt", "hon", "mt", "vs", "etc", "e.g", "i.e", "cf", "no", "vol", "ch", "fig", "p", "pp", "approx", "ft", "messrs", "mme", "mlle", "mons"];
    ABBR.contains(&w.to_lowercase().as_str())
}

pub fn sentences(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let terminal = matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？' | ';') && !(c == '.' && abbreviation(&chars[start..i]));
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

/// Walks the book from a location, one speakable sentence at a time.
struct Cursor<'a> {
    flow: &'a Flow,
    from: Loc,
    section: usize,
    block: usize,
    text: String,
    spans: VecDeque<(usize, usize)>,
}

impl<'a> Cursor<'a> {
    fn new(flow: &'a Flow, from: Loc) -> Self {
        let mut c = Cursor { flow, from, section: from.section, block: from.block, text: String::new(), spans: VecDeque::new() };
        c.load();
        c
    }

    fn load(&mut self) {
        self.spans.clear();
        let Some(b) = self.flow.sections.get(self.section).and_then(|s| s.blocks.get(self.block)) else { return };
        self.text = b.text();
        let first = self.section == self.from.section && self.block == self.from.block;
        self.spans = sentences(&self.text).into_iter().filter(|&(_, e)| !first || e > self.from.offset).collect();
    }

    /// (where it starts, end char in the block, text to speak)
    fn next(&mut self) -> Option<(Loc, usize, String)> {
        loop {
            if let Some((s, e)) = self.spans.pop_front() {
                let sentence = clean(&self.text.chars().skip(s).take(e - s).collect::<String>());
                if sentence.chars().any(|c| c.is_alphanumeric()) {
                    return Some((Loc::new(self.section, self.block, s), e, sentence));
                }
                continue;
            }
            let sec = self.flow.sections.get(self.section)?;
            if self.block + 1 < sec.blocks.len() {
                self.block += 1;
            } else {
                self.section += 1;
                self.block = 0;
                self.flow.sections.get(self.section)?;
            }
            self.load();
        }
    }
}

pub fn start(book: Arc<Book>, from: Loc, rate: f32, engine: Engine, on: impl Fn(Event) + Send + 'static) -> Option<Speaker> {
    match engine {
        Engine::Natural { voice, pack } => Some(start_natural(book, from, rate, voice, pack, on)),
        Engine::System => start_system(book, from, rate, on),
    }
}

fn start_system(book: Arc<Book>, from: Loc, rate: f32, on: impl Fn(Event) + Send + 'static) -> Option<Speaker> {
    let engine = available()?;
    // speak in the book's language (English when unknown), never the desktop
    // locale's: an English book read with another language's rules is gibberish
    let lang = crate::voice::lang2(&book.meta.language).unwrap_or_else(|| "en".into());
    let stop = Arc::new(AtomicBool::new(false));
    let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let (stop2, child2) = (stop.clone(), child.clone());
    // Store a freshly spawned process, unless stop() already ran: then kill it
    // here, since stop() can no longer see it.
    let keep = move |c: Child| -> bool {
        let mut g = child2.lock().unwrap();
        if stop2.load(Ordering::SeqCst) {
            drop(g);
            let mut c = c;
            let _ = c.kill();
            let _ = c.wait();
            return false;
        }
        *g = Some(c);
        true
    };
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
                    if !keep(c) {
                        return;
                    }
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
        let mut cur = Cursor::new(flow, from);
        while let Some((loc, end, sentence)) = cur.next() {
            if stop2.load(Ordering::SeqCst) {
                return;
            }
            on(Event::At(loc, end));
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
                            // bare "en" can land on the first English listed (Caribbean)
                            let l = if lang == "en" { "en-US".to_string() } else { lang.clone() };
                            cmd.args(["-w", "-r", &r.to_string(), "-l", &l, "--"]);
                        }
                        "say" => {
                            cmd.args(["-r", &((175.0 * rate) as i32).to_string(), "--"]);
                        }
                        _ => {
                            // espeak: "en-us" reads English far more naturally than plain "en"
                            let voice = if lang == "en" { "en-us".to_string() } else { lang.clone() };
                            cmd.args(["-v", &voice, "-s", &((170.0 * rate) as i32).to_string(), "--"]);
                        }
                    }
                    cmd.arg(&sentence).stdout(Stdio::null()).stderr(Stdio::null());
                    match cmd.spawn() {
                        Ok(c) => {
                            if !keep(c) {
                                return;
                            }
                            // wait without holding the lock so stop() can kill it
                            loop {
                                if stop2.load(Ordering::SeqCst) {
                                    return;
                                }
                                let mut g = child2.lock().unwrap();
                                match g.as_mut().map(|c| c.try_wait()) {
                                    Some(Ok(Some(_))) | None => {
                                        g.take();
                                        break;
                                    }
                                    Some(Ok(None)) => {}
                                    Some(Err(_)) => break,
                                }
                                drop(g);
                                std::thread::sleep(Duration::from_millis(25));
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
            if !ok && !stop2.load(Ordering::SeqCst) {
                on(Event::Error("speech engine stopped unexpectedly".into()));
                return;
            }
        }
        if !stop2.load(Ordering::SeqCst) {
            on(Event::Done);
        }
    });
    Some(Speaker { stop, child, engine })
}

// ---------- natural voice (seshat-voice helper) ----------

enum HelperEv {
    Started(u64),
    Done(u64),
    Fatal(String),
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    alive: Arc<AtomicBool>,
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
/// Where the helper's events go: the speaker that is currently reading.
static SUB: Mutex<Option<mpsc::Sender<HelperEv>>> = Mutex::new(None);
/// The current natural-voice speaker's stop flag. Starting another one (from
/// another window, or a preview) ends it cleanly instead of leaving it hanging.
static ACTIVE: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn to_sub(ev: HelperEv) {
    if let Some(tx) = SUB.lock().unwrap().as_ref() {
        let _ = tx.send(ev);
    }
}

fn ensure_helper(h: &mut Option<Helper>) -> Result<(), String> {
    if h.as_ref().is_some_and(|h| h.alive.load(Ordering::SeqCst)) {
        return Ok(());
    }
    if let Some(mut old) = h.take() {
        let _ = old.child.kill();
        let _ = old.child.wait();
    }
    let exe = crate::voice::helper().ok_or(tr("The natural voice is not installed."))?;
    let mut child = crate::platform::command(&exe.to_string_lossy())
        .arg("serve")
        .arg(crate::voice::root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| trf!("Could not start the natural voice: {err}", err = e))?;
    let stdin = child.stdin.take().ok_or("no pipe to the voice helper")?;
    let stdout = child.stdout.take().ok_or("no pipe from the voice helper")?;
    let alive = Arc::new(AtomicBool::new(true));
    let alive2 = alive.clone();
    std::thread::spawn(move || {
        let mut fatal = tr("The natural voice stopped unexpectedly.").to_string();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let id = v["id"].as_u64();
            match (v["ev"].as_str(), id) {
                (Some("started"), Some(id)) => to_sub(HelperEv::Started(id)),
                (Some("done"), Some(id)) => to_sub(HelperEv::Done(id)),
                // per-sentence errors are followed by "done"; fatal ones have no id
                (Some("error"), None) => fatal = v["msg"].as_str().unwrap_or(&fatal).to_string(),
                _ => {}
            }
        }
        alive2.store(false, Ordering::SeqCst);
        to_sub(HelperEv::Fatal(fatal));
    });
    *h = Some(Helper { child, stdin, alive });
    Ok(())
}

fn helper_send(v: serde_json::Value) -> Result<(), String> {
    let mut h = HELPER.lock().unwrap();
    ensure_helper(&mut h)?;
    let h = h.as_mut().unwrap();
    writeln!(h.stdin, "{v}").and_then(|_| h.stdin.flush()).map_err(|_| {
        h.alive.store(false, Ordering::SeqCst);
        tr("The natural voice stopped unexpectedly.").to_string()
    })
}

/// Stop the helper (before removing the voice, or on exit).
pub fn shutdown_helper() {
    if let Some(a) = ACTIVE.lock().unwrap().take() {
        a.store(true, Ordering::SeqCst);
    }
    if let Some(mut h) = HELPER.lock().unwrap().take() {
        let _ = h.child.kill();
        let _ = h.child.wait();
    }
}

/// Take over the helper: end whichever speaker had it and flush its audio.
fn claim(stop: &Arc<AtomicBool>, tx: mpsc::Sender<HelperEv>) {
    let mut act = ACTIVE.lock().unwrap();
    if let Some(old) = act.replace(stop.clone()) {
        old.store(true, Ordering::SeqCst);
    }
    *SUB.lock().unwrap() = Some(tx);
    let _ = helper_send(serde_json::json!({"op": "stop"}));
}

/// Say a sample sentence, interrupting any reading: in Kokoro speaker
/// `voice`, or in the Piper voice `pack` (with a sample in its language).
pub fn preview(voice: i32, pack: Option<&str>, rate: f32) -> Result<(), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, _rx) = mpsc::channel();
    claim(&stop, tx);
    let text = match pack {
        None => format!("Hello. This is {}, reading to you from Seshat.", crate::voice::voice_label(voice).split(" · ").next().unwrap_or("Seshat")),
        Some(p) => sample_text(p).to_string(),
    };
    helper_send(serde_json::json!({"op": "speak", "id": NEXT_ID.fetch_add(1, Ordering::SeqCst), "text": text, "voice": voice, "speed": rate, "pack": pack.unwrap_or("")}))
}

/// A short "hello, I'm reading to you" in the Piper voice's language.
fn sample_text(pack: &str) -> &'static str {
    match &pack[..2] {
        "de" => "Hallo. Ich lese Ihnen aus Seshat vor.",
        "fr" => "Bonjour. Je vous lis ce livre avec Seshat.",
        "es" => "Hola. Te leo este libro desde Seshat.",
        "it" => "Ciao. Ti leggo questo libro con Seshat.",
        "pt" => "Olá. Eu leio este livro para você no Seshat.",
        "nl" => "Hallo. Ik lees dit boek voor vanuit Seshat.",
        "ru" => "Здравствуйте. Я читаю вам эту книгу в Seshat.",
        "uk" => "Вітаю. Я читаю вам цю книжку в Seshat.",
        "pl" => "Dzień dobry. Czytam ci tę książkę w Seshat.",
        "cs" => "Dobrý den. Čtu vám tuto knihu v Seshatu.",
        "sv" => "Hej. Jag läser den här boken för dig i Seshat.",
        "tr" => "Merhaba. Bu kitabı size Seshat ile okuyorum.",
        "zh" => "你好。我正在用 Seshat 为你朗读这本书。",
        "ar" => "مرحبا. أقرأ لك هذا الكتاب في سيشات.",
        "hi" => "नमस्ते। मैं सेशात में आपके लिए यह किताब पढ़ रहा हूँ।",
        "fa" => "سلام. من این کتاب را در سشات برای شما می‌خوانم.",
        "vi" => "Xin chào. Tôi đang đọc cuốn sách này cho bạn trong Seshat.",
        "el" => "Γεια σας. Σας διαβάζω αυτό το βιβλίο στο Seshat.",
        "fi" => "Hei. Luen tämän kirjan sinulle Seshatissa.",
        "hu" => "Szia. Ezt a könyvet olvasom fel neked a Seshatban.",
        "ro" => "Bună. Îți citesc această carte în Seshat.",
        "da" => "Hej. Jeg læser denne bog op for dig i Seshat.",
        "no" => "Hei. Jeg leser denne boken for deg i Seshat.",
        _ => "Seshat.",
    }
}

fn start_natural(book: Arc<Book>, from: Loc, rate: f32, voice: i32, pack: Option<&'static str>, on: impl Fn(Event) + Send + 'static) -> Speaker {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    claim(&stop, tx);
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        let Some(flow) = book.flow() else {
            on(Event::Done);
            return;
        };
        // Keep a few sentences queued so the next one is ready when this one ends.
        const AHEAD: usize = 3;
        let mut cur = Cursor::new(flow, from);
        let mut queued: VecDeque<(u64, Loc, usize)> = VecDeque::new();
        loop {
            if stop2.load(Ordering::SeqCst) {
                // stopped by its own Stop, or because another speaker took over
                if !ACTIVE.lock().unwrap().as_ref().is_some_and(|a| Arc::ptr_eq(a, &stop2)) {
                    on(Event::Done);
                }
                return;
            }
            while queued.len() < AHEAD {
                let Some((loc, end, text)) = cur.next() else { break };
                let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
                if let Err(e) = helper_send(serde_json::json!({"op": "speak", "id": id, "text": text, "voice": voice, "speed": rate, "pack": pack.unwrap_or("")})) {
                    on(Event::Error(e));
                    return;
                }
                queued.push_back((id, loc, end));
            }
            if queued.is_empty() {
                let mut act = ACTIVE.lock().unwrap();
                if act.as_ref().is_some_and(|a| Arc::ptr_eq(a, &stop2)) {
                    *act = None;
                }
                on(Event::Done);
                return;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(HelperEv::Started(id)) => {
                    if let Some(&(_, loc, end)) = queued.iter().find(|q| q.0 == id) {
                        on(Event::At(loc, end));
                    }
                }
                Ok(HelperEv::Done(id)) => queued.retain(|q| q.0 != id),
                Ok(HelperEv::Fatal(e)) => {
                    if !stop2.load(Ordering::SeqCst) {
                        on(Event::Error(e));
                    }
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    Speaker { stop, child: Arc::new(Mutex::new(None)), engine: "natural" }
}

#[cfg(test)]
mod tests {
    use super::sentences;

    fn split(t: &str) -> Vec<String> {
        sentences(t).into_iter().map(|(s, e)| t.chars().skip(s).take(e - s).collect::<String>().trim().to_string()).collect()
    }

    #[test]
    fn keeps_abbreviations_and_initials_together() {
        assert_eq!(split("Mr. Bennet replied that he had not. Dr. Watson, e.g. him, arrived."), vec!["Mr. Bennet replied that he had not.", "Dr. Watson, e.g. him, arrived."]);
        assert_eq!(split("J. R. R. Tolkien wrote it. Then he slept."), vec!["J. R. R. Tolkien wrote it.", "Then he slept."]);
    }

    #[test]
    fn still_splits_ordinary_sentences() {
        assert_eq!(split("It rained. \"Why?\" she asked! I. Am. Here."), vec!["It rained.", "\"Why?\"", "she asked!", "I. Am.", "Here."]);
    }
}
