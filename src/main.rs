mod calc;
mod storage;
mod theme;

use std::{collections::VecDeque, time::Duration};

use calc::{Base, Calculation, Calculator, Op, Value, WordSize, format_value};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Div, DragMoveEvent, Empty,
    FocusHandle, FontWeight, KeyDownEvent, Pixels, Rgba, SharedString, Stateful, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use gpui_platform::application;
use theme::{Theme, ThemeWatcher};

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Digit(u8),
    Decimal,
    Op(Op),
    Equals,
    Clear,
    Backspace,
    Negate,
    Percent,
    Open,
    Close,
    Not,
}

impl Key {
    fn label(self) -> SharedString {
        match self {
            Self::Digit(d) => char::from_digit(d.into(), 16)
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string()
                .into(),
            Self::Decimal => ".".into(),
            Self::Op(op) => op.symbol().into(),
            Self::Equals => "=".into(),
            // Not "C", which is a hex digit.
            Self::Clear => "AC".into(),
            Self::Backspace => "⌫".into(),
            Self::Negate => "±".into(),
            Self::Percent => "%".into(),
            Self::Open => "(".into(),
            Self::Close => ")".into(),
            Self::Not => "NOT".into(),
        }
    }

    /// Whether the key does anything in `base`.
    fn enabled(self, base: Base) -> bool {
        match self {
            Self::Digit(d) => u32::from(d) < base.radix(),
            Self::Decimal | Self::Percent => !base.integer(),
            Self::Op(op) => !op.integer_only() || base.integer(),
            Self::Not => base.integer(),
            _ => true,
        }
    }

    /// Map a physical keystroke to a calculator key. In hex, `a` to `f` are digits, so `c`
    /// is the digit C rather than clear.
    fn from_keystroke(event: &KeyDownEvent, base: Base) -> Option<Self> {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "enter" => return Some(Self::Equals),
            "backspace" => return Some(Self::Backspace),
            "escape" | "delete" => return Some(Self::Clear),
            _ => {}
        }
        let ch = keystroke.key_char.as_deref().unwrap_or(&keystroke.key);
        let mut chars = ch.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(d) = c.to_digit(base.radix()).and_then(|d| u8::try_from(d).ok())
        {
            return Some(Self::Digit(d));
        }
        Some(match ch {
            "." | "," => Self::Decimal,
            "+" => Self::Op(Op::Add),
            "-" => Self::Op(Op::Sub),
            "*" | "x" => Self::Op(Op::Mul),
            "/" => Self::Op(Op::Div),
            "=" => Self::Equals,
            "%" if base.integer() => Self::Op(Op::Mod),
            "%" => Self::Percent,
            "&" => Self::Op(Op::And),
            "|" => Self::Op(Op::Or),
            "^" => Self::Op(Op::Xor),
            "<" => Self::Op(Op::Shl),
            ">" => Self::Op(Op::Shr),
            "~" => Self::Not,
            "(" => Self::Open,
            ")" => Self::Close,
            "c" => Self::Clear,
            "n" => Self::Negate,
            _ => return None,
        })
    }
}

/// A key on the keypad grid and how many columns and rows it spans.
#[derive(Clone, Copy)]
struct Cell {
    key: Key,
    cols: u16,
    rows: u16,
}

const fn cell(key: Key) -> Cell {
    Cell {
        key,
        cols: 1,
        rows: 1,
    }
}

const GRID_COLS: u16 = 4;
const GRID_ROWS: u16 = 6;

/// The keypad, row by row; the grid places each cell in the first free slot.
const ROWS: [&[Cell]; GRID_ROWS as usize] = [
    &[
        cell(Key::Clear),
        cell(Key::Backspace),
        cell(Key::Percent),
        cell(Key::Op(Op::Div)),
    ],
    &[
        cell(Key::Open),
        cell(Key::Close),
        cell(Key::Negate),
        cell(Key::Op(Op::Mul)),
    ],
    &[
        cell(Key::Digit(7)),
        cell(Key::Digit(8)),
        cell(Key::Digit(9)),
        cell(Key::Op(Op::Sub)),
    ],
    &[
        cell(Key::Digit(4)),
        cell(Key::Digit(5)),
        cell(Key::Digit(6)),
        cell(Key::Op(Op::Add)),
    ],
    &[
        cell(Key::Digit(1)),
        cell(Key::Digit(2)),
        cell(Key::Digit(3)),
        Cell {
            key: Key::Equals,
            cols: 1,
            rows: 2,
        },
    ],
    &[
        Cell {
            key: Key::Digit(0),
            cols: 2,
            rows: 1,
        },
        cell(Key::Decimal),
    ],
];

/// In the integer bases, a column of bitwise keys goes in front of the keypad, and `%` is
/// replaced by `MOD`.
const BITWISE_KEYS: [Key; GRID_ROWS as usize] = [
    Key::Op(Op::Shl),
    Key::Op(Op::Shr),
    Key::Op(Op::And),
    Key::Op(Op::Or),
    Key::Op(Op::Xor),
    Key::Not,
];

/// In hex, a column of A to F goes between the bitwise keys and the digits.
const HEX_DIGITS: [u8; GRID_ROWS as usize] = [10, 11, 12, 13, 14, 15];

/// Drag payload for the history drawer's resize handle.
struct DrawerHandle;

struct CalculatorView {
    calc: Calculator,
    focus_handle: FocusHandle,
    theme: Theme,
    /// The theme's font, if it is installed.
    font: Option<SharedString>,
    /// Finished calculations, oldest first.
    history: VecDeque<Calculation>,
    history_open: bool,
    /// Set by dragging the drawer's left edge; kept across close and reopen.
    drawer_width: Pixels,
    /// The window's width before and after it was widened to make room for the drawer, to
    /// undo on close.
    widened: Option<(Pixels, Pixels)>,
    /// In the integer bases, show the bits of the number in place of the keypad.
    show_bits: bool,
}

/// How often to check DMS's palette files for changes.
const THEME_POLL: Duration = Duration::from_secs(1);
const MAX_HISTORY: usize = 100;
const DRAWER_WIDTH: Pixels = px(280.);
/// Narrowest the drawer can be dragged; it stays this wide until the pointer passes
/// `DRAWER_CLOSE_AT`, then it closes.
const DRAWER_MIN_WIDTH: Pixels = px(200.);
const DRAWER_CLOSE_AT: Pixels = px(140.);
/// Width of the grab area on the drawer's left edge.
const DRAWER_HANDLE_WIDTH: Pixels = px(8.);
/// Room the keypad keeps when the drawer is dragged wider.
const KEYPAD_DRAG_MIN_WIDTH: Pixels = px(220.);
/// Narrowest the keypad may get when the drawer opens before the window is widened.
const KEYPAD_MIN_WIDTH: Pixels = px(300.);

impl CalculatorView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        // Repaint when DMS regenerates its palette (wallpaper or theme change). Watching starts
        // before the first load below, so a change in between isn't missed.
        let mut watcher = ThemeWatcher::new();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(THEME_POLL).await;
                if !watcher.changed() {
                    continue;
                }
                // A palette caught mid-write doesn't read; keep the current one until it does.
                let Some(theme) = Theme::load_dms() else {
                    continue;
                };
                if this
                    .update(cx, |view, cx| view.set_theme(theme, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut view = Self {
            calc: Calculator::new(),
            focus_handle,
            theme: Theme::fallback(),
            font: None,
            history: storage::load_history(MAX_HISTORY),
            history_open: false,
            drawer_width: DRAWER_WIDTH,
            widened: None,
            show_bits: false,
        };
        view.set_theme(Theme::load(), cx);
        view
    }

    fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.font = theme
            .font_family
            .clone()
            .filter(|family| cx.text_system().all_font_names().contains(family))
            .map(SharedString::from);
        self.theme = theme;
        cx.notify();
    }

    fn press(&mut self, key: Key, cx: &mut Context<Self>) {
        if !key.enabled(self.calc.base()) {
            return;
        }
        let c = &mut self.calc;
        match key {
            Key::Digit(d) => c.digit(d),
            Key::Decimal => c.decimal(),
            Key::Op(op) => c.operator(op),
            Key::Equals => {
                if let Some(calculation) = c.equals() {
                    if self.history.len() == MAX_HISTORY {
                        self.history.pop_front();
                    }
                    self.history.push_back(calculation);
                    storage::save_history(&self.history);
                }
            }
            Key::Clear => c.clear(),
            Key::Backspace => c.backspace(),
            Key::Negate => c.negate(),
            Key::Percent => c.percent(),
            Key::Open => c.open_paren(),
            Key::Close => c.close_paren(),
            Key::Not => c.not(),
        }
        cx.notify();
    }

    fn recall(&mut self, value: Value, cx: &mut Context<Self>) {
        self.calc.recall(value);
        cx.notify();
    }

    fn delete_history(&mut self, index: usize, cx: &mut Context<Self>) {
        self.history.remove(index);
        storage::save_history(&self.history);
        cx.notify();
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.history.clear();
        storage::save_history(&self.history);
        cx.notify();
    }

    fn memory(&mut self, action: Memory, cx: &mut Context<Self>) {
        let c = &mut self.calc;
        match action {
            Memory::Clear => c.memory_clear(),
            Memory::Recall => c.memory_recall(),
            Memory::Add => c.memory_add(false),
            Memory::Subtract => c.memory_add(true),
            Memory::Store => c.memory_store(),
        }
        cx.notify();
    }

    fn copy(&self, cx: &Context<Self>) {
        if let Some(text) = self.calc.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
            && self.calc.paste(&text)
        {
            cx.notify();
        }
    }

    fn toggle_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_history_open(!self.history_open, window, cx);
    }

    fn set_history_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open == self.history_open {
            return;
        }
        self.history_open = open;
        // Keep the keypad usable in a floating window. Tiling compositors ignore this.
        let viewport = window.viewport_size();
        if open {
            let shortfall = KEYPAD_MIN_WIDTH + self.drawer_width - viewport.width;
            if shortfall > px(0.) {
                let widened = viewport.width + shortfall;
                // Applied later, if at all, so it can only be checked on close.
                window.resize(size(widened, viewport.height));
                self.widened = Some((viewport.width, widened));
            }
        } else if let Some((width, widened)) = self.widened.take()
            // Only undo a widening that was honored and not resized since.
            && (viewport.width - widened).abs() < px(1.)
        {
            window.resize(size(width, viewport.height));
        }
        cx.notify();
    }

    /// Follows the pointer while the drawer's edge is dragged; closes the drawer once it is
    /// dragged past `DRAWER_CLOSE_AT`.
    fn resize_drawer(
        &mut self,
        event: &DragMoveEvent<DrawerHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.history_open {
            return;
        }
        // The root fills the window and the drawer is flush with its right edge.
        let width = event.bounds.right() - event.event.position.x;
        if width < DRAWER_CLOSE_AT {
            self.set_history_open(false, window, cx);
            return;
        }
        let max = (event.bounds.size.width - KEYPAD_DRAG_MIN_WIDTH).max(DRAWER_MIN_WIDTH);
        self.drawer_width = width.clamp(DRAWER_MIN_WIDTH, max);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if m.alt || m.platform {
            return;
        }
        // Shortcuts as in Windows' calculator.
        if m.control {
            match key {
                "c" => self.copy(cx),
                "v" => self.paste(cx),
                "l" => self.memory(Memory::Clear, cx),
                "r" => self.memory(Memory::Recall, cx),
                "p" => self.memory(Memory::Add, cx),
                "q" => self.memory(Memory::Subtract, cx),
                "m" => self.memory(Memory::Store, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let base = match key {
            "f5" => Some(Base::Hex),
            "f6" => Some(Base::Dec),
            "f7" => Some(Base::Oct),
            "f8" => Some(Base::Bin),
            _ => None,
        };
        let word = match key {
            "f12" => Some(WordSize::Qword),
            "f2" => Some(WordSize::Dword),
            "f3" => Some(WordSize::Word),
            "f4" => Some(WordSize::Byte),
            _ => None,
        };
        if base.is_some() || word.is_some() {
            base.inspect(|&base| self.set_base(base, cx));
            word.inspect(|&word| self.set_word(word, cx));
            cx.stop_propagation();
            return;
        }
        if key == "h" && !m.shift {
            self.toggle_history(window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(key) = Key::from_keystroke(event, self.calc.base()) {
            self.press(key, cx);
            cx.stop_propagation();
        }
    }

    fn set_base(&mut self, base: Base, cx: &mut Context<Self>) {
        self.calc.set_base(base);
        cx.notify();
    }

    fn set_word(&mut self, word: WordSize, cx: &mut Context<Self>) {
        self.calc.set_word(word);
        cx.notify();
    }

    fn toggle_bit(&mut self, bit: u32, cx: &mut Context<Self>) {
        self.calc.toggle_bit(bit);
        cx.notify();
    }

    fn button(&self, index: usize, cell: Cell, cx: &Context<Self>) -> Stateful<Div> {
        let key = cell.key;
        let enabled = key.enabled(self.calc.base());
        let t = &self.theme;
        let colors = match key {
            Key::Op(op) if self.calc.pending_op() == Some(op) => t.operator_active,
            Key::Op(_) | Key::Equals => t.operator,
            Key::Clear
            | Key::Backspace
            | Key::Negate
            | Key::Percent
            | Key::Open
            | Key::Close
            | Key::Not => t.function,
            Key::Digit(_) | Key::Decimal => t.digit,
        };
        let label = key.label();
        // Word labels such as AND are smaller to fit the narrower programmer keys.
        let word = label.len() > 1 && label.chars().all(|c| c.is_ascii_alphabetic());
        let text_size = if word { 16. } else { 26. };

        div()
            .id(("key", index))
            .col_span(cell.cols)
            .row_span(cell.rows)
            .flex()
            .items_center()
            .justify_center()
            .rounded_xl()
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(px(text_size))
            .font_weight(FontWeight::MEDIUM)
            .child(label)
            // Material 3 disabled state.
            .when(!enabled, |button| button.opacity(0.38))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |s| s.bg(colors.hover))
                    .active(move |s| s.bg(colors.pressed))
                    .on_click(cx.listener(move |this, _, _, cx| this.press(key, cx)))
            })
    }

    /// Segmented control, e.g. for the number base. Labels double as element IDs, so they
    /// must be unique in the window.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        options: [T; 4],
        current: T,
        label: fn(T) -> &'static str,
        select: fn(&mut Self, T, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> Div {
        let t = &self.theme;
        div()
            .flex()
            .p_0p5()
            .rounded_full()
            .bg(t.digit.bg)
            .children(options.map(|option| {
                let selected = option == current;
                let colors = if selected { t.function } else { t.digit };
                div()
                    .id(label(option))
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(selected, |chip| chip.bg(colors.bg).text_color(colors.fg))
                    .when(!selected, |chip| {
                        chip.text_color(t.history)
                            .cursor_pointer()
                            .hover(move |s| s.bg(colors.hover))
                            .active(move |s| s.bg(colors.pressed))
                    })
                    .child(label(option))
                    .on_click(cx.listener(move |this, _, _, cx| select(this, option, cx)))
            }))
    }

    /// The number on screen in every base; clicking one switches to it.
    fn readout(&self, cx: &Context<Self>) -> Div {
        let t = &self.theme;
        let value = self.calc.value();
        let word = self.calc.word();
        div()
            .w_full()
            .flex()
            .flex_col()
            .px_2()
            .children(Base::ALL.map(|base| {
                let current = base == self.calc.base();
                let text = value.map_or_else(String::new, |v| format_value(v, base, word));
                div()
                    .id(("readout", base.radix() as usize))
                    .flex()
                    .gap_2()
                    .px_1()
                    .rounded_md()
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(move |s| s.bg(t.digit.bg))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(30.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if current { t.accent } else { t.history })
                            .child(base.label()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(if current { t.display } else { t.history })
                            .child(text),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_base(base, cx)))
            }))
    }

    /// The bits of the number on screen, 16 to a row; clicking one flips it.
    fn bits_panel(&self, cx: &Context<Self>) -> Div {
        let t = &self.theme;
        let word = self.calc.word();
        let bits = match self.calc.value() {
            Some(Value::Int(n)) => word.unsigned(n),
            _ => 0,
        };
        let per_row = word.bits().min(16);
        let rows = (0..word.bits() / per_row).map(|row| {
            let top = word.bits() - 1 - row * per_row;
            div()
                .w_full()
                .max_w(px(560.))
                .flex()
                .gap_3()
                .children((0..per_row / 4).map(|nibble| {
                    let high = top - nibble * 4;
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(div().w_full().flex().children((0..4).map(|i| {
                            let bit = high - i;
                            let set = bits >> bit & 1 == 1;
                            div()
                                .id(("bit", bit as usize))
                                .flex_1()
                                .flex()
                                .justify_center()
                                .py_1()
                                .rounded_md()
                                .text_size(px(20.))
                                .font_weight(if set {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(if set { t.accent } else { t.history })
                                .cursor_pointer()
                                .hover(move |s| s.bg(t.digit.bg))
                                .active(move |s| s.bg(t.digit.hover))
                                .child(if set { "1" } else { "0" })
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.toggle_bit(bit, cx)),
                                )
                        })))
                        // The index of the lowest bit, under it.
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .children((0..3).map(|_| div().flex_1()))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .justify_center()
                                        .text_size(px(10.))
                                        .text_color(t.history)
                                        .child((high - 3).to_string()),
                                ),
                        )
                }))
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .children(rows)
    }

    /// MC MR M+ M− MS.
    fn memory_row(&self, cx: &Context<Self>) -> Div {
        let t = &self.theme;
        let has_memory = self.calc.has_memory();
        div()
            .flex()
            .justify_around()
            .children(Memory::ALL.map(|action| {
                let enabled = has_memory || !matches!(action, Memory::Clear | Memory::Recall);
                div()
                    .id(action.label())
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.history)
                    .child(action.label())
                    .when(!enabled, |button| button.opacity(0.38))
                    .when(enabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |s| s.bg(t.digit.bg))
                            .active(move |s| s.bg(t.digit.hover))
                            .on_click(cx.listener(move |this, _, _, cx| this.memory(action, cx)))
                    })
            }))
    }

    fn text_button(
        &self,
        id: &'static str,
        label: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.theme.accent)
            .cursor_pointer()
            .hover(|s| s.opacity(0.75))
            .active(|s| s.opacity(0.5))
            .child(label)
            .on_click(on_click)
    }

    /// One history entry: click to recall its result, hover for a delete button.
    fn history_item(
        &self,
        i: usize,
        calculation: &Calculation,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let t = &self.theme;
        let item = t.drawer;
        let value = calculation.result;
        let result = format_value(value, calculation.base, calculation.word);
        // Shrink long results rather than break them mid-number; binary still wraps at its
        // digit groups.
        let result_size = match result.chars().count() {
            0..=14 => 24.,
            15..=20 => 18.,
            _ => 14.,
        };
        // E.g. `HEX`, or `HEX BYTE` for a word size other than the default.
        let base = calculation.base.integer().then(|| calculation.base.label());
        let word = (base.is_some() && calculation.word != WordSize::Qword)
            .then(|| calculation.word.label());
        let delete = div()
            .id(("delete", i))
            .absolute()
            .top_1()
            .left_1()
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_size(px(14.))
            .text_color(t.history)
            .invisible()
            .group_hover("history-item", gpui::Styled::visible)
            .hover(move |s| s.bg(item.pressed))
            .cursor_pointer()
            .child("✕")
            .on_click(cx.listener(move |this, _, _, cx| {
                // Don't also recall the entry.
                cx.stop_propagation();
                this.delete_history(i, cx);
            }));
        div()
            .id(("history", i))
            .group("history-item")
            .relative()
            .flex()
            .flex_col()
            .items_end()
            // Room for the delete button.
            .pl_8()
            .pr_3()
            .py_2()
            .rounded_lg()
            .cursor_pointer()
            .hover(move |s| s.bg(item.hover))
            .active(move |s| s.bg(item.pressed))
            // Long expressions and results wrap within the drawer.
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_baseline()
                    .gap_1()
                    .text_size(px(14.))
                    .text_color(t.history)
                    .children(base.into_iter().chain(word).map(|tag| {
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tag)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ml_1()
                            .text_right()
                            .child(SharedString::from(&calculation.expression)),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_right()
                    .text_size(px(result_size))
                    .text_color(t.display)
                    .child(result),
            )
            .child(delete)
            .on_click(cx.listener(move |this, _, _, cx| this.recall(value, cx)))
    }

    fn history_drawer(&self, cx: &Context<Self>) -> Div {
        let t = &self.theme;
        let item = t.drawer;

        let header = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(px(48.))
            .pl_4()
            .pr_2()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.display)
                    .child("History"),
            )
            .when(!self.history.is_empty(), |header| {
                header.child(self.text_button(
                    "clear-history",
                    "Clear",
                    cx.listener(|this, _, _, cx| this.clear_history(cx)),
                ))
            });

        let body = if self.history.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .text_color(t.history)
                .child("No calculations yet")
                .into_any_element()
        } else {
            div()
                .id("history-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_2()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1()
                .children(
                    self.history
                        .iter()
                        .enumerate()
                        .rev()
                        .map(|(i, calculation)| self.history_item(i, calculation, cx)),
                )
                .into_any_element()
        };

        let handle_hover = Rgba { a: 0.5, ..t.accent };
        let handle = div()
            .id("drawer-handle")
            .absolute()
            .top_0()
            .left(-DRAWER_HANDLE_WIDTH / 2.)
            .w(DRAWER_HANDLE_WIDTH)
            .h_full()
            .cursor_col_resize()
            .hover(move |s| s.bg(handle_hover))
            .on_drag(DrawerHandle, |_, _, _, cx| cx.new(|_| Empty));

        div()
            .relative()
            .w(self.drawer_width)
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(item.bg)
            .child(header)
            .child(body)
            .child(handle)
    }
    /// The expression line and the big number.
    fn numbers(&self) -> Div {
        let t = &self.theme;
        let display = self.calc.display();
        // Shrink long numbers so they fit.
        let display_size = match display.chars().count() {
            0..=9 => 56.,
            10..=13 => 42.,
            14..=18 => 32.,
            19..=26 => 24.,
            _ => 18.,
        };
        let memory_badge = self.calc.has_memory().then(|| {
            div()
                .flex_shrink_0()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.accent)
                .child("M")
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .px_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_start()
                    .gap_2()
                    .children(memory_badge)
                    .child(
                        // Long expressions wrap onto more lines.
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h(px(24.))
                            .text_right()
                            .text_size(px(18.))
                            .text_color(t.history)
                            .child(self.calc.pending_expression()),
                    ),
            )
            .child(
                // Long binary numbers wrap.
                div()
                    .w_full()
                    .text_right()
                    .text_size(px(display_size))
                    .font_weight(FontWeight::LIGHT)
                    .text_color(t.display)
                    .child(display),
            )
    }

    /// Selectors, readout, expression and the big number.
    fn display_area(&self, cx: &Context<Self>) -> Div {
        let base = self.calc.base();
        let integer = base.integer();

        let toggle = self.text_button(
            "toggle-history",
            if self.history_open {
                "Hide history"
            } else {
                "History"
            },
            cx.listener(|this, _, window, cx| this.toggle_history(window, cx)),
        );
        let top_row = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .child(self.segmented(Base::ALL, base, Base::label, Self::set_base, cx))
            .child(toggle);

        let programmer_row = integer.then(|| {
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(self.segmented(
                    WordSize::ALL,
                    self.calc.word(),
                    WordSize::label,
                    Self::set_word,
                    cx,
                ))
                .child(self.text_button(
                    "toggle-bits",
                    if self.show_bits { "Keypad" } else { "Bits" },
                    cx.listener(|this, _, _, cx| {
                        this.show_bits = !this.show_bits;
                        cx.notify();
                    }),
                ))
        });

        div()
            .flex_shrink_0()
            .min_h(px(240.))
            .flex()
            .flex_col()
            .items_end()
            .justify_between()
            .gap_2()
            .child(top_row)
            .children(programmer_row)
            .children(integer.then(|| self.readout(cx)))
            .child(self.numbers())
    }

    /// The keypad grid, or the bit grid in its place.
    fn keys(&self, cx: &Context<Self>) -> AnyElement {
        let base = self.calc.base();
        let integer = base.integer();
        let hex = base == Base::Hex;
        let cols = GRID_COLS + u16::from(integer) + u16::from(hex);
        if integer && self.show_bits {
            return self.bits_panel(cx).into_any_element();
        }
        // Built lazily, so a frame allocates no list of cells.
        let cells = ROWS
            .iter()
            .zip(BITWISE_KEYS.iter().zip(HEX_DIGITS))
            .flat_map(|(row, (&bitwise, letter))| {
                let bitwise = integer.then(|| cell(bitwise));
                let letter = hex.then(|| cell(Key::Digit(letter)));
                let row = row.iter().map(move |&c| match c.key {
                    Key::Percent if integer => Cell {
                        key: Key::Op(Op::Mod),
                        ..c
                    },
                    _ => c,
                });
                bitwise.into_iter().chain(letter).chain(row)
            });
        div()
            .flex_1()
            .min_h_0()
            .grid()
            .grid_cols(cols)
            .grid_rows(GRID_ROWS)
            .when(cols >= 6, gpui::Styled::gap_2)
            .when(cols < 6, gpui::Styled::gap_3)
            .children(
                cells
                    .enumerate()
                    .map(|(index, cell)| self.button(index, cell, cx)),
            )
            .into_any_element()
    }
}

impl Render for CalculatorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let keypad = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(self.display_area(cx))
            .child(self.memory_row(cx))
            .child(self.keys(cx));

        let mut root = div().id("calculator");
        if let Some(font) = self.font.clone() {
            root = root.font_family(font);
        }
        root.track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drag_move(cx.listener(Self::resize_drawer))
            .size_full()
            .flex()
            .bg(self.theme.background)
            .child(keypad)
            .when(self.history_open, |root| {
                root.child(self.history_drawer(cx))
            })
    }
}

/// The memory keys.
#[derive(Clone, Copy)]
enum Memory {
    Clear,
    Recall,
    Add,
    Subtract,
    Store,
}

impl Memory {
    const ALL: [Self; 5] = [
        Self::Clear,
        Self::Recall,
        Self::Add,
        Self::Subtract,
        Self::Store,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Clear => "MC",
            Self::Recall => "MR",
            Self::Add => "M+",
            Self::Subtract => "M−",
            Self::Store => "MS",
        }
    }
}

fn main() {
    application().run(|cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(360.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(280.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Calculator".into()),
                    ..Default::default()
                }),
                app_id: Some("calculator".into()),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| CalculatorView::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
