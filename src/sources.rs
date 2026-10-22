//! Source positions (plan/12, T1): where each list in the code being
//! compiled was read, so that errors can say `file:line`.
//!
//! A position is a file and a line, packed into one number (see
//! [`Sources::position`]) so the compiler in Scheme can hold it as a
//! fixnum. File names are kept for the life of the process.
//!
//! The reader records positions only while a table is open: from reading
//! a unit of source (a program, a REPL form, a library file) until it has
//! been compiled. The key is the list's first pair. Objects don't move, so
//! a pair keeps its key while it is live; the unit's forms are live until
//! it is compiled, when its table is closed. `read` in a program records
//! nothing.

use crate::value::Value;
use rustc_hash::FxHashMap;

/// Lines above this are recorded as this.
const MAX_LINE: u32 = (1 << 24) - 1;

#[derive(Default)]
pub struct Sources {
    /// The file names positions refer to, by number.
    files: Vec<Box<str>>,
    ids: FxHashMap<Box<str>, u32>,
    /// The open tables, innermost (most recently opened) last: each maps
    /// a list to its position.
    tables: Vec<FxHashMap<Value, u64>>,
}

impl Sources {
    /// Open a table, for a unit of source about to be read.
    pub fn open(&mut self) {
        self.tables.push(FxHashMap::default());
    }

    /// Close the innermost table.
    pub fn close(&mut self) {
        self.tables.pop();
    }

    /// The number of open tables, to close back to with [`truncate`](Self::truncate).
    pub fn depth(&self) -> usize {
        self.tables.len()
    }

    /// Close the tables opened after there were `depth`, as after an error.
    pub fn truncate(&mut self, depth: usize) {
        self.tables.truncate(depth);
    }

    /// Whether the reader should record positions.
    pub fn recording(&self) -> bool {
        !self.tables.is_empty()
    }

    /// The position of `line` in `file`.
    pub fn position(&mut self, file: &str, line: u32) -> u64 {
        let id = match self.ids.get(file) {
            Some(&id) => id,
            None => {
                let id = self.files.len() as u32;
                self.files.push(file.into());
                self.ids.insert(file.into(), id);
                id
            }
        };
        ((id as u64) << 24) | line.min(MAX_LINE) as u64
    }

    /// Record that `list` was read at `position`, in the innermost table.
    pub fn record(&mut self, list: Value, position: u64) {
        if let Some(table) = self.tables.last_mut() {
            table.insert(list, position);
        }
    }

    /// Where `list` was read, if it was read as source.
    pub fn lookup(&self, list: Value) -> Option<u64> {
        self.tables.iter().rev().find_map(|t| t.get(&list).copied())
    }

    /// The file and line of `position`.
    pub fn parts(&self, position: u64) -> Option<(&str, u32)> {
        let file = self.files.get((position >> 24) as usize)?;
        Some((file, (position & MAX_LINE as u64) as u32))
    }

    /// `position` as `file:line`.
    pub fn describe(&self, position: u64) -> Option<String> {
        self.parts(position).map(|(file, line)| format!("{}:{}", file, line))
    }
}
