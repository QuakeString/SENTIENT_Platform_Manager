// Copyright © 2016-2026 The SENTIENT Authors

//! The manager's dialogs: a dim layer over the whole window and a panel
//! centred in the page area (right of the menu, below the top bar), or in
//! the window when it does not fit there. Escape, the close button and a
//! click on the dim layer close it; the body scrolls between a fixed title
//! and the buttons the content draws at its bottom.

use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, button::{Button, ButtonVariants as _}, h_flex, v_flex};
use gpui_kit::*;

/// What a page asks to show.
#[derive(Clone)]
pub struct Modal {
    pub title: SharedString,
    pub body: AnyView,
    pub width: f32,
    /// Called once when it closes by Escape, the close button or the dim layer.
    pub on_close: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// The one dialog open now (the manager draws it).
#[derive(Default)]
pub struct ModalSlot(pub Option<Modal>);

impl Global for ModalSlot {}

pub fn open(modal: Modal, cx: &mut App) {
    cx.set_global(ModalSlot(Some(modal)));
    cx.refresh_windows();
}

/// Change the open dialog's title (a job's result).
pub fn set_title(title: impl Into<SharedString>, cx: &mut App) {
    let title = title.into();
    let changed = cx.try_global::<ModalSlot>().and_then(|s| s.0.as_ref()).is_some_and(|m| m.title != title);
    if changed {
        if let Some(m) = cx.global_mut::<ModalSlot>().0.as_mut() {
            m.title = title;
        }
        cx.refresh_windows();
    }
}

/// Close without calling its on_close (the page closed it itself).
pub fn close(cx: &mut App) {
    if cx.try_global::<ModalSlot>().is_some_and(|s| s.0.is_some()) {
        cx.set_global(ModalSlot(None));
        cx.refresh_windows();
    }
}

/// Close it as the user did (Escape, ✕, the dim layer): calls on_close.
pub fn dismiss(window: &mut Window, cx: &mut App) {
    let closing = cx.try_global::<ModalSlot>().and_then(|s| s.0.clone());
    close(cx);
    if let Some(f) = closing.and_then(|m| m.on_close) {
        f(window, cx);
    }
}

/// The layer, or nothing. `page_left` and `page_top` are where the page
/// area starts (menu width, top bar height).
pub fn layer(window: &mut Window, cx: &mut App, page_left: f32, page_top: f32) -> Option<AnyElement> {
    let modal = cx.try_global::<ModalSlot>().and_then(|s| s.0.clone())?;
    let theme = cx.theme().clone();
    let vw = f32::from(window.viewport_size().width);
    let vh = f32::from(window.viewport_size().height);
    let w = modal.width.min(vw - 32.);
    // Centre in the page area when the panel fits there, else in the window.
    let (area_left, area_w) = if vw - page_left >= w + 32. { (page_left, vw - page_left) } else { (0., vw) };
    let left = area_left + (area_w - w) / 2.;
    let max_h = (vh - page_top - 32.).max(240.);
    let top_space = page_top + 16.;
    let panel = v_flex()
        .id("modal-panel")
        .occlude()
        .w(px(w))
        .max_h(px(max_h))
        .rounded(px(crate::ui::PANEL_RADIUS))
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .shadow_lg()
        .child(
            h_flex()
                .h(px(48.))
                .flex_shrink_0()
                .px_4()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(modal.title.clone()))
                .child(Button::new("modal-close").ghost().small().icon(IconName::Close).on_click(|_, window, cx| dismiss(window, cx))),
        )
        .child(div().id("modal-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(modal.body.clone()));
    Some(
        div()
            .id("modal-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(black().opacity(if crate::theme::is_dark(cx) { 0.55 } else { 0.40 }))
            .on_click(|_, window, cx| dismiss(window, cx))
            .child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top_space))
                    .bottom(px(16.))
                    .w(px(w))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(crate::ui::appear(panel.on_click(|_, _, cx| cx.stop_propagation()), "modal-appear", 160)),
            )
            .into_any_element(),
    )
}

/// A confirmation in the shared dialog: title, text, Cancel and the action.
/// Built like the toolkit's alert dialog, so callers read the same.
pub struct Alert {
    title: SharedString,
    text: SharedString,
    ok: SharedString,
    on_ok: Option<std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) -> bool>>,
}

impl Alert {
    pub fn title(mut self, t: impl Into<SharedString>) -> Self {
        self.title = t.into();
        self
    }
    pub fn description(mut self, t: impl Into<SharedString>) -> Self {
        self.text = t.into();
        self
    }
    /// Cancel is always offered.
    pub fn confirm(self) -> Self {
        self
    }
    pub fn ok_text(mut self, t: impl Into<SharedString>) -> Self {
        self.ok = t.into();
        self
    }
    /// Return true to close the dialog.
    pub fn on_ok(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static) -> Self {
        self.on_ok = Some(std::rc::Rc::new(f));
        self
    }
}

pub fn alert(window: &mut Window, cx: &mut App, build: impl FnOnce(Alert, &mut Window, &mut App) -> Alert) {
    let a = build(Alert { title: "".into(), text: "".into(), ok: "OK".into(), on_ok: None }, window, cx);
    let title = a.title.clone();
    let view = cx.new(|_| AlertBody { a });
    open(Modal { title, body: view.into(), width: 480., on_close: None }, cx);
}

struct AlertBody {
    a: Alert,
}

impl Render for AlertBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Taking something away reads red; anything else is the primary action.
        let lower = self.a.ok.to_lowercase();
        let destructive = ["delete", "remove", "uninstall", "stop", "replace", "sign everyone out", "sign out"].iter().any(|w| lower.starts_with(w));
        let on_ok = self.a.on_ok.clone();
        let ok = Button::new("alert-ok").label(self.a.ok.clone()).on_click(move |e, window, cx| {
            let close_it = on_ok.as_ref().is_none_or(|f| f(e, window, cx));
            if close_it {
                close(cx);
            }
        });
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(theme.muted_foreground).child(self.a.text.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("alert-cancel").outline().label("Cancel").on_click(|_, _, cx| close(cx)))
                    .child(if destructive { ok.danger() } else { ok.primary() }),
            )
    }
}
