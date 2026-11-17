// Copyright © 2016-2026 The SENTIENT Authors

//! The Terminal dialog (the approved mockup): "Terminal: <name>" in the
//! shared dialog (crate::modal) with its full-screen button, the screen
//! grid in MesloLGS NF, the status bar as the dialog's foot. A second
//! terminal on the same computer opens beside the first (the foot's +);
//! with more than one, a row of tabs picks the one shown. Closing a tab ends
//! that session; closing the dialog ends all of them.
//!
//! The grid is drawn row by row as styled text (one run per colour change),
//! the cursor as an inverted cell (or an outline while the dialog is not
//! focused), the selection as a highlight. Keys go to the program
//! (crate::terminal::keys), except Ctrl+Shift+C / Ctrl+Shift+V (copy and
//! paste); Escape too, so the dialog does not close under nano. The mouse
//! selects (double click: a word), the wheel scrolls back, or goes to the
//! program when it asked for the mouse (htop) or runs on the alternate
//! screen (less, man).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use futures::StreamExt as _;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::keys::{self, Mods, Shortcut};
use super::session::{GridSize, Session, Spawn};

/// The terminal's font (shipped, crate::terminal::load_fonts).
pub const FONT_FAMILY: &str = "MesloLGS NF";
const FONT_SIZE: f32 = 13.;
const LINE_H: f32 = 17.;
/// Space inside the screen's dark area.
const PAD: f32 = 10.;

/// Text colour (the mockup's #d8dee0) and the screen behind it.
const FG: u32 = 0xd8dee0;
const BG_LIGHT: u32 = 0x1b2124;
const BG_DARK: u32 = 0x151a1c;
const SELECTION: u32 = 0x3d5a63;

/// The 16 ANSI colours, tuned to sit on the dark screen.
const ANSI: [u32; 16] = [
    0x2e3538, 0xe06c6c, 0x8cc570, 0xe5c07b, 0x61a6e8, 0xc678dd, 0x4fb8c0, 0xd8dee0, // normal
    0x5c6b70, 0xff8a8a, 0xa5e08a, 0xf0d48a, 0x82bdf5, 0xd99cf0, 0x6fd3db, 0xffffff, // bright
];

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

fn from_rgb(c: Rgb) -> Hsla {
    Rgba { r: c.r as f32 / 255., g: c.g as f32 / 255., b: c.b as f32 / 255., a: 1. }.into()
}

/// xterm's 256 colours: the 16, the 6×6×6 cube, the 24 greys.
pub fn indexed(i: u8) -> u32 {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
            (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
        }
        _ => {
            let v = 8 + 10 * (i as u32 - 232);
            (v << 16) | (v << 8) | v
        }
    }
}

fn dim(c: Hsla) -> Hsla {
    Hsla { l: c.l * 0.66, ..c }
}

/// A colour as drawn: the program's own palette changes first, then ours.
fn resolve(c: Color, colors: &Colors, bg: Hsla) -> Hsla {
    match c {
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Indexed(i) => colors[i as usize].map(from_rgb).unwrap_or_else(|| hex(indexed(i))),
        Color::Named(n) => {
            if let Some(c) = colors[n] {
                return from_rgb(c);
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => hex(FG),
                NamedColor::Background => bg,
                NamedColor::DimForeground => dim(hex(FG)),
                n if (n as usize) < 16 => hex(ANSI[n as usize]),
                // Dim black … dim white follow Foreground … in the enum.
                n => {
                    let k = (n as usize).saturating_sub(NamedColor::DimBlack as usize).min(7);
                    dim(hex(ANSI[k]))
                }
            }
        }
    }
}

/// One cell's look (equal looks share a run).
#[derive(Clone, Copy, PartialEq)]
struct Look {
    fg: Hsla,
    bg: Option<Hsla>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// One row: its text and runs.
struct Row {
    text: String,
    runs: Vec<TextRun>,
}

/// The screen as drawn this frame.
struct Frame {
    rows: Vec<Row>,
    /// The cursor's viewport row and column, and its shape, when shown.
    cursor: Option<(usize, usize, CursorShape)>,
}

fn font_for(bold: bool, italic: bool) -> Font {
    Font { weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL }, style: if italic { FontStyle::Italic } else { FontStyle::Normal }, ..font(FONT_FAMILY) }
}

/// The screen of one session.
pub struct TermView {
    session: Option<Arc<Session>>,
    error: Option<String>,
    /// The shell's name for the status bar when known up front.
    shell: Option<String>,
    focus: FocusHandle,
    /// Where the grid was drawn (mouse positions to cells).
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The width of a cell (measured from the font once drawn).
    cell_w: Rc<Cell<f32>>,
    selecting: bool,
    scroll_px: f32,
    ended_noted: bool,
    _pump: Option<Task<()>>,
}

impl TermView {
    pub fn new(spawn: Spawn, shell: Option<String>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let started = Session::start(&spawn, GridSize { cols: 120, rows: 32 }, move || {
            let _ = tx.unbounded_send(());
        });
        let (session, error) = match started {
            Ok(s) => (Some(Arc::new(s)), None),
            Err(e) => (None, Some(e)),
        };
        // Redraw when output came (several wake-ups, one redraw).
        let pump = session.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                while rx.next().await.is_some() {
                    while let Ok(Some(())) = rx.try_next() {}
                    if this.update(cx, |v, cx| v.on_output(cx)).is_err() {
                        break;
                    }
                }
            })
        });
        TermView { session, error, shell, focus: cx.focus_handle(), bounds: Rc::new(Cell::new(Bounds::default())), cell_w: Rc::new(Cell::new(7.8)), selecting: false, scroll_px: 0., ended_noted: false, _pump: pump }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn alive(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.alive())
    }

    /// The shell's name: known up front, or what the session said.
    pub fn shell_name(&self) -> Option<String> {
        self.shell.clone().or_else(|| self.session.as_ref().and_then(|s| s.shell.lock().ok().and_then(|g| g.clone())))
    }

    pub fn end(&self) {
        if let Some(s) = &self.session {
            s.end();
        }
    }

    fn on_output(&mut self, cx: &mut Context<Self>) {
        if !self.alive() && !self.ended_noted {
            self.ended_noted = true;
            if let Some(s) = &self.session {
                if let Ok(mut e) = s.emu.lock() {
                    e.feed(b"\r\n\x1b[0;2mThe session has ended. Close this terminal and open it again to sign in again.\x1b[0m\r\n");
                }
            }
        }
        cx.notify();
    }

    fn mods(m: &Modifiers) -> Mods {
        Mods { ctrl: m.control, alt: m.alt, shift: m.shift }
    }

    fn key_down(&mut self, e: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let ks = &e.keystroke;
        // The system's own shortcuts (Super/Cmd) stay the window's.
        if ks.modifiers.platform {
            return;
        }
        cx.stop_propagation();
        let m = Self::mods(&ks.modifiers);
        match keys::shortcut(&ks.key, m) {
            Some(Shortcut::Copy) => {
                let text = s.emu.lock().ok().and_then(|e| e.term.selection_to_string());
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(t));
                }
                return;
            }
            Some(Shortcut::Paste) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    let bracketed = s.emu.lock().ok().is_some_and(|e| e.term.mode().contains(TermMode::BRACKETED_PASTE));
                    s.write(keys::paste_bytes(&text, bracketed));
                }
                return;
            }
            None => {}
        }
        let mode = s.emu.lock().ok().map(|e| *e.term.mode()).unwrap_or_default();
        // Shift+Page Up/Down page through what scrolled away.
        if m.shift && !m.ctrl && !mode.contains(TermMode::ALT_SCREEN) && (ks.key == "pageup" || ks.key == "pagedown") {
            if let Ok(mut e) = s.emu.lock() {
                e.term.scroll_display(if ks.key == "pageup" { Scroll::PageUp } else { Scroll::PageDown });
            }
            cx.notify();
            return;
        }
        // AltGr (Windows says Ctrl+Alt): the character it types.
        let bytes = if e.prefer_character_input {
            ks.key_char.as_ref().map(|c| c.as_bytes().to_vec())
        } else {
            keys::key_bytes(&ks.key, ks.key_char.as_deref(), m, mode.contains(TermMode::APP_CURSOR))
        };
        if let Some(b) = bytes {
            if let Ok(mut e) = s.emu.lock() {
                e.term.selection = None;
            }
            s.write(b);
            cx.notify();
        }
    }

    /// The cell under a window position, and the half of it.
    fn cell_at(&self, pos: Point<Pixels>, cols: usize, rows: usize) -> (usize, usize, Side) {
        let b = self.bounds.get();
        let cw = self.cell_w.get().max(1.);
        let x = (f32::from(pos.x - b.origin.x) - PAD).max(0.);
        let y = (f32::from(pos.y - b.origin.y) - PAD).max(0.);
        let col = ((x / cw).floor() as usize).min(cols.saturating_sub(1));
        let row = ((y / LINE_H).floor() as usize).min(rows.saturating_sub(1));
        let side = if x - col as f32 * cw < cw / 2. { Side::Left } else { Side::Right };
        (row, col, side)
    }

    /// The program asked for the mouse (SGR reports only; the old X10
    /// encoding is not sent).
    fn mouse_reporting(mode: TermMode) -> bool {
        mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE)
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(s) = self.session.clone() else { return };
        let Ok(mut emu) = s.emu.lock() else { return };
        let (cols, rows, offset, mode) = (emu.term.columns(), emu.term.screen_lines(), emu.term.grid().display_offset(), *emu.term.mode());
        let (row, col, side) = self.cell_at(e.position, cols, rows);
        if Self::mouse_reporting(mode) && !e.modifiers.shift {
            drop(emu);
            s.write(format!("\x1b[<0;{};{}M", col + 1, row + 1).into_bytes());
            return;
        }
        let point = GridPoint::new(Line(row as i32 - offset as i32), Column(col));
        let ty = if e.click_count >= 3 { SelectionType::Lines } else if e.click_count == 2 { SelectionType::Semantic } else { SelectionType::Simple };
        emu.term.selection = Some(Selection::new(ty, point, side));
        drop(emu);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(s) = self.session.clone() else { return };
        let Ok(mut emu) = s.emu.lock() else { return };
        let (cols, rows, offset) = (emu.term.columns(), emu.term.screen_lines(), emu.term.grid().display_offset());
        let (row, col, side) = self.cell_at(e.position, cols, rows);
        let point = GridPoint::new(Line(row as i32 - offset as i32), Column(col));
        if let Some(sel) = emu.term.selection.as_mut() {
            sel.update(point, side);
        }
        drop(emu);
        cx.notify();
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let mode = s.emu.lock().ok().map(|e| *e.term.mode()).unwrap_or_default();
        if Self::mouse_reporting(mode) && !e.modifiers.shift && !self.selecting {
            let (cols, rows) = s.emu.lock().map(|e| (e.term.columns(), e.term.screen_lines())).unwrap_or((80, 24));
            let (row, col, _) = self.cell_at(e.position, cols, rows);
            s.write(format!("\x1b[<0;{};{}m", col + 1, row + 1).into_bytes());
            return;
        }
        self.selecting = false;
        if let Ok(mut emu) = s.emu.lock() {
            if emu.term.selection.as_ref().is_some_and(|x| x.is_empty()) {
                emu.term.selection = None;
            }
        }
        cx.notify();
    }

    fn scroll(&mut self, e: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        // VERIFY: a positive y is the wheel turned up (towards older lines).
        self.scroll_px += f32::from(e.delta.pixel_delta(px(LINE_H)).y);
        let lines = (self.scroll_px / LINE_H).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * LINE_H;
        cx.stop_propagation();
        let Ok(mut emu) = s.emu.lock() else { return };
        let mode = *emu.term.mode();
        if Self::mouse_reporting(mode) {
            let (cols, rows) = (emu.term.columns(), emu.term.screen_lines());
            drop(emu);
            let (row, col, _) = self.cell_at(e.position, cols, rows);
            let button = if lines > 0 { 64 } else { 65 };
            let one = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            s.write(one.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            drop(emu);
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            s.write(arrow.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else {
            emu.term.scroll_display(Scroll::Delta(lines));
            drop(emu);
            cx.notify();
        }
    }

    /// The grid's size for the area it was given last frame.
    fn fit(&self, s: &Session) {
        let b = self.bounds.get();
        let w = f32::from(b.size.width) - 2. * PAD;
        let h = f32::from(b.size.height) - 2. * PAD;
        if w <= 0. || h <= 0. {
            return;
        }
        let cols = (w / self.cell_w.get().max(1.)).floor().max(2.) as u16;
        let rows = (h / LINE_H).floor().max(1.) as u16;
        s.resize(GridSize { cols, rows });
    }

    /// The screen's rows as text runs (the grid lock only while copying).
    fn frame(s: &Session, focused: bool, bg: Hsla) -> Option<Frame> {
        let emu = s.emu.lock().ok()?;
        let term = &emu.term;
        let content = term.renderable_content();
        let (cols, rows) = (term.columns(), term.screen_lines());
        let offset = content.display_offset;
        let colors = content.colors;
        let selection = content.selection;
        let cur = content.cursor;
        let cursor_view = (cur.point.line.0 + offset as i32, cur.point.column.0);
        let cursor = (cur.shape != CursorShape::Hidden && offset == 0 && cursor_view.0 >= 0).then(|| (cursor_view.0 as usize, cursor_view.1, cur.shape));
        let mut looks: Vec<Vec<(char, Option<Vec<char>>, Look)>> = (0..rows).map(|_| Vec::with_capacity(cols)).collect();
        for ind in content.display_iter {
            let vline = ind.point.line.0 + offset as i32;
            if vline < 0 || vline as usize >= rows {
                continue;
            }
            let cell = ind.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let f = cell.flags;
            // Bold makes the eight colours bright, as xterm does.
            let fg_c = match cell.fg {
                Color::Named(n) if f.contains(Flags::BOLD) && (n as usize) < 8 => Color::Named(n.to_bright()),
                c => c,
            };
            let mut fg = resolve(fg_c, colors, bg);
            let mut cbg = match cell.bg {
                Color::Named(NamedColor::Background) if colors[NamedColor::Background].is_none() => None,
                c => Some(resolve(c, colors, bg)),
            };
            if f.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if f.contains(Flags::INVERSE) {
                let back = cbg.unwrap_or(bg);
                cbg = Some(fg);
                fg = back;
            }
            if f.contains(Flags::HIDDEN) {
                fg = cbg.unwrap_or(bg);
            }
            if selection.is_some_and(|r| r.contains(ind.point)) {
                cbg = Some(hex(SELECTION));
            }
            if focused && cursor.is_some_and(|(r, c, shape)| shape == CursorShape::Block && r == vline as usize && c == ind.point.column.0) {
                let back = cbg.unwrap_or(bg);
                cbg = Some(hex(FG));
                fg = back;
            }
            let look = Look { fg, bg: cbg, bold: f.contains(Flags::BOLD), italic: f.contains(Flags::ITALIC), underline: f.intersects(Flags::ALL_UNDERLINES), strike: f.contains(Flags::STRIKEOUT) };
            let c = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };
            looks[vline as usize].push((c, cell.zerowidth().map(|z| z.to_vec()), look));
        }
        drop(emu);
        let rows = looks
            .into_iter()
            .map(|cells| {
                let mut text = String::with_capacity(cells.len());
                let mut runs: Vec<TextRun> = Vec::new();
                let mut last: Option<Look> = None;
                for (c, zw, look) in cells {
                    let start = text.len();
                    text.push(c);
                    if let Some(z) = zw {
                        text.extend(z);
                    }
                    let len = text.len() - start;
                    match runs.last_mut() {
                        Some(r) if last == Some(look) => r.len += len,
                        _ => {
                            runs.push(TextRun {
                                len,
                                font: font_for(look.bold, look.italic),
                                color: look.fg,
                                background_color: look.bg,
                                underline: look.underline.then(|| UnderlineStyle { thickness: px(1.), color: Some(look.fg), wavy: false }),
                                strikethrough: look.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(look.fg) }),
                            });
                            last = Some(look);
                        }
                    }
                }
                if text.is_empty() {
                    text.push(' ');
                    runs.push(TextRun { len: 1, font: font_for(false, false), color: hex(FG), background_color: None, underline: None, strikethrough: None });
                }
                Row { text, runs }
            })
            .collect();
        Some(Frame { rows, cursor })
    }
}

impl Render for TermView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = hex(if crate::theme::is_dark(cx) { BG_DARK } else { BG_LIGHT });
        // The cell width from the font (Meslo: every cell the same).
        let fid = window.text_system().resolve_font(&font(FONT_FAMILY));
        if let Ok(a) = window.text_system().advance(fid, px(FONT_SIZE), 'm') {
            self.cell_w.set(f32::from(a.width));
        }
        let screen = div().relative().flex_1().min_h_0().w_full().rounded(px(8.)).bg(bg).overflow_hidden();
        let Some(s) = self.session.clone() else {
            let msg = self.error.clone().unwrap_or_else(|| "The terminal could not be started.".into());
            return screen.p(px(PAD)).text_color(hex(FG)).text_size(px(FONT_SIZE)).font_family(FONT_FAMILY).child(msg).into_any_element();
        };
        self.fit(&s);
        let focused = self.focus.is_focused(window);
        let Some(frame) = Self::frame(&s, focused, bg) else { return screen.into_any_element() };
        let cw = self.cell_w.get();
        // The cursor drawn on top: an outline while not focused, a bar or
        // an underline when the program asks for one.
        let cursor = frame.cursor.and_then(|(r, c, shape)| {
            let block_inverted = focused && shape == CursorShape::Block;
            (!block_inverted).then(|| {
                let d = div().absolute().left(px(PAD + c as f32 * cw)).top(px(PAD + r as f32 * LINE_H));
                match shape {
                    CursorShape::Beam => d.w(px(2.)).h(px(LINE_H)).bg(hex(FG)),
                    CursorShape::Underline => d.w(px(cw)).h(px(2.)).mt(px(LINE_H - 2.)).bg(hex(FG)),
                    _ => d.w(px(cw)).h(px(LINE_H)).border_1().border_color(hex(FG)),
                }
            })
        });
        // Where the grid is (mouse positions to cells); a new size redraws.
        let (cell, weak) = (self.bounds.clone(), cx.entity().downgrade());
        let probe = canvas(
            move |b, _, cx| {
                if cell.get() != b {
                    cell.set(b);
                    cx.defer(move |cx| {
                        weak.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        screen
            .id("term-screen")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .cursor_text()
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(probe)
            .child(
                v_flex()
                    .absolute()
                    .left(px(PAD))
                    .top(px(PAD))
                    .font_family(FONT_FAMILY)
                    .text_size(px(FONT_SIZE))
                    .line_height(px(LINE_H))
                    .text_color(hex(FG))
                    .children(frame.rows.into_iter().map(|r| div().h(px(LINE_H)).flex_shrink_0().whitespace_nowrap().child(StyledText::new(r.text).with_runs(r.runs)))),
            )
            .children(cursor)
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// The dialog: the sessions of one computer, the status bar
// ---------------------------------------------------------------------------

/// How to start one more session on the dialog's computer.
#[derive(Clone)]
pub struct Target {
    pub spawn: Spawn,
    /// The shell's name when known before it starts.
    pub shell: Option<String>,
}

/// What the dialog asks before it ends running sessions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ask {
    /// The dialog's close button (or Escape outside the screen).
    All,
    /// One tab's close button.
    Tab(usize),
}

/// The question before closing ends sessions that still run (None: none
/// runs, close at once).
pub fn end_question(running: usize, one_tab: bool) -> Option<String> {
    match (running, one_tab) {
        (0, _) => None,
        (_, true) => Some("End this terminal session?".into()),
        (1, false) => Some("End 1 running terminal session?".into()),
        (n, false) => Some(format!("End {n} running terminal sessions?")),
    }
}

/// The dialog's body: the sessions, the tabs when there are several.
pub struct Deck {
    target: Target,
    tabs: Vec<Entity<TermView>>,
    active: usize,
    /// Asking before ending running sessions.
    ask: Option<Ask>,
    _watch: Vec<Subscription>,
}

impl Deck {
    pub fn new(target: Target, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut d = Deck { target, tabs: Vec::new(), active: 0, ask: None, _watch: Vec::new() };
        d.add(window, cx);
        d
    }

    /// One more session, shown and focused.
    pub fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = self.target.clone();
        let view = cx.new(|cx| TermView::new(t.spawn, t.shell, cx));
        self._watch.push(cx.observe(&view, |_, _, cx| cx.notify()));
        let focus = view.read(cx).focus_handle();
        self.tabs.push(view);
        self.active = self.tabs.len() - 1;
        window.focus(&focus, cx);
        cx.notify();
    }

    fn select(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.tabs.get(i) {
            self.active = i;
            let focus = v.read(cx).focus_handle();
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    /// Close one tab: its session ends. The last one closes the dialog.
    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        let v = self.tabs.remove(i);
        v.read(cx).end();
        if self.tabs.is_empty() {
            crate::modal::close(cx);
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.select(self.active, window, cx);
    }

    /// How many sessions still run.
    fn running(&self, cx: &App) -> usize {
        self.tabs.iter().filter(|v| v.read(cx).alive()).count()
    }

    /// The dialog is asked to close: true closes it now (nothing runs),
    /// false asks first.
    pub fn ask_close_all(&mut self, cx: &mut Context<Self>) -> bool {
        if self.running(cx) == 0 {
            return true;
        }
        self.ask = Some(Ask::All);
        cx.notify();
        false
    }

    /// A tab's close button: asks first while its session runs.
    fn ask_close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.get(i).is_some_and(|v| v.read(cx).alive()) {
            self.ask = Some(Ask::Tab(i));
            cx.notify();
        } else {
            self.close_tab(i, window, cx);
        }
    }

    /// The answer: end the sessions asked about, or keep them.
    fn answer(&mut self, end: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ask = self.ask.take();
        cx.notify();
        match (end, ask) {
            (true, Some(Ask::All)) => {
                self.end_all(cx);
                crate::modal::close(cx);
            }
            (true, Some(Ask::Tab(i))) => self.close_tab(i, window, cx),
            _ => {
                if let Some(v) = self.active_view() {
                    let focus = v.read(cx).focus_handle();
                    window.focus(&focus, cx);
                }
            }
        }
    }

    /// End every session (the dialog closed).
    pub fn end_all(&self, cx: &App) {
        for v in &self.tabs {
            v.read(cx).end();
        }
    }

    fn active_view(&self) -> Option<&Entity<TermView>> {
        self.tabs.get(self.active)
    }
}

impl Render for Deck {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let names: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{} {}", v.read(cx).shell_name().unwrap_or_else(|| "Terminal".into()), i + 1))
            .collect();
        let active = self.active;
        let question = self.ask.and_then(|a| match a {
            Ask::All => end_question(self.running(cx), false).map(|q| (q, "End sessions")),
            Ask::Tab(_) => end_question(1, true).map(|q| (q, "End session")),
        });
        // Over the screen while asking: the question, Cancel, the action.
        let overlay = question.map(|(q, action)| {
            div()
                .id("term-ask")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .bg(black().opacity(0.45))
                .child(
                    v_flex()
                        .w(px(400.))
                        .gap_4()
                        .p(px(20.))
                        .rounded(px(crate::ui::PANEL_RADIUS))
                        .bg(theme.background)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(q))
                        .child(
                            h_flex()
                                .gap(crate::ui::button_gap())
                                .justify_end()
                                .child(crate::ui::probed(crate::ui::button("term-ask-cancel").outline().label("Cancel").on_click(cx.listener(|d, _, w, cx| d.answer(false, w, cx)))))
                                .child(crate::ui::probed(crate::modal::danger_button(&theme, "term-ask-end", action.into(), cx.listener(|d, _, w, cx| d.answer(true, w, cx))))),
                        ),
                )
        });
        v_flex()
            .relative()
            .size_full()
            .min_h_0()
            .gap(px(8.))
            .when(self.tabs.len() > 1, |c| {
                c.child(h_flex().gap(px(6.)).flex_shrink_0().children(names.into_iter().enumerate().map(|(i, name)| {
                    h_flex()
                        .id(("term-tab", i))
                        .h(px(26.))
                        .pl(px(10.))
                        .pr(px(4.))
                        .gap(px(4.))
                        .items_center()
                        .rounded(px(8.))
                        .text_size(px(13.))
                        .cursor_pointer()
                        .when(i == active, |t| t.bg(theme.muted).text_color(theme.foreground))
                        .when(i != active, |t| t.text_color(theme.muted_foreground).hover(|t| t.bg(theme.muted.opacity(0.5))))
                        .on_click(cx.listener(move |d, _, w, cx| d.select(i, w, cx)))
                        .child(name)
                        .child(
                            crate::ui::button(("term-tab-close", i))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Close this terminal")
                                .on_click(cx.listener(move |d, _, w, cx| {
                                    cx.stop_propagation();
                                    d.ask_close_tab(i, w, cx);
                                })),
                        )
                })))
            })
            .children(self.active_view().cloned())
            .children(overlay)
    }
}

/// The status bar (the dialog's foot): connected or not, the shell, a new
/// terminal, the copy and paste keys.
pub struct StatusBar {
    deck: Entity<Deck>,
    _watch: Subscription,
}

impl StatusBar {
    pub fn new(deck: Entity<Deck>, cx: &mut Context<Self>) -> Self {
        let watch = cx.observe(&deck, |_, _, cx| cx.notify());
        StatusBar { deck, _watch: watch }
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (alive, shell) = self.deck.read(cx).active_view().map(|v| (v.read(cx).alive(), v.read(cx).shell_name())).unwrap_or((false, None));
        let deck = self.deck.clone();
        h_flex()
            .gap(px(8.))
            .items_center()
            .text_size(px(13.))
            .child(div().size(px(crate::ui::STATUS_DOT)).rounded_full().bg(rgb(if alive { crate::ui::STATUS_GREEN } else { crate::ui::STATUS_GREY })))
            .child(div().text_color(theme.foreground).child(if alive { "Connected" } else { "Disconnected" }))
            .children(shell.map(|s| div().text_color(theme.muted_foreground).child(s)))
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).whitespace_nowrap().child("Copy Ctrl+Shift+C, paste Ctrl+Shift+V"))
            .child(crate::ui::probed(
                crate::ui::button("term-new")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("Open another terminal")
                    .on_click(move |_, w, cx| {
                        deck.update(cx, |d, cx| d.add(w, cx));
                    }),
            ))
    }
}

/// Open the dialog with one session of `target`, titled for `name`.
pub fn open(name: &str, target: Target, window: &mut Window, cx: &mut App) {
    super::load_fonts(cx);
    let deck = cx.new(|cx| Deck::new(target, window, cx));
    let footer = cx.new(|cx| StatusBar::new(deck.clone(), cx));
    let weak = deck.downgrade();
    let deck_weak = deck.downgrade();
    let on_close: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
        if let Some(d) = weak.upgrade() {
            d.read(cx).end_all(cx);
        }
    });
    let mut shown = name.to_string();
    if let Some(c) = shown.get(..1) {
        shown = format!("{}{}", c.to_uppercase(), &shown[1..]);
    }
    crate::modal::open(crate::modal::Modal { title: format!("Terminal: {shown}").into(), body: deck.into(), width: 1100., icon: Some(Icon::new(Lucide::SquareTerminal)), on_close: Some(on_close) }, cx);
    crate::modal::set_footer(footer.into(), cx);
    crate::modal::set_list_body(cx);
    crate::modal::offer_full_screen(cx);
    // A click beside the dialog does nothing; closing asks first while a
    // session runs.
    let weak = deck_weak;
    crate::modal::set_guard(
        crate::modal::ModalGuard {
            keep_on_backdrop: true,
            before_close: Some(Rc::new(move |_, cx| weak.upgrade().is_none_or(|d| d.update(cx, |d, cx| d.ask_close_all(cx))))),
        },
        cx,
    );
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings gpui's own `test` attribute macro in,
    // which shadows the standard one ("recursion limit reached while
    // expanding #[test]").
    use super::{end_question, indexed, ANSI};

    #[test]
    fn closing_running_sessions_asks_with_their_count() {
        assert_eq!(end_question(0, false), None);
        assert_eq!(end_question(0, true), None);
        assert_eq!(end_question(1, false).as_deref(), Some("End 1 running terminal session?"));
        assert_eq!(end_question(2, false).as_deref(), Some("End 2 running terminal sessions?"));
        assert_eq!(end_question(1, true).as_deref(), Some("End this terminal session?"));
    }

    #[test]
    fn the_256_colours_are_xterms() {
        assert_eq!(indexed(1), ANSI[1]);
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(21), 0x0000ff);
        assert_eq!(indexed(196), 0xff0000);
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
        assert_eq!(indexed(255), 0xeeeeee);
    }
}
