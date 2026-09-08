//! The go-to modal: type a timestamp, land on it. Ctrl+G, or the Playback
//! menu's own row.
//!
//! The seek strip already covers "somewhere around there" and the step keys
//! cover "a hair either way". This is the third case, the one neither of
//! them does: a time you already know, off a tracklist or a cue sheet or a
//! note you took, typed rather than aimed at.
//!
//! Modeled on the bookmark modal, down to the Enter binding and the footer.

use gpui::{
    actions, div, prelude::*, px, size, App, Bounds, Context, Div, Entity, FocusHandle, Focusable,
    KeyBinding, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};

use rox_core::fmt::fmt_time;
use rox_design::assets::icons;
use rox_design::{palette, tokens};
use rox_panel_api::panel::AppState;
use rox_panel_kit::ui::{kbd_line, section, small_button, Seg};
use rox_services::backdrop::WindowBackdrop;

actions!(goto_dialog, [Go]);

/// The key context the window's own bindings scope to.
const CONTEXT: &str = "GoToTime";

/// The modal's commit binding; call once at startup. Bound on the window
/// root so Enter goes wherever focus is.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", Go, Some(CONTEXT))]);
}

/// Open the modal over the playing track. Nothing playing opens nothing:
/// there's no track to move inside of, which is the same silence the
/// bookmark and A-B keys answer with.
pub fn open(state: AppState, cx: &mut App) {
    if state.player.read(cx).now_playing().is_none() {
        return;
    }
    let title = rox_i18n::t!("goto-window-title");
    let bounds = Bounds::centered(None, size(px(400.), px(240.)), cx);
    rox_panel_api::panel::open_child_window(cx, title, bounds, None, move |window, cx| {
        cx.new(|cx| GoToWindow::new(state, window, cx))
    });
}

struct GoToWindow {
    state: AppState,
    input: Entity<InputState>,
    backdrop: WindowBackdrop,
    _input_events: Subscription,
    /// The position line reads the clock, so it needs the player's own
    /// notify to stay live rather than freezing at the time the window
    /// opened. Same raw observe the seek strip runs on.
    _player: Subscription,
    /// This window pumps its own frames, so the backdrop needs its own wake on
    /// a new bake.
    _backdrop_changed: Subscription,
}

impl GoToWindow {
    fn new(state: AppState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Empty rather than seeded with the current time: a seeded field
        // puts the caret after four characters you have to clear before you
        // can type the one time you came here to type. The placeholder and
        // the line under it say where you are instead.
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(rox_i18n::t!("goto-placeholder")));
        let _input_events = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        let _player = cx.observe(&state.player, |_, _, cx| cx.notify());
        let _backdrop_changed = cx.observe(&state.now_art, |_, _, cx| cx.notify());
        window.focus(&input.read(cx).focus_handle(cx));
        GoToWindow {
            state,
            input,
            backdrop: WindowBackdrop::default(),
            _input_events,
            _player,
            _backdrop_changed,
        }
    }

    /// The time the field currently reads, clamped inside the track. None
    /// while the field is empty or holds something that isn't a time, which
    /// is what leaves Enter and the Go button inert.
    fn target(&self, cx: &App) -> Option<f64> {
        let secs = parse_time(&self.input.read(cx).value())?;
        let duration = self
            .state
            .player
            .read(cx)
            .now_playing()
            .and_then(|now| now.duration_secs);
        // A time past the end lands on the end rather than refusing: the
        // intent is legible, and a track whose tagged duration is short by
        // a second shouldn't reject the last second of itself.
        Some(match duration {
            Some(duration) => secs.min(duration),
            None => secs,
        })
    }

    /// Seek and close.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(secs) = self.target(cx) else {
            return;
        };
        self.state.player.read(cx).seek_to(secs);
        window.remove_window();
    }

    /// Where the playhead is now, and how long the track runs. The pair the
    /// typed time is aimed between.
    fn position_line(&self, cx: &App) -> Div {
        let line = match self.state.player.read(cx).now_playing() {
            Some(now) => match now.duration_secs {
                Some(duration) => rox_i18n::t!(
                    "goto-now-of",
                    time = fmt_time(now.position_secs),
                    duration = fmt_time(duration)
                ),
                None => rox_i18n::t!("goto-now", time = fmt_time(now.position_secs)),
            },
            None => rox_i18n::t!("goto-nothing-playing"),
        };
        div()
            .text_sm()
            .text_color(palette::text_muted())
            .child(line)
    }

    /// What Enter would do, read back: the time the field resolves to, or
    /// the complaint that it doesn't resolve to one. Blank while the field
    /// is empty, since an untouched field isn't a mistake.
    fn target_line(&self, cx: &App) -> Option<Div> {
        let typed = self.input.read(cx).value();
        if typed.trim().is_empty() {
            return None;
        }
        let (text, color) = match self.target(cx) {
            Some(secs) => (
                rox_i18n::t!("goto-target", time = fmt_time(secs)),
                palette::text_bright(),
            ),
            None => (rox_i18n::t!("goto-unreadable"), palette::tone_bad()),
        };
        Some(div().text_sm().text_color(color).child(text))
    }

    /// The window's own actions: the go, and the shortcut for it.
    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let hint = kbd_line([
            Seg::Text(rox_i18n::t!("goto-hint-before")),
            Seg::Key(rox_i18n::t!("goto-hint-key")),
            Seg::Text(rox_i18n::t!("goto-hint-after")),
        ])
        .text_xs();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(tokens::SPACE_SM)
            .px(tokens::SPACE_MD)
            .py(tokens::SPACE_SM)
            .border_t_1()
            .border_color(palette::border())
            .bg(palette::bg_panel())
            .child(hint)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tokens::SPACE_SM)
                    .child(small_button(
                        rox_i18n::t!("goto-go"),
                        icons::MOVE_HORIZONTAL,
                        false,
                        cx.listener(|this, _, window, cx| this.commit(window, cx)),
                    ))
                    .child(small_button(
                        rox_i18n::t!("settings-common-cancel"),
                        icons::CLOSE,
                        false,
                        cx.listener(|_, _, window, _| window.remove_window()),
                    )),
            )
    }
}

impl Focusable for GoToWindow {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for GoToWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Go, window, cx| this.commit(window, cx)))
            .bg(palette::bg_elevated())
            .text_color(palette::text_bright())
            .text_sm()
            .children(self.backdrop.layer(&self.state.now_art, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(tokens::SPACE_MD)
                    .flex()
                    .flex_col()
                    .gap(tokens::SPACE_MD)
                    .bg(palette::bg_elevated())
                    .child(section(
                        rox_i18n::t!("goto-time"),
                        None,
                        div()
                            .flex()
                            .flex_col()
                            .gap(tokens::SPACE_XS)
                            .child(Input::new(&self.input).w_full())
                            .child(self.position_line(cx))
                            .children(self.target_line(cx)),
                    )),
            )
            .child(self.footer(cx))
    }
}

/// A typed timestamp as seconds: plain seconds ("83"), minutes and seconds
/// ("1:23"), or hours in front of both ("1:02:03"), with a fraction allowed
/// on the last field either way ("1:23.5"). Fields over 60 read as written,
/// so "0:90" is a minute and a half rather than an error.
///
/// Anything else is None, which keeps a half-typed entry inert instead of
/// resolving it to a time nobody asked for.
fn parse_time(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let fields: Vec<&str> = text.split(':').collect();
    if fields.len() > 3 {
        return None;
    }
    let mut secs = 0.0f64;
    for (ix, field) in fields.iter().enumerate() {
        let field = field.trim();
        // Only the last field takes a fraction. A fractional minute is a
        // typo far more often than it's an intent.
        let value: f64 = if ix + 1 == fields.len() {
            field.parse().ok()?
        } else {
            field.parse::<u32>().ok()? as f64
        };
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        secs = secs * 60.0 + value;
    }
    Some(secs)
}

#[cfg(test)]
mod tests {
    use super::parse_time;

    #[test]
    fn plain_seconds_read_as_seconds() {
        assert_eq!(parse_time("83"), Some(83.0));
        assert_eq!(parse_time("  83  "), Some(83.0));
        assert_eq!(parse_time("0"), Some(0.0));
    }

    #[test]
    fn colons_read_as_minutes_and_hours() {
        assert_eq!(parse_time("1:23"), Some(83.0));
        assert_eq!(parse_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_time("0:07"), Some(7.0));
    }

    #[test]
    fn a_fraction_rides_the_last_field() {
        assert_eq!(parse_time("1:23.5"), Some(83.5));
        assert_eq!(parse_time("83.25"), Some(83.25));
        // Not on a minute, where it reads as a slip rather than a time.
        assert_eq!(parse_time("1.5:23"), None);
    }

    /// A field over 60 is unambiguous, so it's taken at face value rather
    /// than refused: "0:90" is a minute and a half.
    #[test]
    fn oversized_fields_read_as_written() {
        assert_eq!(parse_time("0:90"), Some(90.0));
        assert_eq!(parse_time("0:90:00"), Some(5400.0));
    }

    #[test]
    fn anything_that_isnt_a_time_is_nothing() {
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("   "), None);
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time("1:"), None);
        assert_eq!(parse_time(":30"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
        assert_eq!(parse_time("-30"), None);
        assert_eq!(parse_time("1:-30"), None);
        assert_eq!(parse_time("inf"), None);
        assert_eq!(parse_time("NaN"), None);
    }
}
