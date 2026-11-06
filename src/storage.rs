//! Keeps the history between runs, as JSON in `$XDG_DATA_HOME/calculator/history.json`.

use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
};

use crate::calc::Calculation;

fn history_path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(data.join("calculator").join("history.json"))
}

/// The saved history, newest last and at most `limit` long, or an empty one if there is none
/// or it can't be read.
pub fn load_history(limit: usize) -> VecDeque<Calculation> {
    let mut history = history_path().map(|path| load(&path)).unwrap_or_default();
    history.drain(..history.len().saturating_sub(limit));
    history.shrink_to_fit();
    history
}

pub fn save_history(history: &VecDeque<Calculation>) {
    if let Some(path) = history_path()
        && let Err(err) = save(&path, history)
    {
        eprintln!("could not save history to {}: {err}", path.display());
    }
}

/// A history that can't be read is moved aside, so the next save doesn't overwrite it.
fn load(path: &Path) -> VecDeque<Calculation> {
    let Ok(bytes) = std::fs::read(path) else {
        return VecDeque::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        eprintln!("could not read history from {}: {err}", path.display());
        let aside = path.with_extension("json.bad");
        if std::fs::rename(path, &aside).is_ok() {
            eprintln!("moved it to {}", aside.display());
        }
        VecDeque::new()
    })
}

/// Writes to a temporary file first, so a crash never leaves a half-written history.
fn save(path: &Path, history: &VecDeque<Calculation>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Per process, so two running calculators don't write the same temporary file.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(history)?)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::{Base, Value, WordSize};

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("calc-history-test-{}", std::process::id()));
        let path = dir.join("sub").join("history.json");
        assert!(load(&path).is_empty());

        let history = VecDeque::from([
            Calculation {
                expression: "1 + 2 =".into(),
                result: Value::Real(3.0),
                base: Base::Dec,
                word: WordSize::Qword,
            },
            Calculation {
                expression: "FF + 1 =".into(),
                result: Value::Int(0),
                base: Base::Hex,
                word: WordSize::Byte,
            },
        ]);
        save(&path, &history).unwrap();
        assert_eq!(load(&path), history);

        // A corrupt file reads as empty, and is kept aside.
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path).is_empty());
        assert_eq!(
            std::fs::read(path.with_extension("json.bad")).unwrap(),
            b"not json"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
