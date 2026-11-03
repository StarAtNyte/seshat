# zlib-reader

A lightweight native **ebook reader** with Z-Library search built in — find, download, and read books with your own UI, **no webview, no Chromium** (~15 MB binary vs the 872 MB Electron app).

Built in Rust with [egui](https://github.com/emilk/egui); talks directly to the Z-Library EAPI.

## Features

### Two search mechanisms

- 📚 **Library search** — instantly filter your local collection by title/author (scans your download folder; EPUB metadata indexed in the background)
- 🌐 **Z-Library search** — find new books online: title/author/ISBN; filters: exact, year range, format (EPUB/PDF/MOBI/…), language, sort order; paginated

### Reader

- 📖 **Built-in EPUB reader** — chapter navigation (⏮ ◀ ▶ + chapter dropdown), adjustable font size (A- / A+), remembers your reading position per book
- ⬇️ **One-click download** — with progress bar; atomic file writes; "Read now" button on completed downloads
- 🔌 **Auto mirror discovery** — probes known EAPI mirrors at startup, first reachable wins
- 🔑 **Sign in / register** — email/password, cookie import fallback (`remix_userid` / `remix_userkey`), or create a new account with emailed confirmation code
- 🕶 **Optional proxy** — SOCKS5/HTTP (e.g. Tor: `socks5://127.0.0.1:9050`)

## Build & run

```sh
cargo run --release
```

Linux requires the usual WebKit/GTK-free GUI deps only (this app uses egui, so just X11/Wayland headers which most distros ship).

Session and settings are stored in `~/.config/zlib-reader/`.

## How it works (protocol)

The official Z-Library desktop app (Electron 30.5.1, v3.2.1) was statically reverse-engineered
(REA + direct source inspection of `app.asar`) and cross-checked against the
[heartleo/zlib](https://github.com/heartleo/zlib) CLI. The desktop app is a thin webview wrapper
around the Z-Library site plus this JSON EAPI, which we use directly:

| Purpose | Endpoint |
|---|---|
| Mirror health check | `GET /eapi/info/ok` |
| Login | `POST /eapi/user/login` (form: `email`, `password`) → `user.id`, `user.remix_userkey` |
| Register | `POST /eapi/user/registration` (form: `email`, `password`, `name`) |
| Confirm email code | `POST /eapi/user/email/confirmation` (…, `code`) → session credentials |
| Resend code | `POST /eapi/user/email/confirmation/resend` |
| Search | `POST /eapi/book/search` (form: `message`, `page`, `limit`, `e`, `yearFrom`, `yearTo`, `languages[]`, `extensions[]`, `order`) |
| Book details | `GET /eapi/book/{id}/{hash}` |
| Download link | `GET /eapi/book/{id}/{hash}/file` → `file.downloadLink` |

Auth cookies: `remix_userid` + `remix_userkey` (set server-side via the web UI, or returned by EAPI login).
Mirrors share account sessions; EAPI-capable mirrors (verified live: `z-lib.gd`, `1lib.sk`,
`z-library.sk`) serve the API, while `z-lib.ws` / `z-lib.mx` are web-UI-only mirrors of the same service.

## Disclaimer

For personal use with your own Z-Library account. This project is not affiliated with Z-Library. Respect copyright and your local laws.

## Sync

Keep the GitHub private repo up to date:

```sh
./sync.sh          # commit + push (private repo, StarAtNyte/zlib-reader)
```
