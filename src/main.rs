mod diff;
mod git;
mod review;
mod text_area;
mod theme;
mod view;

use diff::Diff;
use git::Repo;
use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBounds, WindowOptions, actions, px, size,
};
use review::Review;
use text_area::TextArea;
use view::{CopyComments, DiffView};

actions!(diffront, [Quit]);

const USAGE: &str = "\
Review a git diff, leave inline comments, and copy them for an agent.

Usage: diffront [git diff args...]

With no arguments, shows all uncommitted work: `git diff HEAD` plus untracked files.
Otherwise arguments go to `git diff`, e.g. `diffront main...HEAD` or `diffront --cached`.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return;
    }
    let (repo_name, review) = match load_review(&args) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("diffront: {err}");
            std::process::exit(1);
        }
    };
    if review.diff.files.is_empty() {
        println!("No changes.");
        return;
    }

    Application::new().run(move |cx: &mut App| {
        TextArea::bind_keys(cx);
        DiffView::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.set_menus(vec![Menu {
            name: "diffront".into(),
            items: vec![
                MenuItem::action("Copy Comments", CopyComments),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        }]);

        let bounds = Bounds::centered(None, size(px(1280.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("{repo_name} — diffront").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DiffView::new(review, window, cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}

fn load_review(args: &[String]) -> Result<(String, Review), String> {
    let repo = Repo::discover()?;
    let source = if args.is_empty() {
        "git diff HEAD + untracked files".to_string()
    } else {
        format!("git diff {}", args.join(" "))
    };
    let diff = Diff::parse(&repo.patch(args)?);
    Ok((repo.name(), Review::new(source, diff)))
}
