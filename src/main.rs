use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use rustypdf::config::Config;
use rustypdf::fonts::FontStore;
use rustypdf::images::{self, fetch::Fetcher};
use rustypdf::registry;
use rustypdf::render::{self, RenderInput};
use rustypdf::world::Shared;

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
        /// Dev only: send image requests for HOST to ADDR (e.g. a local mock
        /// image server), skipping the DNS SSRF check for that host only.
        #[arg(long = "resolve", value_name = "HOST=ADDR")]
        resolve: Vec<String>,
    },
    /// Exit 0 if `GET /healthz` on the local service answers 200 (for
    /// container health checks; the runtime image has no curl).
    Healthcheck,
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
            resolve,
        } => render_cli(&config, &template_dir, &data, &output, &resolve),
        Command::Healthcheck => healthcheck(config.port),
    }
}

fn healthcheck(port: u16) -> Result<()> {
    use std::io::{Read, Write};
    let timeout = std::time::Duration::from_secs(2);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut head = [0u8; 12];
    stream.read_exact(&mut head)?;
    if head.starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        bail!("unhealthy: {}", String::from_utf8_lossy(&head))
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
    resolve: &[String],
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
    let template = Arc::new(registry::load_one(root, &name, &shared)?);
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data).with_context(|| format!("cannot read {}", data.display()))?,
    )
    .with_context(|| format!("{} is not valid JSON", data.display()))?;

    // The same steps as the service: validate, normalize, resolve images.
    if let Err(errors) = template.validate(&json) {
        for e in &errors {
            eprintln!("invalid input at {:?}: {}", e.path, e.msg);
        }
        bail!("{} does not match {name}/schema.json", data.display());
    }
    let mut json = json;
    render::normalize_nfc(&mut json);
    let limits = images::Limits {
        max_per_request: config.image_max_per_request,
        fetch_concurrency: config.image_fetch_concurrency,
        max_bytes: config.image_max_bytes,
    };
    let overrides = resolve
        .iter()
        .map(|r| {
            let (host, addr) = r
                .split_once('=')
                .with_context(|| format!("--resolve {r:?} must be HOST=ADDR"))?;
            let addr: std::net::SocketAddr = addr
                .parse()
                .with_context(|| format!("--resolve {r:?}: {addr:?} is not IP:PORT"))?;
            Ok((host, addr))
        })
        .collect::<Result<Vec<_>>>()?;
    let fetcher = Fetcher::with_dns_overrides(config, &overrides);
    let runtime = tokio::runtime::Runtime::new()?;
    let (json, resolved) = runtime
        .block_on(images::resolve(template.clone(), json, &fetcher, limits))
        .map_err(|errors| {
            for e in &errors {
                eprintln!(
                    "image error: {}",
                    serde_json::to_string(e).unwrap_or_default()
                );
            }
            anyhow::anyhow!("images could not be loaded")
        })?;
    for w in &resolved.warnings {
        eprintln!(
            "image warning (fallback used): {}",
            serde_json::to_string(w)?
        );
    }

    let start = std::time::Instant::now();
    let result = render::render(
        &shared,
        &RenderInput {
            project: &template.project,
            main: &template.main,
            data: &json,
            images: &resolved.images,
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
