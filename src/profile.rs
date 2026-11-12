//! `SONOMORI_LOAD`, `SONOMORI_PLAY`, `SONOMORI_READY`, and `SONOMORI_STATS`
//! drive a running standalone. Nothing here runs unless they are set.

use std::path::PathBuf;
use std::sync::OnceLock;

pub struct Boot {
    pub path: PathBuf,
    pub play: bool,
    pub ready: Option<PathBuf>,
}

pub fn boot() -> Option<Boot> {
    let path = std::env::var("SONOMORI_LOAD").ok()?;
    if path.is_empty() {
        return None;
    }
    Some(Boot {
        path: PathBuf::from(path),
        play: std::env::var_os("SONOMORI_PLAY").is_some(),
        ready: std::env::var("SONOMORI_READY")
            .ok()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
    })
}

pub fn stats_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var("SONOMORI_STATS").ok())
        .as_deref()
}

pub struct Watch {
    id: u64,
    play: bool,
    ready: Option<PathBuf>,
}

impl Watch {
    pub fn idle() -> Self {
        Self {
            id: 0,
            play: false,
            ready: None,
        }
    }

    pub fn arm(&mut self, id: u64, boot: Boot) {
        self.id = id;
        self.play = boot.play;
        self.ready = boot.ready;
    }

    /// Whether this load should start audition.
    pub fn finish(&mut self, id: u64, ok: bool) -> bool {
        if self.id != id {
            return false;
        }
        self.id = 0;
        let play = ok && self.play;
        self.play = false;
        if let Some(path) = self.ready.take() {
            let body = if ok { "ok\n" } else { "fail\n" };
            let _ = std::fs::write(path, body);
        }
        play
    }
}
