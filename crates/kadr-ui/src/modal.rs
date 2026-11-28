//! Shared modal geometry and keyboard focus lifecycle.
use super::*;

#[derive(Default)]
pub(super) struct ModalFocus {
    handle: Option<FocusHandle>,
    opener: Option<FocusHandle>,
    active: bool,
}

impl ModalFocus {
    /// Call before assigning focus to a modal's search or login field.
    pub(super) fn update(&mut self, active: bool, window: &mut Window, cx: &mut Context<KadrView>) {
        let handle = self.handle.get_or_insert_with(|| cx.focus_handle());
        if active && !self.active {
            self.opener = window.focused(cx);
            window.focus(handle);
        } else if !active && self.active {
            if let Some(opener) = self.opener.take() {
                window.focus(&opener);
            }
        } else if active && !handle.contains_focused(window, cx) {
            window.focus(handle);
        }
        self.active = active;
    }

    pub(super) fn handle(&self) -> Option<&FocusHandle> {
        self.handle.as_ref()
    }

    /// GPUI tab groups order controls but do not trap focus. Skip background
    /// stops in either direction and detect wraparound if no modal stop exists.
    pub(super) fn cycle(&self, backwards: bool, window: &mut Window, cx: &gpui::App) {
        let Some(handle) = &self.handle else { return };
        let mut first = None;
        loop {
            if backwards {
                window.focus_prev();
            } else {
                window.focus_next();
            }
            if handle.contains_focused(window, cx) {
                return;
            }
            let Some(focused) = window.focused(cx) else {
                break;
            };
            if first
                .as_ref()
                .is_some_and(|first: &FocusHandle| first.is_focused(window))
            {
                break;
            }
            first.get_or_insert(focused);
        }
        window.focus(handle);
    }
}

pub(super) fn surface(
    viewport_width: f32,
    viewport_height: f32,
    preferred_width: f32,
    preferred_height: f32,
    header: Div,
    body: Stateful<Div>,
    footer: Div,
) -> Div {
    let (width, height) = bounded_size(
        viewport_width,
        viewport_height,
        preferred_width,
        preferred_height,
    );
    div()
        .flex()
        .flex_col()
        .w(px(width))
        .max_h(px(height))
        .min_w_0()
        .min_h_0()
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(0x404046))
        .bg(rgb(PANEL))
        .shadow_lg()
        .overflow_hidden()
        .child(header.flex_shrink_0())
        .child(body.flex_auto().min_h_0().min_w_0())
        .child(footer.flex_shrink_0())
}

fn bounded_size(
    width: f32,
    height: f32,
    preferred_width: f32,
    preferred_height: f32,
) -> (f32, f32) {
    (
        preferred_width.min((width - 32.).max(0.)),
        preferred_height.min((height - 32.).max(0.)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_geometry_never_exceeds_viewport_even_when_short() {
        assert_eq!(bounded_size(1440., 900., 840., 640.), (840., 640.));
        assert_eq!(bounded_size(640., 300., 840., 640.), (608., 268.));
        assert_eq!(bounded_size(20., 12., 840., 640.), (0., 0.));
    }
}
