use crate::auth::{self, MinecraftAccount};
use crate::config;
use crate::instance::Instance;
use crate::launcher::Launcher;
use crate::modpack;
use crate::term::Term;
use clap::{Parser, Subcommand};

mod manage;
mod shell;

pub use manage::pick_instance;

#[derive(Parser)]
#[command(
    name = "mirage",
    about = "Mirage Launcher: a fast terminal launcher for Minecraft",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(long, global = true, help = "Custom Java executable path")]
    pub java: Option<String>,

    #[arg(short = 'i', long, global = true, help = "Target instance name")]
    pub instance: Option<String>,

    #[arg(long, global = true, help = "Login with Microsoft OAuth")]
    pub login: bool,

    #[arg(long, global = true, help = "Check account login status")]
    pub status: bool,

    #[arg(long, global = true, help = "Logout active account")]
    pub logout: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(about = "Launch Minecraft")]
    Launch {
        #[arg(help = "Version to launch (e.g. 1.21.1, fabric-1.21.1, or defaults to instance version)")]
        version: Option<String>,
        #[arg(long, help = "Open the game in a new terminal window (overrides config)")]
        new_terminal: bool,
        #[arg(long, help = "Run the game in this terminal (overrides config)")]
        same_terminal: bool,
    },

    #[command(about = "Install Fabric loader for a Minecraft version")]
    Fabric {
        #[arg(help = "Minecraft version (e.g. 1.21.1)")]
        version: String,
    },

    #[command(about = "Manage Minecraft instances")]
    Instance {
        #[command(subcommand)]
        action: InstanceAction,
    },

    #[command(about = "Manage mods for an instance")]
    Mod {
        #[command(subcommand)]
        action: ModAction,
    },

    #[command(about = "Search and install Modrinth modpacks")]
    Modpack {
        #[command(subcommand)]
        action: ModpackAction,
    },

    #[command(about = "Account management (Microsoft login)")]
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },

    #[command(about = "Launcher configuration (RAM, Java, arguments)")]
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },

    #[command(about = "Interactive menu instance manager")]
    Manage {
        #[arg(help = "Instance name to manage")]
        name: Option<String>,
    },

    #[command(about = "List and search available Minecraft versions from Mojang manifest")]
    Versions {
        #[arg(short = 's', long, help = "Include snapshots and experimental versions")]
        snapshots: bool,
        #[arg(short = 'q', long, help = "Filter version search query (e.g. 1.21)")]
        query: Option<String>,
    },

    #[command(about = "Launch full Graphical Terminal UI (TUI)")]
    Tui,

    #[command(about = "Interactive CLI Shell REPL")]
    Shell,
}

#[derive(Subcommand)]
pub enum InstanceAction {
    #[command(about = "Create a new instance")]
    Create {
        name: String,
        #[arg(short = 'v', long, default_value = "1.21.1")]
        version: String,
        #[arg(short = 'l', long, default_value = "fabric")]
        loader: String,
        #[arg(long)]
        ram: Option<String>,
    },
    #[command(about = "List all instances")]
    List,
    #[command(about = "Delete an instance")]
    Delete { name: String },
    #[command(about = "Duplicate an existing instance")]
    Duplicate { source: String, new_name: String },
    #[command(about = "Show instance details")]
    Info { name: String },
}

#[derive(Subcommand)]
pub enum ModAction {
    #[command(about = "Search and install a mod by slug, name, or project ID")]
    Add {
        slug_or_id: String,
        #[arg(short = 'i', long)]
        instance: Option<String>,
    },
    #[command(about = "List installed mods in an instance")]
    List {
        #[arg(short = 'i', long)]
        instance: Option<String>,
    },
    #[command(about = "Toggle a mod enabled/disabled")]
    Toggle {
        filename: String,
        #[arg(short = 'i', long)]
        instance: Option<String>,
    },
    #[command(about = "Remove a mod file from an instance")]
    Remove {
        filename: String,
        #[arg(short = 'i', long)]
        instance: Option<String>,
    },
    #[command(about = "Interactive mod search on Modrinth")]
    Search {
        query: String,
        #[arg(short = 'i', long)]
        instance: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ModpackAction {
    #[command(about = "Search modpacks on Modrinth")]
    Search { query: String },
    #[command(about = "Download and install a Modrinth modpack (.mrpack)")]
    Install {
        slug_or_id: String,
        #[arg(short = 'n', long, help = "Custom instance name")]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AccountAction {
    #[command(about = "Login via Microsoft Device OAuth")]
    Login,
    #[command(about = "View active account status (verifies session)")]
    Status,
    #[command(about = "Force-refresh Microsoft session now")]
    Refresh,
    #[command(about = "Logout and clear active account")]
    Logout,
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    #[command(about = "List all configuration values")]
    List,
    #[command(about = "Set default RAM allocation (e.g. 2G 6G or 4G)")]
    SetRam { min: String, max: Option<String> },
    #[command(about = "Set default Java executable path")]
    SetJava { path: String },
    #[command(about = "Add a JVM or Game argument")]
    Add { kind: String, value: String },
    #[command(about = "Remove a JVM or Game argument by index")]
    Rm { kind: String, index: usize },
    #[command(about = "Turn new-terminal game launches on/off (default on)")]
    Terminal { state: String },
    #[command(about = "Reset configuration to defaults")]
    Clear,
}

pub async fn get_account() -> anyhow::Result<MinecraftAccount> {
    match auth::load_account() {
        // Unexpired tokens are used as-is; expired ones are refreshed.
        Some(acc) => auth::account_for_launch(&acc).await,
        None => {
            Term::warn("No account found. A Microsoft account that owns Minecraft is required.");
            Term::header("Microsoft OAuth Login");
            auth::login_microsoft().await
        }
    }
}

async fn print_account_status() {
    let Some(acc) = auth::load_account() else {
        Term::warn("Not logged in. Use 'mirage account login'");
        return;
    };
    match auth::revalidate_account(&acc).await {
        Ok((fresh, note)) => {
            if let Some(note) = note {
                Term::info(&note);
            }
            Term::done(&format!(
                "Logged in as {} (UUID: {}, session: {})",
                fresh.username,
                fresh.uuid,
                fresh.expiry_label()
            ));
        }
        Err(e) => Term::err(&format!("{:#}", e)),
    }
}

pub async fn run() -> anyhow::Result<()> {
    let raw: Vec<String> = std::env::args().collect();
    let args: Vec<String> = if raw.len() > 1
        && !raw[1].starts_with('-')
        && ![
            "launch", "fabric", "instance", "mod", "modpack", "account", "config",
            "manage", "versions", "tui", "shell", "help", "--help", "-h", "-v", "--version",
        ]
        .contains(&raw[1].as_str())
    {
        // Shorthand e.g. `mirage 1.21.1` -> `mirage launch 1.21.1`
        let mut v = vec![raw[0].clone(), "launch".into()];
        v.extend(raw[1..].iter().cloned());
        v
    } else {
        raw
    };

    let cli = Cli::parse_from(args);

    if cli.status {
        print_account_status().await;
        return Ok(());
    }

    if cli.login {
        Term::header("Microsoft Login");
        let acc = auth::login_microsoft().await?;
        Term::done(&format!("Logged in as {}", acc.username));
        return Ok(());
    }

    if cli.logout {
        auth::delete_account(None)?;
        Term::done("Logged out");
        return Ok(());
    }

    match cli.command {
        Some(Commands::Launch { version, new_terminal, same_terminal }) => {
            let account = get_account().await?;
            let mut launcher = Launcher::new(cli.java, cli.instance);

            let ver = if let Some(v) = version {
                v
            } else {
                launcher.instance.effective_version_id()
            };

            let force = if same_terminal {
                Some(false)
            } else if new_terminal {
                Some(true)
            } else {
                None
            };
            launcher.launch_auto(&account, &ver, &[], force).await?;
        }

        Some(Commands::Fabric { version }) => {
            Term::header(&format!("Fabric Installer — MC {}", version));
            let id = modpack::install_fabric(&version).await?;
            let launcher = Launcher::new(cli.java.clone(), cli.instance);
            launcher.prepare(&version).await?;
            Term::success(&format!("Fabric profile installed: {}", id));
            Term::cmd(&format!("mirage launch {}", id));
        }

        Some(Commands::Instance { action }) => match action {
            InstanceAction::Create {
                name,
                version,
                loader,
                ram,
            } => {
                let mut inst = Instance::create(&name, &version, &loader, None)?;
                if let Some(r) = ram {
                    inst.ram_max = Some(r);
                    let _ = inst.save();
                }
                if loader.eq_ignore_ascii_case("fabric") {
                    let _ = modpack::install_fabric(&version).await;
                }
                Term::success(&format!("Instance '{}' created with {} {}!", inst.name, inst.loader, inst.mc_version));
            }

            InstanceAction::List => {
                let instances = Instance::list_all();
                Term::header(&format!("Minecraft Instances ({})", instances.len()));
                for inst in instances {
                    let mods_count = inst.installed_mods().len();
                    println!(
                        "    • {:16} [{:^7}] {:^7} ({} mods)  -> {}",
                        inst.name,
                        inst.loader,
                        inst.mc_version,
                        mods_count,
                        inst.dir().display()
                    );
                }
                println!();
            }

            InstanceAction::Delete { name } => {
                let inst = Instance::load(&name)?;
                inst.delete()?;
                Term::success(&format!("Instance '{}' deleted.", name));
            }

            InstanceAction::Duplicate { source, new_name } => {
                let inst = Instance::load(&source)?;
                let cloned = inst.duplicate(&new_name)?;
                Term::success(&format!("Duplicated '{}' to '{}'.", source, cloned.name));
            }

            InstanceAction::Info { name } => {
                let inst = Instance::load(&name)?;
                let mods = inst.installed_mods();
                Term::header(&format!("Instance: {}", inst.name));
                Term::label("Minecraft Version", &inst.mc_version);
                Term::label("Mod Loader", &inst.loader);
                Term::label("RAM Allocation", &format!("{} - {}", inst.ram_min.as_deref().unwrap_or("2G"), inst.ram_max.as_deref().unwrap_or("4G")));
                Term::label("Mods Installed", &format!("{} total", mods.len()));
                Term::label("Directory", &inst.dir().display().to_string());
                Term::label("Created At", &inst.created_at);
            }
        },

        Some(Commands::Mod { action }) => match action {
            ModAction::Add { slug_or_id, instance } => {
                let inst_name = instance.unwrap_or_else(|| config::load().default_instance);
                let inst = Instance::load(&inst_name).unwrap_or_else(|_| Instance::new(&inst_name, "latest", "fabric"));
                let res = modpack::install_mod_to_instance(&slug_or_id, &inst).await?;
                Term::success(&format!("Installed {} ({}) into instance '{}'!", res.title, res.version_number, inst.name));
            }

            ModAction::List { instance } => {
                let inst_name = instance.unwrap_or_else(|| config::load().default_instance);
                let inst = Instance::load(&inst_name)?;
                let mods = inst.installed_mods();
                Term::header(&format!("Installed Mods in '{}' ({})", inst.name, mods.len()));
                if mods.is_empty() {
                    println!("    No mods installed.");
                } else {
                    for m in mods {
                        let status = if m.enabled { "✔ [ON]" } else { "✗ [OFF]" };
                        println!("    {} {:35} ({:.1} KB)", status, m.display_name, m.size_bytes as f64 / 1024.0);
                    }
                }
            }

            ModAction::Toggle { filename, instance } => {
                let inst_name = instance.unwrap_or_else(|| config::load().default_instance);
                let inst = Instance::load(&inst_name)?;
                let new_st = inst.toggle_mod(&filename)?;
                Term::success(&format!("Mod {} is now {}", filename, if new_st { "Enabled" } else { "Disabled" }));
            }

            ModAction::Remove { filename, instance } => {
                let inst_name = instance.unwrap_or_else(|| config::load().default_instance);
                let inst = Instance::load(&inst_name)?;
                inst.remove_mod(&filename)?;
                Term::success(&format!("Removed mod {}", filename));
            }

            ModAction::Search { query, instance } => {
                modpack::install_mod_interactive(&query, instance.as_deref()).await?;
            }
        },

        Some(Commands::Modpack { action }) => match action {
            ModpackAction::Search { query } => {
                modpack::install_modpack_interactive(&query).await?;
            }
            ModpackAction::Install { slug_or_id, name } => {
                let inst = modpack::install_modpack(&slug_or_id, name.as_deref()).await?;
                Term::success(&format!("Modpack ready in instance '{}'!", inst.name));
            }
        },

        Some(Commands::Account { action }) => match action {
            AccountAction::Login => {
                Term::header("Microsoft OAuth Login");
                let acc = auth::login_microsoft().await?;
                Term::success(&format!("Logged in as {}", acc.username));
            }
            AccountAction::Status => {
                print_account_status().await;
            }
            AccountAction::Refresh => {
                match auth::load_account() {
                    None => Term::warn("No active account to refresh."),
                    Some(acc) => match auth::refresh_microsoft_account(&acc).await {
                        Ok(fresh) => Term::done(&format!(
                            "Refreshed {} (session: {})",
                            fresh.username,
                            fresh.expiry_label()
                        )),
                        Err(e) => Term::err(&format!("Refresh failed: {:#}", e)),
                    },
                }
            }
            AccountAction::Logout => {
                auth::delete_account(None)?;
                Term::done("Logged out all accounts");
            }
        },

        Some(Commands::Config { action }) => match action {
            ConfigCommands::List => config::list(),
            ConfigCommands::SetRam { min, max } => {
                let max_val = max.unwrap_or_else(|| min.clone());
                config::set_ram(&min, &max_val)?;
                Term::done(&format!("RAM limits set to {} - {}", min, max_val));
            }
            ConfigCommands::SetJava { path } => {
                config::set_java(Some(path.clone()))?;
                Term::done(&format!("Default Java set to {}", path));
            }
            ConfigCommands::Add { kind, value } => {
                match kind.as_str() {
                    "jvm" => config::add_jvm_arg(&value)?,
                    "game" => config::add_game_arg(&value)?,
                    _ => anyhow::bail!("Unknown kind: '{}' (use jvm or game)", kind),
                }
                Term::done(&format!("Added {} argument: {}", kind, value));
            }
            ConfigCommands::Rm { kind, index } => {
                match kind.as_str() {
                    "jvm" => config::remove_jvm_arg(index)?,
                    "game" => config::remove_game_arg(index)?,
                    _ => anyhow::bail!("Unknown kind: '{}' (use jvm or game)", kind),
                }
                Term::done(&format!("Removed {} argument #{}", kind, index));
            }
            ConfigCommands::Terminal { state } => {
                let on = match state.to_lowercase().as_str() {
                    "on" | "true" | "1" | "yes" | "enable" | "enabled" => true,
                    "off" | "false" | "0" | "no" | "disable" | "disabled" => false,
                    _ => anyhow::bail!("Use `config terminal on|off` (got '{}')", state),
                };
                config::set_new_terminal(on)?;
                Term::done(&format!(
                    "New-terminal launches {}",
                    if on { "ON — games open in a new window" } else { "OFF — games run in this terminal" }
                ));
            }
            ConfigCommands::Clear => {
                config::save(&config::Config::default())?;
                Term::done("Config reset to defaults");
            }
        },

        Some(Commands::Manage { name }) => {
            let inst_name = match name {
                Some(n) => n,
                None => pick_instance().await?,
            };
            let account = get_account().await?;
            manage::manage_instance(&inst_name, &account).await?;
        }

        Some(Commands::Versions { snapshots, query }) => {
            Term::header("Minecraft Versions (Mojang Online Manifest)");
            let all = crate::launcher::fetch_minecraft_versions(snapshots).await?;
            let filter_q = query.as_deref().unwrap_or("").to_lowercase();
            let mut count = 0;
            for (ver, rel_type) in all {
                if filter_q.is_empty() || ver.to_lowercase().contains(&filter_q) {
                    println!("    • {:15} [{}]", ver, rel_type);
                    count += 1;
                    if count >= 30 && filter_q.is_empty() {
                        println!("    ... and more. Use --query <search> or --snapshots to filter.");
                        break;
                    }
                }
            }
            println!();
        }

        Some(Commands::Tui) => {
            crate::tui::run_tui().await?;
        }

        Some(Commands::Shell) => {
            let inst = cli.instance.unwrap_or_else(|| config::load().default_instance);
            shell::shell_loop(inst).await?;
        }

        None => {
            // Default to TUI if stdout is a terminal, or show banner & help
            if crossterm::tty::IsTty::is_tty(&std::io::stdout()) {
                crate::tui::run_tui().await?;
            } else {
                Term::banner();
                println!("  Run 'mirage tui' for Graphical UI or 'mirage --help' for CLI options.");
            }
        }
    }

    Ok(())
}
