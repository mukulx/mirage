use crate::instance::Instance;
use crate::launcher::{colorize_log, Launcher};
use crate::modpack;
use crate::term::Term;
use std::io::{BufRead, BufReader, Write};

use super::get_account;

pub async fn shell_loop(default_instance: String) -> anyhow::Result<()> {
    let mut current_instance = default_instance;
    let account = get_account().await?;

    Term::banner();
    println!("  Interactive Shell Mode. Type 'help' for commands, 'exit' to quit.\n");

    loop {
        print!("  mirage [{}] > ", current_instance);
        let _ = std::io::stdout().flush();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            break;
        }

        let cmd = input.trim().to_string();
        if cmd.is_empty() {
            continue;
        }

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        match parts[0] {
            "exit" | "quit" | "q" => break,
            "help" | "?" => {
                println!("\n  Shell Commands:");
                println!("    launch [version]          Launch game (e.g. launch 1.21.1 or launch)");
                println!("    use <instance>            Switch active instance");
                println!("    instance list             List all instances");
                println!("    instance create <name> [ver] [loader] Create instance");
                println!("    instance delete <name>    Delete an instance");
                println!("    mod search <query>        Search Modrinth mods");
                println!("    mod add <slug_or_id>      Install mod to current instance");
                println!("    mod list                  List mods in current instance");
                println!("    mod toggle <filename>     Enable/Disable mod");
                println!("    mod remove <filename>     Remove mod file");
                println!("    modpack search <query>    Search modpacks");
                println!("    modpack install <slug>    Install modpack from Modrinth");
                println!("    config ...                Manage settings");
                println!("    tui                       Launch Graphical TUI");
                println!("    clear                     Clear terminal");
                println!("    exit                      Quit shell\n");
            }
            "clear" | "cls" => {
                print!("\x1b[2J\x1b[H");
                let _ = std::io::stdout().flush();
            }
            "use" => {
                if let Some(target) = parts.get(1) {
                    if Instance::load(target).is_ok() {
                        current_instance = target.to_string();
                        Term::done(&format!("Active instance is now '{}'", current_instance));
                    } else {
                        Term::warn(&format!("Instance '{}' not found", target));
                    }
                } else {
                    println!("  Usage: use <instance_name>");
                }
            }
            "launch" => {
                let inst = Instance::load(&current_instance).unwrap_or_else(|_| {
                    Instance::new(&current_instance, "latest", "vanilla")
                });
                let ver = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| inst.effective_version_id());

                let mut launcher = Launcher::new(None, Some(current_instance.clone()));
                match launcher.spawn_minecraft(&account, &ver, &[]).await {
                    Ok(mut child) => {
                        let pid = child.id();
                        let stdout = child.stdout.take().unwrap();
                        let stderr = child.stderr.take().unwrap();
                        std::thread::spawn(move || {
                            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                                println!("{}", colorize_log(&line));
                            }
                        });
                        std::thread::spawn(move || {
                            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                                eprintln!("{}", colorize_log(&line));
                            }
                        });
                        Term::success(&format!("Minecraft is running in background (PID {})", pid));
                    }
                    Err(e) => Term::err(&format!("Launch failed: {}", e)),
                }
            }
            "instance" => {
                let sub = parts.get(1).copied().unwrap_or("list");
                match sub {
                    "list" | "ls" => {
                        let instances = Instance::list_all();
                        Term::header(&format!("Instances ({})", instances.len()));
                        for i in instances {
                            let is_active = if i.name == current_instance { "*" } else { " " };
                            println!("  {} {:15} [{}] {} ({} mods)", is_active, i.name, i.loader, i.mc_version, i.installed_mods().len());
                        }
                    }
                    "create" => {
                        if let Some(name) = parts.get(2) {
                            let mc = parts.get(3).copied().unwrap_or("1.21.1");
                            let loader = parts.get(4).copied().unwrap_or("fabric");
                            match Instance::create(name, mc, loader, None) {
                                Ok(inst) => {
                                    if loader == "fabric" {
                                        let _ = modpack::install_fabric(mc).await;
                                    }
                                    Term::success(&format!("Created instance '{}' ({} {})", inst.name, inst.loader, inst.mc_version));
                                }
                                Err(e) => Term::err(&format!("Failed to create instance: {}", e)),
                            }
                        } else {
                            println!("  Usage: instance create <name> [mc_version] [loader]");
                        }
                    }
                    "delete" | "rm" => {
                        if let Some(name) = parts.get(2) {
                            if let Ok(inst) = Instance::load(name) {
                                if inst.delete().is_ok() {
                                    Term::success(&format!("Deleted instance '{}'", name));
                                    if current_instance == *name {
                                        current_instance = Instance::list_all().first().map_or_else(|| "default".to_string(), |i| i.name.clone());
                                    }
                                }
                            } else {
                                Term::warn(&format!("Instance '{}' not found", name));
                            }
                        } else {
                            println!("  Usage: instance delete <name>");
                        }
                    }
                    _ => println!("  Unknown instance command. Use: instance list, instance create, instance delete"),
                }
            }
            "mod" => {
                let sub = parts.get(1).copied().unwrap_or("list");
                let inst = Instance::load(&current_instance).unwrap_or_else(|_| {
                    Instance::new(&current_instance, "latest", "vanilla")
                });

                match sub {
                    "list" | "ls" => {
                        let mods = inst.installed_mods();
                        Term::header(&format!("Mods in '{}' ({})", inst.name, mods.len()));
                        for m in mods {
                            let status = if m.enabled { "✔ [ON]" } else { "✗ [OFF]" };
                            println!("    {} {} ({:.1} KB)", status, m.display_name, m.size_bytes as f64 / 1024.0);
                        }
                    }
                    "search" => {
                        let q = if parts.len() > 2 { parts[2..].join(" ") } else { String::new() };
                        if !q.is_empty() {
                            let _ = modpack::install_mod_interactive(&q, Some(&current_instance)).await;
                        } else {
                            println!("  Usage: mod search <query>");
                        }
                    }
                    "add" | "install" => {
                        if let Some(slug) = parts.get(2) {
                            match modpack::install_mod_to_instance(slug, &inst).await {
                                Ok(res) => Term::success(&format!("Installed {} ({})", res.title, res.version_number)),
                                Err(e) => Term::err(&format!("Mod install failed: {}", e)),
                            }
                        } else {
                            println!("  Usage: mod add <slug_or_project_id>");
                        }
                    }
                    "toggle" => {
                        if let Some(fn_name) = parts.get(2) {
                            match inst.toggle_mod(fn_name) {
                                Ok(new_st) => Term::success(&format!("Mod {} is now {}", fn_name, if new_st { "Enabled" } else { "Disabled" })),
                                Err(e) => Term::err(&format!("Toggle failed: {}", e)),
                            }
                        } else {
                            println!("  Usage: mod toggle <filename>");
                        }
                    }
                    "remove" | "rm" => {
                        if let Some(fn_name) = parts.get(2) {
                            match inst.remove_mod(fn_name) {
                                Ok(_) => Term::success(&format!("Removed mod {}", fn_name)),
                                Err(e) => Term::err(&format!("Remove failed: {}", e)),
                            }
                        } else {
                            println!("  Usage: mod remove <filename>");
                        }
                    }
                    _ => println!("  Unknown mod command. Use: mod list, mod search, mod add, mod toggle, mod remove"),
                }
            }
            "modpack" => {
                let sub = parts.get(1).copied().unwrap_or("search");
                match sub {
                    "search" => {
                        let q = if parts.len() > 2 { parts[2..].join(" ") } else { "optimization".into() };
                        let _ = modpack::install_modpack_interactive(&q).await;
                    }
                    "install" => {
                        if let Some(slug) = parts.get(2) {
                            let custom_name = parts.get(3).copied();
                            match modpack::install_modpack(slug, custom_name).await {
                                Ok(new_inst) => {
                                    Term::success(&format!("Modpack installed as instance '{}'", new_inst.name));
                                    current_instance = new_inst.name;
                                }
                                Err(e) => Term::err(&format!("Modpack install failed: {}", e)),
                            }
                        } else {
                            println!("  Usage: modpack install <slug_or_id> [instance_name]");
                        }
                    }
                    _ => println!("  Unknown modpack command. Use: modpack search <query>, modpack install <slug>"),
                }
            }
            "tui" => {
                let _ = crate::tui::run_tui().await;
            }
            _ => {
                Term::warn(&format!("Unknown command: '{}'. Type 'help' for available commands.", parts[0]));
            }
        }
    }

    Ok(())
}
