mod desktop;
mod posters;

use std::{
    cell::{Cell, RefCell},
    process::ExitCode,
    rc::Rc,
};

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBounds, WindowOptions, actions, px, size,
};
use kadr_ui::{KadrView, UiStatus};

actions!(kadr, [Quit]);

fn request_quit(cx: &App) {
    // Run outside platform callbacks: startup can lose an early quit, and X11 holds a borrow.
    cx.spawn(async |cx| cx.update(|cx| cx.quit()))
        .detach_and_log_err(cx);
}

fn main() -> ExitCode {
    // An unattended platform smoke still creates the real window/renderer.
    // It never opens a source or sends a catalogue query.
    let smoke_ui = std::env::args_os().any(|argument| argument == "--smoke-ui");
    let exit_code = Rc::new(Cell::new(ExitCode::SUCCESS));
    let startup_exit_code = Rc::clone(&exit_code);
    let desktop = Rc::new(RefCell::new(None::<Rc<RefCell<desktop::Desktop>>>));
    let running_desktop = desktop.clone();

    let poster_policy = posters::PosterPolicy::default();
    let application = Application::new().with_assets(kadr_ui::IconAssets);
    let application = match posters::Posters::new(poster_policy.clone()) {
        Ok(client) => application.with_http_client(std::sync::Arc::new(client)),
        Err(_) => application,
    };
    application.run(move |cx: &mut App| {
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

        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(760.0), px(560.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("kadr".into()),
                ..Default::default()
            }),
            app_id: Some("dev.kadr.Kadr".into()),
            ..Default::default()
        };

        if let Err(error) = cx.open_window(options, |window, cx| {
            window.set_window_title("kadr");
            let view = cx.new(|_| KadrView::new());
            match desktop::attach(&view, poster_policy.clone(), smoke_ui, cx) {
                Ok(desktop) => *running_desktop.borrow_mut() = Some(desktop),
                Err(_) => view.update(cx, |view, cx| {
                    view.set_status(
                        UiStatus::error("Не удалось запустить фоновый поток приложения."),
                        cx,
                    );
                }),
            }
            if smoke_ui {
                window.on_next_frame(|_, _| eprintln!("kadr-ui-smoke: frame"));
                let timer = cx
                    .background_executor()
                    .timer(std::time::Duration::from_secs(5));
                cx.spawn(async move |cx| {
                    timer.await;
                    let _ = cx.update(|cx| request_quit(cx));
                })
                .detach();
            }
            view
        }) {
            eprintln!("Не удалось открыть окно kadr: {error:#}");
            startup_exit_code.set(ExitCode::FAILURE);
            request_quit(cx);
            return;
        }

        cx.activate(true);
    });

    // GPUI allows only 100ms for quit observers; reap players and flush SQLite
    // after the event loop, without blocking render or abandoning owned children.
    if let Some(desktop) = desktop.borrow_mut().take()
        && !desktop.borrow_mut().join()
    {
        eprintln!("Фоновый поток kadr завершился с ошибкой.");
        exit_code.set(ExitCode::FAILURE);
    }
    exit_code.get()
}
