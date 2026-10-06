use crate::term::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::Stylize;
use crossterm::terminal;

pub struct MenuItem {
    pub label: String,
    pub desc: String,
}

pub enum MenuAction {
    Selected(usize),
    Quit,
}

pub fn show_menu(title: &str, items: &[MenuItem]) -> MenuAction {
    if items.is_empty() {
        return MenuAction::Quit;
    }

    let mut selected = 0;
    terminal::enable_raw_mode().ok();

    loop {
        print!("\r\x1b[2J\x1b[H");
        println!("  {}", title.with(Theme::ACCENT).bold());
        println!();

        for (i, item) in items.iter().enumerate() {
            if i == selected {
                print!(
                    "  {} {}",
                    "▸".with(Theme::GREEN).bold(),
                    item.label.clone().on(Theme::BG_HI).with(Theme::GREEN).bold()
                );
            } else {
                print!("    {}", item.label.clone().with(Theme::MUTED));
            }

            if !item.desc.is_empty() && i == selected {
                println!("\n      {}", item.desc.clone().with(Theme::BRIGHT));
            } else {
                println!();
            }
        }

        println!(
            "\n  {} {} {} {} {} {}",
            "↑/↓ or J/K".with(Theme::INFO),
            "navigate  •".with(Theme::MUTED),
            "Enter".with(Theme::INFO),
            "select  •".with(Theme::MUTED),
            "Esc / Q".with(Theme::INFO),
            "back".with(Theme::MUTED)
        );

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('w') | KeyCode::Char('K') | KeyCode::Char('W') => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') | KeyCode::Char('J') | KeyCode::Char('S') => {
                    if selected + 1 < items.len() {
                        selected += 1;
                    }
                }
                KeyCode::Home => {
                    selected = 0;
                }
                KeyCode::End => {
                    selected = items.len().saturating_sub(1);
                }
                KeyCode::Enter => {
                    terminal::disable_raw_mode().ok();
                    print!("\r\x1b[2J\x1b[H");
                    return MenuAction::Selected(selected);
                }
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                    terminal::disable_raw_mode().ok();
                    print!("\r\x1b[2J\x1b[H");
                    return MenuAction::Quit;
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < items.len() {
                        terminal::disable_raw_mode().ok();
                        print!("\r\x1b[2J\x1b[H");
                        return MenuAction::Selected(idx);
                    }
                }
                _ => {}
            },
            Err(_) => break,
            _ => {}
        }
    }

    terminal::disable_raw_mode().ok();
    print!("\r\x1b[2J\x1b[H");
    MenuAction::Quit
}
