use anyhow::{Context, Result};
use beatbox::{mcp, tools, Engine};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "beatbox",
    version,
    about = "AI-native beat maker: synths, drums, samples, FX and a mix analyzer, all as MCP tools"
)]
struct Cli {
    /// Working directory for renders and downloaded samples
    #[arg(long, global = true)]
    workdir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the desktop studio (GUI). AIs can drive it live via `beatbox mcp --connect`.
    #[cfg(feature = "gui")]
    Studio {
        /// Project file to open
        project: Option<PathBuf>,
        /// Control server address for live AI control
        #[arg(long, default_value = beatbox::server::DEFAULT_ADDR)]
        listen: String,
        /// Save a PNG screenshot of the studio after N frames and exit (for docs/CI)
        #[arg(long, hide = true)]
        screenshot: Option<PathBuf>,
    },
    /// Run the MCP server on stdio (add this to Claude Desktop, Cursor, etc.)
    Mcp {
        /// Forward tool calls to a running studio, e.g. http://127.0.0.1:7878
        #[arg(long)]
        connect: Option<String>,
        /// Project file to open at start (local mode)
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Call any tool: beatbox call generate_beat '{"style":"trap"}' --project song.json
    Call {
        tool: String,
        /// JSON arguments (default {})
        args: Option<String>,
        /// Project to load first; saved back after a successful call
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// List every tool (what an AI sees)
    Tools {
        /// Full JSON schemas
        #[arg(long)]
        json: bool,
    },
    /// Make a complete beat in one shot and render it
    Beat {
        /// house, techno, trap, drill, boom_bap, lofi, dnb, reggaeton, afrobeats, phonk, garage
        style: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        seed: Option<u64>,
        /// Also save the project JSON here
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Render a project to WAV
    Render {
        project: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        stems: Option<PathBuf>,
    },
    /// Analyze a project's mix and print suggestions
    Analyze { project: PathBuf },
}

fn engine(workdir: &Option<PathBuf>) -> Engine {
    Engine::new(
        workdir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into())),
    )
}

fn load(e: &mut Engine, p: &PathBuf) -> Result<()> {
    e.call("load_project", &json!({"path": p}))?;
    Ok(())
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        #[cfg(feature = "gui")]
        Cmd::Studio {
            project,
            listen,
            screenshot,
        } => {
            let mut e = engine(&cli.workdir);
            if let Some(p) = &project {
                load(&mut e, p)?;
            } else {
                e.call(
                    "generate_beat",
                    &json!({"style": "trap", "key": "A", "seed": 7, "name": "first beat"}),
                )?;
            }
            beatbox::gui::run(e, &listen, project, screenshot)?;
        }
        Cmd::Mcp { connect, project } => {
            let backend = match connect {
                Some(url) => mcp::Backend::Remote(url),
                None => {
                    let mut e = engine(&cli.workdir);
                    if let Some(p) = &project {
                        load(&mut e, p)?;
                    }
                    mcp::Backend::Local(Box::new(e))
                }
            };
            mcp::serve(backend)?;
        }
        Cmd::Call {
            tool,
            args,
            project,
        } => {
            let mut e = engine(&cli.workdir);
            if let Some(p) = &project {
                if p.exists() {
                    load(&mut e, p)?;
                }
            }
            let a: Value = serde_json::from_str(args.as_deref().unwrap_or("{}"))
                .context("args must be JSON")?;
            let r = e.call(&tool, &a)?;
            print(&r);
            if let Some(p) = &project {
                if tools::find(&tool).map(|t| t.mutates).unwrap_or(false) {
                    e.call("save_project", &json!({"path": p}))?;
                    eprintln!("saved {}", p.display());
                }
            }
        }
        Cmd::Tools { json: full } => {
            if full {
                print(&tools::tools_json());
            } else {
                for t in tools::registry() {
                    println!("{:<20} {}", t.name, t.description);
                }
                println!("\n{} tools", tools::registry().len());
            }
        }
        Cmd::Beat {
            style,
            out,
            key,
            seed,
            save,
        } => {
            let mut e = engine(&cli.workdir);
            let mut a = json!({"style": style});
            if let Some(k) = key {
                a["key"] = json!(k);
            }
            if let Some(s) = seed {
                a["seed"] = json!(s);
            }
            e.call("generate_beat", &a)?;
            let mut r = json!({});
            if let Some(o) = out {
                r["path"] = json!(o);
            }
            let res = e.call("render", &r)?;
            let rep = e.call("analyze_mix", &json!({}))?;
            if let Some(s) = save {
                e.call("save_project", &json!({"path": s}))?;
            }
            print(
                &json!({"render": res, "score": rep["score"], "suggestions": rep["suggestions"]}),
            );
        }
        Cmd::Render {
            project,
            out,
            stems,
        } => {
            let mut e = engine(&cli.workdir);
            load(&mut e, &project)?;
            let mut a = json!({});
            if let Some(o) = out {
                a["path"] = json!(o);
            }
            if let Some(s) = stems {
                a["stems_dir"] = json!(s);
            }
            print(&e.call("render", &a)?);
        }
        Cmd::Analyze { project } => {
            let mut e = engine(&cli.workdir);
            load(&mut e, &project)?;
            print(&e.call("analyze_mix", &json!({}))?);
        }
    }
    Ok(())
}
