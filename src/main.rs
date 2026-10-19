use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use rustypdf::config::Config;
use rustypdf::fonts::FontStore;
use rustypdf::render::{self, RenderInput};
use rustypdf::world::{ProjectFiles, Shared};

#[derive(Parser)]
#[command(version, about = "Render Typst templates into PDF/A-3b from JSON")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP service (the default).
    Serve,
    /// Render one template without HTTP (development aid).
    Render {
        /// Template folder containing `main.typ`. Its parent folder is the
        /// Typst project root, so `/_shared/...` imports work.
        template_dir: PathBuf,
        /// JSON file served to the template as `/data.json`.
        data: PathBuf,
        /// Output PDF path.
        #[arg(short, long, default_value = "out.pdf")]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();
    let config = Config::from_env()?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(rustypdf::api::serve(config))
        }
        Command::Render {
            template_dir,
            data,
            output,
        } => render_cli(&config, &template_dir, &data, &output),
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("LOG_FORMAT").is_ok_and(|v| v == "json");
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

fn render_cli(
    config: &Config,
    template_dir: &std::path::Path,
    data: &std::path::Path,
    output: &std::path::Path,
) -> Result<()> {
    let template_dir = template_dir
        .canonicalize()
        .with_context(|| format!("cannot open {}", template_dir.display()))?;
    let (Some(root), Some(name)) = (template_dir.parent(), template_dir.file_name()) else {
        bail!(
            "{} has no parent folder to use as the project root",
            template_dir.display()
        );
    };
    let name = name.to_string_lossy();

    let fonts = FontStore::load(&config.fonts_dir, &config.required_fonts)?;
    let shared = Shared::new(Arc::new(fonts));
    let project =
        ProjectFiles::load(root).with_context(|| format!("cannot read {}", root.display()))?;
    let mut json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data).with_context(|| format!("cannot read {}", data.display()))?,
    )
    .with_context(|| format!("{} is not valid JSON", data.display()))?;
    render::normalize_nfc(&mut json);

    let main = format!("/{name}/main.typ");
    let images = HashMap::new();
    let start = std::time::Instant::now();
    let result = render::render(
        &shared,
        &RenderInput {
            project: &project,
            main: &main,
            data: &json,
            images: &images,
            now: chrono::Utc::now(),
        },
    );
    match result {
        Ok(rendered) => {
            for w in &rendered.warnings {
                eprintln!("warning: {}", serde_json::to_string(w)?);
            }
            std::fs::write(output, &rendered.pdf)
                .with_context(|| format!("cannot write {}", output.display()))?;
            eprintln!(
                "wrote {} ({} bytes) in {} ms",
                output.display(),
                rendered.pdf.len(),
                start.elapsed().as_millis()
            );
            Ok(())
        }
        Err(err) => {
            for d in err.diagnostics() {
                eprintln!("error: {}", serde_json::to_string(d)?);
            }
            bail!("{err}")
        }
    }
}
