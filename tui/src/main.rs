//! ache-tui binary: parse args, build the real facade (an MCP
//! stdio client spawning the ache server), run full-screen where
//! a TTY exists or line mode otherwise.
//!
//! Args: `--server <path>` (default `ache` on PATH), `--line`
//! (force line mode). The TUI reads no credentials and passes no
//! config to the server; the server resolves its own.

#![forbid(unsafe_code)]

use std::io::{BufRead, IsTerminal, Write};

use ache_tui::app::App;
use ache_tui::facade::StdioFacade;
use ache_tui::panels;
use ache_tui::screens::execute_line;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut server_bin = "ache".to_string();
    let mut force_line = false;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--server" => {
                idx += 1;
                if let Some(value) = args.get(idx) {
                    server_bin = value.clone();
                }
            }
            "--line" => force_line = true,
            other => {
                eprintln!(
                    "ache-tui: unknown argument {other:?} (usage: [--server <path>] [--line])"
                );
                std::process::exit(2);
            }
        }
        idx += 1;
    }
    let mut facade = match StdioFacade::spawn(&server_bin) {
        Ok(f) => f,
        Err(error) => {
            eprintln!("ache-tui: cannot spawn ache server {server_bin:?}: {error}");
            std::process::exit(2);
        }
    };
    let mut app = App::new();
    app.refresh_status(&mut facade);
    let interactive =
        !force_line && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let code = if interactive {
        run_fullscreen(&mut app, &mut facade)
    } else {
        run_line_mode(&mut app, &mut facade);
        0
    };
    std::process::exit(code);
}

fn run_line_mode(app: &mut App, facade: &mut StdioFacade) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(text) = line else { break };
        let answer = execute_line(app, facade, &text);
        let _ = writeln!(stdout, "{answer}");
        let _ = stdout.flush();
        if app.should_quit {
            break;
        }
    }
}

fn run_fullscreen(app: &mut App, facade: &mut StdioFacade) -> i32 {
    if crossterm::terminal::enable_raw_mode().is_err() {
        return 2;
    }
    let mut stdout = std::io::stdout();
    if crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen).is_err() {
        let _ = crossterm::terminal::disable_raw_mode();
        return 2;
    }
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = crossterm::terminal::disable_raw_mode();
            return 2;
        }
    };
    let mut result = 0;
    loop {
        if terminal
            .draw(|frame| {
                let area = frame.area();
                panels::render(app, frame, area);
            })
            .is_err()
        {
            result = 2;
            break;
        }
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(key)) => {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    match key.code {
                        crossterm::event::KeyCode::Char(c) => app.on_key(facade, c),
                        crossterm::event::KeyCode::Tab => app.on_key(facade, '\t'),
                        crossterm::event::KeyCode::Esc => app.should_quit = true,
                        _ => {}
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                result = 2;
                break;
            }
        }
        if app.should_quit {
            break;
        }
    }
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    result
}
