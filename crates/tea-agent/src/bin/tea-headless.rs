//! Headless OpenRouter probe using the same agent assembly as `tea`.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tea_agent::build_host_agent;
use tea_core::provider::openrouter::{OpenRouterConfig, OpenRouterProvider};
use tea_core::state::{Message, ModelDescriptor};
use tea_core::DefaultCodingTools;

struct Args {
    model: String,
    prompt: String,
    cwd: PathBuf,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut model = None;
        let mut prompt = None;
        let mut cwd = None;
        let mut arguments = env::args_os().skip(1);
        while let Some(flag) = arguments.next() {
            let flag = flag.to_string_lossy().into_owned();
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--model" => set_once(&mut model, value, "--model")?,
                "--prompt" => set_once(&mut prompt, value, "--prompt")?,
                "--cwd" => set_once(&mut cwd, value, "--cwd")?,
                _ => return Err(format!("unknown option {flag}")),
            }
        }
        let model = model
            .ok_or_else(|| "missing required option --model".to_owned())?
            .into_string()
            .map_err(|_| "--model must be valid UTF-8".to_owned())?;
        let prompt = prompt
            .ok_or_else(|| "missing required option --prompt".to_owned())?
            .into_string()
            .map_err(|_| "--prompt must be valid UTF-8".to_owned())?;
        if model.trim().is_empty() {
            return Err("--model must not be empty".into());
        }
        if prompt.trim().is_empty() {
            return Err("--prompt must not be empty".into());
        }
        let cwd = cwd
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().map_err(|error| error.to_string())?);
        Ok(Self { model, prompt, cwd })
    }
}

fn set_once(
    destination: &mut Option<std::ffi::OsString>,
    value: std::ffi::OsString,
    flag: &str,
) -> Result<(), String> {
    if destination.replace(value).is_some() {
        Err(format!("duplicate option {flag}"))
    } else {
        Ok(())
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("tea-headless: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    let api_key = env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY must be supplied by the caller".to_owned())?;
    let config = OpenRouterConfig::try_new(api_key, args.model.clone())
        .map_err(|error| error.to_string())?;
    let provider = Arc::new(OpenRouterProvider::new(config));
    let tools =
        DefaultCodingTools::new(&args.cwd).map_err(|error| format!("invalid --cwd: {error}"))?;
    let agent = build_host_agent(tools)
        .map_err(|error| error.to_string())?
        .model(ModelDescriptor {
            provider: "openrouter".into(),
            model: args.model,
            revision: None,
        })
        .model_provider(provider)
        .build();
    let run = agent
        .start_prompt(args.prompt)
        .map_err(|error| format!("could not start prompt: {error}"))?;
    smol::block_on(run.drive()).map_err(|error| error.to_string())?;
    let text = agent
        .snapshot()
        .messages
        .into_iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .unwrap_or_default();
    println!("{text}");
    Ok(())
}
