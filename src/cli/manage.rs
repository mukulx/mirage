use crate::auth::MinecraftAccount;
use crate::instance::Instance;
use crate::launcher::Launcher;
use crate::modpack;
use crate::term::Term;
use anyhow::Result;

pub async fn pick_instance() -> Result<String> {
    let instances = Instance::list_all();
    if instances.is_empty() {
        anyhow::bail!("No instances yet. Create one with 'mirage instance create <name>'");
    }
    if instances.len() == 1 {
        return Ok(instances[0].name.clone());
    }

    let items: Vec<crate::menu::MenuItem> = instances
        .iter()
        .map(|inst| {
            let mods_count = inst.installed_mods().len();
            crate::menu::MenuItem {
                label: inst.name.clone(),
                desc: format!(
                    "{} {} | {} mods",
                    inst.loader.to_uppercase(),
                    inst.mc_version,
                    mods_count
                ),
            }
        })
        .collect();

    match crate::menu::show_menu("Select Instance", &items) {
        crate::menu::MenuAction::Selected(i) => Ok(instances[i].name.clone()),
        crate::menu::MenuAction::Quit => anyhow::bail!("Instance selection cancelled"),
    }
}

pub async fn manage_instance(name: &str, account: &MinecraftAccount) -> Result<()> {
    let inst = Instance::load(name)?;

    loop {
        let mods = inst.installed_mods();
        let total_mods = mods.len();
        let enabled_mods = mods.iter().filter(|m| m.enabled).count();

        let items = vec![
            crate::menu::MenuItem {
                label: "List Mods".into(),
                desc: format!("{} mods installed ({} active)", total_mods, enabled_mods),
            },
            crate::menu::MenuItem {
                label: "Search & Install Mod".into(),
                desc: "Search Modrinth and download into this instance".into(),
            },
            crate::menu::MenuItem {
                label: "Toggle Mod (Enable/Disable)".into(),
                desc: "Enable or disable an installed mod".into(),
            },
            crate::menu::MenuItem {
                label: "Delete Mod".into(),
                desc: "Permanently remove a mod".into(),
            },
            crate::menu::MenuItem {
                label: "Launch Game".into(),
                desc: format!("Start Minecraft with instance '{}'", inst.name),
            },
            crate::menu::MenuItem {
                label: "Back".into(),
                desc: "Return to previous menu".into(),
            },
        ];

        match crate::menu::show_menu(&format!("Manage Instance: '{}'", inst.name), &items) {
            crate::menu::MenuAction::Selected(0) => {
                let current_mods = inst.installed_mods();
                Term::header(&format!("Installed Mods ({})", current_mods.len()));
                if current_mods.is_empty() {
                    println!("    No mods installed.");
                } else {
                    for m in &current_mods {
                        let status = if m.enabled { "✔ [ON]" } else { "✗ [OFF]" };
                        println!("    {} {} ({:.1} KB)", status, m.display_name, m.size_bytes as f64 / 1024.0);
                    }
                }
                println!("\n  Press Enter to continue...");
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
            }
            crate::menu::MenuAction::Selected(1) => {
                print!("  Enter mod search query: ");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                let mut query = String::new();
                let _ = std::io::stdin().read_line(&mut query);
                let query = query.trim();

                if !query.is_empty() {
                    let hits = modpack::search_mods(query, Some(&inst.loader), Some(&inst.mc_version), 15).await?;
                    if hits.is_empty() {
                        Term::warn("No mods found matching query.");
                        continue;
                    }

                    let hit_items: Vec<crate::menu::MenuItem> = hits
                        .iter()
                        .map(|h| {
                            let author = h.author.as_deref().unwrap_or("unknown");
                            let d = h.downloads.unwrap_or(0);
                            crate::menu::MenuItem {
                                label: format!("{} (by {})", h.title, author),
                                desc: format!("{} | ⬇ {}", h.description, d),
                            }
                        })
                        .collect();

                    match crate::menu::show_menu("Select Mod to Install", &hit_items) {
                        crate::menu::MenuAction::Selected(idx) => {
                            let hit = &hits[idx];
                            Term::info(&format!("Installing {}...", hit.title));
                            match modpack::install_mod_to_instance(&hit.slug, &inst).await {
                                Ok(res) => {
                                    Term::success(&format!("Successfully installed {} ({})!", hit.title, res.version_number));
                                }
                                Err(e) => {
                                    Term::err(&format!("Installation failed: {}", e));
                                }
                            }
                            println!("\n  Press Enter to continue...");
                            let mut buf = String::new();
                            let _ = std::io::stdin().read_line(&mut buf);
                        }
                        crate::menu::MenuAction::Quit => {}
                    }
                }
            }
            crate::menu::MenuAction::Selected(2) => {
                let current_mods = inst.installed_mods();
                if current_mods.is_empty() {
                    Term::warn("No mods installed.");
                    continue;
                }

                let toggle_items: Vec<crate::menu::MenuItem> = current_mods
                    .iter()
                    .map(|m| {
                        let st = if m.enabled { "Enabled" } else { "Disabled" };
                        crate::menu::MenuItem {
                            label: format!("[{}] {}", st, m.display_name),
                            desc: format!("File: {}", m.filename),
                        }
                    })
                    .collect();

                match crate::menu::show_menu("Select Mod to Toggle", &toggle_items) {
                    crate::menu::MenuAction::Selected(idx) => {
                        let m = &current_mods[idx];
                        if let Ok(new_state) = inst.toggle_mod(&m.filename) {
                            let st = if new_state { "Enabled" } else { "Disabled" };
                            Term::success(&format!("{} mod {}", st, m.display_name));
                        }
                    }
                    crate::menu::MenuAction::Quit => {}
                }
            }
            crate::menu::MenuAction::Selected(3) => {
                let current_mods = inst.installed_mods();
                if current_mods.is_empty() {
                    Term::warn("No mods installed.");
                    continue;
                }

                let del_items: Vec<crate::menu::MenuItem> = current_mods
                    .iter()
                    .map(|m| crate::menu::MenuItem {
                        label: m.display_name.clone(),
                        desc: format!("File: {}", m.filename),
                    })
                    .collect();

                match crate::menu::show_menu("Select Mod to Delete", &del_items) {
                    crate::menu::MenuAction::Selected(idx) => {
                        let m = &current_mods[idx];
                        let _ = inst.remove_mod(&m.filename);
                        Term::success(&format!("Deleted mod {}", m.display_name));
                    }
                    crate::menu::MenuAction::Quit => {}
                }
            }
            crate::menu::MenuAction::Selected(4) => {
                let ver = inst.effective_version_id();
                let mut launcher = Launcher::new(None, Some(inst.name.clone()));
                launcher.launch_auto(account, &ver, &[], None).await?;
                break;
            }
            _ => break,
        }
    }

    Ok(())
}
