use std::{cell::Cell, process::ExitCode, rc::Rc};

use gpui::{
    App, Application, Bounds, Context, KeyBinding, Menu, MenuItem, TitlebarOptions, Window,
    WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};

actions!(kadr, [Quit]);

struct Kadr;

impl Render for Kadr {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .gap_4()
            .bg(rgb(0x18181b))
            .text_color(rgb(0xf4f4f5))
            .child(div().text_3xl().child("kadr"))
            .child("Фильмы и сериалы в локальном приложении")
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa1a1aa))
                    .child("P0 · Прототип окна GPUI. Воспроизведение пока недоступно."),
            )
    }
}

fn request_quit(cx: &App) {
    // Run outside platform callbacks: startup can lose an early quit, and X11 holds a borrow.
    cx.spawn(async |cx| cx.update(|cx| cx.quit()))
        .detach_and_log_err(cx);
}

fn main() -> ExitCode {
    let exit_code = Rc::new(Cell::new(ExitCode::SUCCESS));
    let startup_exit_code = Rc::clone(&exit_code);

    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| request_quit(cx));
        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.set_menus(vec![Menu {
            name: "kadr".into(),
            items: vec![MenuItem::action("Quit kadr", Quit)],
        }]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                request_quit(cx);
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.0), px(360.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("kadr".into()),
                ..Default::default()
            }),
            app_id: Some("kadr".into()),
            ..Default::default()
        };

        if let Err(error) = cx.open_window(options, |window, cx| {
            window.set_window_title("kadr");
            cx.new(|_| Kadr)
        }) {
            eprintln!("Не удалось открыть окно kadr: {error:#}");
            startup_exit_code.set(ExitCode::FAILURE);
            request_quit(cx);
            return;
        }

        cx.activate(true);
    });

    exit_code.get()
}
