// Copyright © 2016-2026 The SENTIENT Authors

//! The manager's dialogs: a dim layer over the whole window and a panel
//! centred in the page area (right of the menu, below the top bar), or in
//! the window when it does not fit there. Escape, the close button and a
//! click on the dim layer close it; the body scrolls between a fixed title
//! and the buttons the content draws at its bottom.

use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

/// What a page asks to show.
#[derive(Clone)]
pub struct Modal {
    pub title: SharedString,
    pub body: AnyView,
    pub width: f32,
    /// An icon before the title (the mockup's dialogs).
    pub icon: Option<gpui_kit::component::Icon>,
    /// Called once when it closes by Escape, the close button or the dim layer.
    pub on_close: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

/// Buttons along the open dialog's bottom edge, edge to edge under a line,
/// outside the scrolling body (the mockup's dialogs): a view of their own
/// that follows the body's state. Set after [`open`]; [`open`] and
/// [`close`] clear it.
#[derive(Default)]
pub struct ModalFooter(pub Option<AnyView>);

impl Global for ModalFooter {}

pub fn set_footer(view: AnyView, cx: &mut App) {
    cx.set_global(ModalFooter(Some(view)));
    cx.refresh_windows();
}

/// The one dialog open now (the manager draws it).
#[derive(Default)]
pub struct ModalSlot(pub Option<Modal>);

impl Global for ModalSlot {}

/// Widen or narrow the open dialog (it glides to the new width).
pub fn set_width(width: f32, cx: &mut App) {
    if let Some(m) = cx.try_global::<ModalSlot>().and_then(|s| s.0.clone()) {
        cx.set_global(ModalSlot(Some(Modal { width, ..m })));
        cx.refresh_windows();
    }
}

pub fn open(modal: Modal, cx: &mut App) {
    cx.set_global(ModalFooter(None));
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
        cx.set_global(ModalFooter(None));
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
    let footer = cx.try_global::<ModalFooter>().and_then(|f| f.0.clone());
    let theme = cx.theme().clone();
    let vw = f32::from(window.viewport_size().width);
    let vh = f32::from(window.viewport_size().height);
    // A width change glides (260 ms), per dialog.
    let w = crate::ui::tween(SharedString::from(format!("modal-w-{}", modal.title)), modal.width, 260, window).min(vw - 32.);
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
                .gap_2()
                .when_some(modal.icon.clone(), |r, i| r.child(i.size_4().text_color(theme.muted_foreground)))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(modal.title.clone()))
                .child(crate::ui::button("modal-close").ghost().small().icon(IconName::Close).on_click(|_, window, cx| dismiss(window, cx))),
        )
        .child(
            // 20 px around the body (the mockup's dialog); the probe lets the
            // layout test check nothing reaches into it.
            div().id("modal-body").relative().flex_1().min_h_0().overflow_y_scroll().px(px(20.)).pt(px(12.)).pb(px(16.)).child(crate::ui::layout_probe("dialog")).child(modal.body.clone()),
        )
        .when_some(footer, |p, f| p.child(div().flex_shrink_0().px(px(20.)).py(px(14.)).border_t_1().border_color(theme.border).child(f)));
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
                    // The dialog rises 8 px as it fades in (180 ms).
                    .child({
                        let panel = panel.on_click(|_, _, cx| cx.stop_propagation()).relative();
                        if crate::ui::reduce_motion() {
                            panel.into_any_element()
                        } else {
                            panel
                                .with_animation("modal-appear", Animation::new(std::time::Duration::from_millis(180)).with_easing(crate::ui::ease_out), |el, t| el.opacity(t).top(px(8. * (1. - t))))
                                .into_any_element()
                        }
                    }),
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
    open(Modal { title, body: view.into(), width: 480., icon: None, on_close: None }, cx);
}

/// A dialog's destructive action: a solid red fill with white text, as tall
/// as Cancel (the toolkit's danger variant is a pale tint that reads as
/// disabled).
pub fn danger_button(theme: &gpui_kit::component::Theme, id: &'static str, label: SharedString, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(px(32.))
        .px_3()
        .items_center()
        .rounded(theme.radius)
        .bg(theme.danger)
        .text_color(gpui_kit::white())
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|d| d.opacity(0.9))
        .child(label)
        .on_click(on_click)
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
        let click = move |e: &ClickEvent, window: &mut Window, cx: &mut App| {
            let close_it = on_ok.as_ref().is_none_or(|f| f(e, window, cx));
            if close_it {
                close(cx);
            }
        };
        // The destructive action is the dialog's primary button: a solid red
        // fill with white text, as tall as Cancel (the toolkit's danger
        // variant is a pale tint that reads as disabled).
        let ok: AnyElement = if destructive {
            danger_button(&theme, "alert-ok", self.a.ok.clone(), click).into_any_element()
        } else {
            crate::ui::button("alert-ok").label(self.a.ok.clone()).primary().on_click(click).into_any_element()
        };
        v_flex()
            .gap_4()
            .child(div().text_sm().text_color(theme.muted_foreground).child(self.a.text.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(crate::ui::button("alert-cancel").outline().label("Cancel").on_click(|_, _, cx| close(cx)))
                    .child(ok),
            )
    }
}
