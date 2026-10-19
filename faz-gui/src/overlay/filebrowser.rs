//! A directory browser overlay, used to pick the file to send or the
//! destination directory to receive into during a ZMODEM transfer.
//!
//! Built on the same immediate-mode pattern as `overlay::selector`: a
//! `run_loop` polls input, rebuilds the visible rows, and renders them with
//! termwiz. It differs in navigating a tree instead of filtering a flat list,
//! so it keeps its own state: the current directory, the entries in it, and
//! the cursor.
//!
//! The result is returned through a channel rather than an event callback.
//! Transfers are started from the caller's own thread, so a channel keeps the
//! "what did the user pick" answer and the "start the transfer" action in the
//! same place, without routing a synthetic event through the config system.

use std::path::{Path, PathBuf};

use termwiz::cell::CellAttributes;
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use termwiz::surface::{Change, Position};
use termwiz::terminal::Terminal;

use mux::termwiztermtab::TermWizTerminal;

/// What the browser is allowed to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// Only files can be chosen. Enter on a directory descends into it.
    File,
    /// Only directories can be chosen. Enter descends; the confirm key picks
    /// the current directory.
    Directory,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickResult {
    /// The chosen path. For [`PickMode::File`] this is a regular file; for
    /// [`PickMode::Directory`] a directory.
    Chosen(PathBuf),
    /// Escape or `q`: nothing chosen.
    Cancelled,
}

/// One row in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

impl Entry {
    /// The name as shown, with a trailing slash so directories are obvious.
    fn label(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// Read a directory into sorted entries: directories first, then files, each
/// group sorted case-insensitively by name.
///
/// Unreadable directories yield an empty listing rather than an error, so the
/// browser can show "permission denied" as an empty view instead of dying.
fn read_dir(dir: &Path) -> Vec<Entry> {
    let Ok(iter) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for entry in iter.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            // Hidden files are skipped: a transfer is nearly always aimed at a
            // visible file, and dotfiles would dominate home directories.
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(Entry { name, path, is_dir });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// The parent of `dir`, or `None` at the filesystem root.
fn parent_of(dir: &Path) -> Option<PathBuf> {
    dir.parent().map(|p| p.to_path_buf())
}

struct BrowserState {
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Index into `entries`. Clamped to `0` for an empty listing.
    cursor: usize,
    /// First visible row, kept so the cursor stays on screen.
    top: usize,
    mode: PickMode,
    title: String,
    /// Rows available for the listing, set from the terminal size.
    viewport: usize,
    /// Set when the user confirmed; consumed by `run_loop`.
    outcome: Option<PickResult>,
}

impl BrowserState {
    fn new(dir: PathBuf, mode: PickMode, title: String) -> Self {
        let entries = read_dir(&dir);
        Self {
            dir,
            entries,
            cursor: 0,
            top: 0,
            mode,
            title,
            viewport: 10,
            outcome: None,
        }
    }

    fn navigate_to(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.entries = read_dir(&self.dir);
        self.cursor = 0;
        self.top = 0;
    }

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        }
    }

    fn move_down(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1).min(self.entries.len() - 1);
        if self.cursor >= self.top + self.viewport {
            self.top = self.cursor + 1 - self.viewport;
        }
    }

    /// Enter on the current row: descend into a directory, or confirm a file.
    fn activate(&mut self) {
        let Some(entry) = self.current().cloned() else {
            // Nothing under the cursor. In Directory mode that means either an
            // empty directory or one with no visible entries, and the directory
            // on screen is the only thing that can be chosen — so Enter picks
            // it rather than doing nothing.
            self.choose_current_dir();
            return;
        };
        if entry.is_dir {
            self.navigate_to(entry.path);
        } else if self.mode == PickMode::File {
            self.outcome = Some(PickResult::Chosen(entry.path));
        }
        // A file under PickMode::Directory is not selectable; ignore.
    }

    /// Confirm the current directory itself. Only meaningful in Directory mode.
    fn choose_current_dir(&mut self) {
        if self.mode == PickMode::Directory {
            self.outcome = Some(PickResult::Chosen(self.dir.clone()));
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = parent_of(&self.dir) {
            // Keep the directory we came from selected, so a mistaken descent
            // is easy to undo.
            let previous = self.dir.clone();
            self.navigate_to(parent);
            if let Some(idx) = self.entries.iter().position(|e| e.path == previous) {
                self.cursor = idx;
                if self.cursor >= self.top + self.viewport {
                    self.top = self.cursor + 1 - self.viewport;
                }
            }
        }
    }

    fn render(&self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let size = term.get_screen_size()?;
        let width = size.cols.max(20);
        let height = size.rows.max(6);

        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
        ];

        // Header: title, then the current directory.
        let header = CellAttributes::default()
            .set_intensity(termwiz::cell::Intensity::Bold)
            .clone();
        changes.push(Change::AllAttributes(header));
        changes.push(Change::Text(truncate(&self.title, width)));
        changes.push(Change::Text("\r\n".to_string()));

        changes.push(Change::AllAttributes(CellAttributes::default()));
        changes.push(Change::Text(truncate(
            &format!("[{}]", self.dir.display()),
            width,
        )));
        changes.push(Change::Text("\r\n".to_string()));

        // Listing.
        let rows = height.saturating_sub(4);
        let end = (self.top + rows).min(self.entries.len());
        if self.entries.is_empty() {
            changes.push(Change::AllAttributes(CellAttributes::default()));
            changes.push(Change::Text("  (empty)".to_string()));
            changes.push(Change::Text("\r\n".to_string()));
        }
        for (offset, entry) in self.entries[self.top..end].iter().enumerate() {
            let idx = self.top + offset;
            let selected = idx == self.cursor;
            let mut attr = CellAttributes::default();
            if selected {
                attr.set_reverse(true);
            }
            changes.push(Change::AllAttributes(attr));
            changes.push(Change::Text(truncate(
                &format!("  {}", entry.label()),
                width,
            )));
            changes.push(Change::AllAttributes(CellAttributes::default()));
            changes.push(Change::Text("\r\n".to_string()));
        }

        // Footer: the keys that apply to this mode.
        let footer = match self.mode {
            PickMode::File => "  方向键移动   Enter: 选择文件/进入目录   Backspace: 上级   Esc: 取消",
            PickMode::Directory => "  方向键移动   Enter: 进入目录   空格 或 Ctrl-S: 选择当前目录   Backspace: 上级   Esc: 取消",
        };
        let footer_attr = CellAttributes::default()
            .set_intensity(termwiz::cell::Intensity::Half)
            .clone();
        changes.push(Change::AllAttributes(footer_attr));
        changes.push(Change::Text(truncate(footer, width)));

        term.render(&changes)?;
        Ok(())
    }

    fn run_loop(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<PickResult> {
        loop {
            if let Some(outcome) = self.outcome.take() {
                return Ok(outcome);
            }
            self.render(term)?;

            let Some(event) = term.poll_input(None)? else {
                // Terminal closed underneath us.
                return Ok(PickResult::Cancelled);
            };

            match self.handle_event(event) {
                EventAction::Continue => {}
                EventAction::Done(result) => return Ok(result),
            }
        }
    }

    /// Apply one input event, returning what the loop should do next.
    ///
    /// Split out of `run_loop` so the key handling can be tested without a
    /// terminal: the Ctrl chords in particular are easy to break by matching
    /// lowercase, which never fires.
    fn handle_event(&mut self, event: InputEvent) -> EventAction {
        match event {
            InputEvent::Resized { .. } => {}
            // Ctrl chords match UPPERCASE letters. `TermWizTerminal::poll_input`
            // normalizes CTRL+lowercase to CTRL+uppercase before overlays see
            // it (see `mux/src/termwiztermtab.rs`), and `overlay::selector`
            // matches the same way. Matching lowercase here silently never
            // fires.
            InputEvent::Key(KeyEvent {
                key: KeyCode::UpArrow,
                ..
            }) => self.move_up(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::DownArrow,
                ..
            }) => self.move_down(),
            InputEvent::Key(KeyEvent {
                key,
                modifiers: mods,
            }) if ctrl_chord(&key, mods) == Some('k') => self.move_up(),
            InputEvent::Key(KeyEvent {
                key,
                modifiers: mods,
            }) if ctrl_chord(&key, mods) == Some('j') => self.move_down(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Enter,
                ..
            }) => self.activate(),
            // Space confirms the current directory in Directory mode. A
            // bare key: it still works when the Ctrl modifier is dropped
            // somewhere between AppKit and here.
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(' '),
                modifiers: Modifiers::NONE,
            }) => self.choose_current_dir(),
            InputEvent::Key(KeyEvent {
                key,
                modifiers: mods,
            }) if ctrl_chord(&key, mods) == Some('s') => {
                log::info!(
                    "filebrowser: Ctrl-S ({}) confirmed {}",
                    describe_ctrl_chord(&key, mods),
                    self.dir.display()
                );
                self.choose_current_dir();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Backspace,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::LeftArrow,
                ..
            }) => self.go_up(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Char('q'),
                ..
            }) => return EventAction::Done(PickResult::Cancelled),
            _ => {}
        }
        EventAction::Continue
    }
}

/// Does this event represent `Ctrl+<letter>`?
///
/// The same physical chord can arrive in several shapes and the code cannot
/// assume which: macOS may deliver a control character with no modifier, the
/// terminal layer may normalise the letter's case, or the letter may arrive
/// as typed. Accepting all of them keeps the binding working regardless, and
/// [`describe_ctrl_chord`] logs which shape actually showed up so the set can
/// be narrowed once observed.
fn ctrl_chord(key: &KeyCode, mods: Modifiers) -> Option<char> {
    let KeyCode::Char(c) = key else {
        return None;
    };
    // Control codes for Ctrl+A..Ctrl+Z are 0x01..0x1a.
    let lower = if ('\u{1}'..='\u{1a}').contains(c) {
        let offset = (*c as u32 - 1) as u8;
        (b'a' + offset) as char
    } else {
        c.to_ascii_lowercase()
    };
    if mods.contains(Modifiers::CTRL) || ('\u{1}'..='\u{1a}').contains(c) {
        Some(lower)
    } else {
        None
    }
}

/// Render an event for logging, so a binding that does not fire can be
/// diagnosed from the log instead of by guessing at the platform's behaviour.
fn describe_ctrl_chord(key: &KeyCode, mods: Modifiers) -> String {
    format!("key={key:?} mods={mods:?}")
}

/// What `BrowserState::handle_event` decided.
#[derive(Debug, PartialEq, Eq)]
enum EventAction {
    /// Keep looping.
    Continue,
    /// Return this result from `run_loop`.
    Done(PickResult),
}

/// Render `s` clipped to `width` columns.
///
/// Counts characters, not display cells: names are short and this only has to
/// stop a long path from wrapping and corrupting the overlay, not align a
/// table.
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Run the browser on `term` and return what the user picked.
///
/// Returns [`PickResult::Cancelled`] if the terminal closes underneath the
/// overlay, so callers never have to distinguish "cancelled" from "gone".
pub fn browse(
    mut term: TermWizTerminal,
    start_dir: PathBuf,
    mode: PickMode,
    title: String,
) -> anyhow::Result<PickResult> {
    term.no_grab_mouse_in_raw_mode();
    let mut state = BrowserState::new(start_dir, mode, title);
    state.run_loop(&mut term)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"x").expect("write file");
    }

    /// A temp tree: files `b.txt`, `a.txt`, dirs `zdir/`, `adir/`.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("zdir")).expect("mkdir zdir");
        std::fs::create_dir(dir.path().join("adir")).expect("mkdir adir");
        touch(&dir.path().join("b.txt"));
        touch(&dir.path().join("a.txt"));
        dir
    }

    #[test]
    fn read_dir_lists_directories_first_then_files_sorted() {
        let dir = fixture();
        let entries = read_dir(dir.path());
        let names: Vec<_> = entries.iter().map(|e| e.label()).collect();
        assert_eq!(names, vec!["adir/", "zdir/", "a.txt", "b.txt"]);
    }

    #[test]
    fn read_dir_skips_hidden_entries() {
        let dir = fixture();
        touch(&dir.path().join(".hidden"));
        std::fs::create_dir(dir.path().join(".hiddendir")).expect("mkdir");
        let entries = read_dir(dir.path());
        assert!(
            entries.iter().all(|e| !e.name.starts_with('.')),
            "hidden entries leaked: {:?}",
            entries.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn read_dir_on_a_missing_directory_is_empty_not_an_error() {
        let entries = read_dir(Path::new("/nonexistent/definitely/not/here"));
        assert!(entries.is_empty());
    }

    #[test]
    fn cursor_moves_within_bounds() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());

        state.move_up();
        assert_eq!(state.cursor, 0, "cannot move above the first row");

        state.move_down();
        state.move_down();
        assert_eq!(state.cursor, 2);
        state.move_down();
        state.move_down();
        state.move_down();
        assert_eq!(state.cursor, 3, "clamped to the last row");
    }

    #[test]
    fn moving_down_scrolls_the_viewport() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..30 {
            touch(&dir.path().join(format!("f{i:02}.txt")));
        }
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        state.viewport = 5;

        for _ in 0..9 {
            state.move_down();
        }
        assert_eq!(state.cursor, 9);
        assert!(
            state.top + state.viewport > state.cursor,
            "cursor {} must stay inside viewport [{}, {})",
            state.cursor,
            state.top,
            state.top + state.viewport
        );
    }

    #[test]
    fn enter_on_a_directory_descends_into_it() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        // First entry is `adir/`.
        assert_eq!(state.current().map(|e| e.name.clone()), Some("adir".into()));
        state.activate();
        assert_eq!(state.dir, dir.path().join("adir"));
        assert!(state.entries.is_empty(), "adir is empty");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn enter_on_a_file_in_file_mode_chooses_it() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        // Index 2 is `a.txt` (after adir/ and zdir/).
        state.cursor = 2;
        assert_eq!(
            state.current().map(|e| e.name.clone()),
            Some("a.txt".into())
        );
        state.activate();
        assert_eq!(
            state.outcome,
            Some(PickResult::Chosen(dir.path().join("a.txt")))
        );
    }

    #[test]
    fn enter_on_a_file_in_directory_mode_does_nothing() {
        let dir = fixture();
        let mut state =
            BrowserState::new(dir.path().to_path_buf(), PickMode::Directory, "t".into());
        state.cursor = 2;
        state.activate();
        assert_eq!(state.outcome, None, "a file is not a valid destination");
        assert_eq!(state.dir, dir.path(), "must not navigate");
    }

    #[test]
    fn ctrl_s_chooses_the_current_directory_in_directory_mode() {
        let dir = fixture();
        let mut state =
            BrowserState::new(dir.path().to_path_buf(), PickMode::Directory, "t".into());
        state.choose_current_dir();
        assert_eq!(
            state.outcome,
            Some(PickResult::Chosen(dir.path().to_path_buf()))
        );
    }

    #[test]
    fn ctrl_s_is_ignored_in_file_mode() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        state.choose_current_dir();
        assert_eq!(state.outcome, None);
    }

    #[test]
    fn go_up_returns_to_the_parent_and_reselects_it() {
        let dir = fixture();
        let nested = dir.path().join("adir");
        let mut state = BrowserState::new(nested.clone(), PickMode::File, "t".into());

        state.go_up();
        assert_eq!(state.dir, dir.path());
        // The directory we came from is selected, so a mistake is easy to undo.
        assert_eq!(
            state.current().map(|e| e.path.clone()),
            Some(nested),
            "the directory we left should be selected"
        );
    }

    #[test]
    fn go_up_at_the_root_stays_put() {
        let mut state = BrowserState::new(PathBuf::from("/"), PickMode::File, "t".into());
        state.go_up();
        assert_eq!(state.dir, PathBuf::from("/"));
    }

    #[test]
    fn truncate_keeps_short_strings_and_clips_long_ones() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 3), "abc");
        let clipped = truncate("abcdefghij", 5);
        assert_eq!(clipped.chars().count(), 5);
        assert!(clipped.ends_with('…'));
    }

    /// Build a key event the way `TermWizTerminal::poll_input` delivers it.
    fn ctrl(c: char) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key: KeyCode::Char(c),
            modifiers: Modifiers::CTRL,
        })
    }

    fn arrow(key: KeyCode) -> InputEvent {
        InputEvent::Key(KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        })
    }

    /// Regression for a reported bug: Ctrl-S did nothing after picking a
    /// directory. The chord's exact shape depends on the platform and the
    /// terminal layer, so every shape that has been observed must confirm.
    #[test]
    fn ctrl_s_confirms_the_directory_in_every_shape() {
        for (label, event) in [
            ("uppercase + CTRL", ctrl('S')),
            ("lowercase + CTRL", ctrl('s')),
            (
                "control char, no modifier",
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('\u{13}'),
                    modifiers: Modifiers::NONE,
                }),
            ),
            (
                "control char + CTRL",
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('\u{13}'),
                    modifiers: Modifiers::CTRL,
                }),
            ),
        ] {
            let dir = fixture();
            let mut state =
                BrowserState::new(dir.path().to_path_buf(), PickMode::Directory, "t".into());
            state.handle_event(event);
            assert_eq!(
                state.outcome,
                Some(PickResult::Chosen(dir.path().to_path_buf())),
                "Ctrl-S as {label} must confirm the directory"
            );
        }
    }

    #[test]
    fn space_confirms_the_directory_without_a_modifier() {
        // A bare key path, so the binding survives a dropped Ctrl modifier.
        let dir = fixture();
        let mut state =
            BrowserState::new(dir.path().to_path_buf(), PickMode::Directory, "t".into());
        state.handle_event(InputEvent::Key(KeyEvent {
            key: KeyCode::Char(' '),
            modifiers: Modifiers::NONE,
        }));
        assert_eq!(
            state.outcome,
            Some(PickResult::Chosen(dir.path().to_path_buf()))
        );
    }

    #[test]
    fn enter_confirms_an_empty_directory_in_directory_mode() {
        // Nothing to descend into, so the directory on screen is the answer.
        let empty = tempfile::tempdir().expect("tempdir");
        let mut state =
            BrowserState::new(empty.path().to_path_buf(), PickMode::Directory, "t".into());
        state.handle_event(arrow(KeyCode::Enter));
        assert_eq!(
            state.outcome,
            Some(PickResult::Chosen(empty.path().to_path_buf()))
        );
    }

    #[test]
    fn ctrl_j_and_k_move_the_cursor() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());

        state.handle_event(ctrl('J'));
        assert_eq!(state.cursor, 1);
        state.handle_event(ctrl('K'));
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn arrows_move_the_cursor() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        state.handle_event(arrow(KeyCode::DownArrow));
        assert_eq!(state.cursor, 1);
        state.handle_event(arrow(KeyCode::UpArrow));
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn escape_and_q_cancel() {
        let dir = fixture();
        for key in ['q'] {
            let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
            assert_eq!(
                state.handle_event(InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(key),
                    modifiers: Modifiers::NONE,
                })),
                EventAction::Done(PickResult::Cancelled)
            );
        }
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        assert_eq!(
            state.handle_event(arrow(KeyCode::Escape)),
            EventAction::Done(PickResult::Cancelled)
        );
    }

    #[test]
    fn enter_descends_and_backspace_goes_up() {
        let dir = fixture();
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        assert_eq!(
            state.handle_event(arrow(KeyCode::Enter)),
            EventAction::Continue
        );
        assert_eq!(state.dir, dir.path().join("adir"));
        state.handle_event(arrow(KeyCode::Backspace));
        assert_eq!(state.dir, dir.path());
    }

    #[test]
    fn empty_directory_has_no_current_entry_and_ignores_movement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = BrowserState::new(dir.path().to_path_buf(), PickMode::File, "t".into());
        assert!(state.current().is_none());
        state.move_down();
        state.move_up();
        state.activate();
        assert_eq!(state.cursor, 0);
        assert_eq!(state.outcome, None);
    }
}
